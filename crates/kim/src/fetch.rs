//! Retrieval of remote documents. Used only by explicit user commands (`kim catalog refresh`);
//! nothing in Kim fetches in the background (KIM-NFR-004).
//!
//! Allowed sources: `https://…`, `http://` to a loopback host (local mirrors and tests), and
//! `file://` paths (offline mirrors). Everything fetched is size-limited; integrity comes from
//! signatures, not from the transport.

use crate::error::{ErrorCode, KimError, Result};
use std::io::Read;
use std::time::Duration;

/// Something that can fetch bytes from a source URL.
pub trait Fetcher {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>>;
}

/// Validates a source URL against the allowed schemes and hosts.
pub fn check_source_url(url: &str) -> Result<()> {
    if url.starts_with("https://") && url.len() > "https://".len() {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
        let hostname = if host.starts_with('[') {
            host.split(']')
                .next()
                .map(|h| format!("{h}]"))
                .unwrap_or_default()
        } else {
            host.split(':').next().unwrap_or("").to_string()
        };
        if matches!(hostname.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(());
        }
        return Err(KimError::new(
            ErrorCode::UnsupportedSource,
            format!("refusing plain http:// to a non-loopback host: {url}"),
        )
        .with_hint("use an https:// URL"));
    }
    if url.starts_with("file://") {
        return Ok(());
    }
    Err(KimError::new(
        ErrorCode::UnsupportedSource,
        format!("unsupported source URL `{url}` (use https://, http://localhost or file://)"),
    ))
}

/// Converts a `file://` URL to a path (`file:///C:/x`, `file://C:/x` and `file:///home/x`).
pub fn file_url_to_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    // `/C:/...` → `C:/...` on Windows-style URLs.
    let rest = if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
        &rest[1..]
    } else {
        rest
    };
    (!rest.is_empty()).then(|| std::path::PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Real fetcher: file:// via the file system, http(s) via ureq with the OS TLS stack
/// (Windows: SChannel and the Windows certificate store).
#[derive(Debug, Clone)]
pub struct DefaultFetcher {
    pub timeout: Duration,
}

impl Default for DefaultFetcher {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl DefaultFetcher {
    /// Agent for small documents: one global deadline for the whole request.
    fn agent(&self) -> ureq::Agent {
        self.agent_with(Some(self.timeout))
    }

    /// Agent for package downloads: bounded connect/response wait, but no deadline on the body
    /// (large packages on slow links); the user can cancel at any time.
    fn download_agent(&self) -> ureq::Agent {
        self.agent_with(None)
    }

    fn agent_with(&self, global: Option<Duration>) -> ureq::Agent {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(global)
            .timeout_connect(Some(self.timeout))
            .timeout_recv_response(Some(self.timeout))
            .max_redirects(5)
            .user_agent(format!("kim/{}", crate::KIM_VERSION))
            .build()
            .new_agent()
    }
}

impl Fetcher for DefaultFetcher {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        check_source_url(url)?;
        if url.starts_with("file://") {
            let path = file_url_to_path(url).ok_or_else(|| {
                KimError::new(
                    ErrorCode::UnsupportedSource,
                    format!("bad file URL `{url}`"),
                )
            })?;
            let file = std::fs::File::open(&path).map_err(|e| {
                KimError::new(
                    ErrorCode::NetworkUnavailable,
                    format!("cannot open {}: {e}", path.display()),
                )
            })?;
            let mut buf = Vec::new();
            file.take(limit + 1).read_to_end(&mut buf).map_err(|e| {
                KimError::new(
                    ErrorCode::NetworkUnavailable,
                    format!("cannot read {}: {e}", path.display()),
                )
            })?;
            return check_limit(buf, limit, url);
        }
        let mut response = self.agent().get(url).call().map_err(|e| match e {
            ureq::Error::StatusCode(code) => KimError::new(
                ErrorCode::NetworkUnavailable,
                format!("the server answered HTTP {code} for {url}"),
            )
            .with_hint(
                "the source URL may be wrong or not published yet; \
                 the cached or bundled catalog stays in use",
            ),
            other => KimError::new(
                ErrorCode::NetworkUnavailable,
                format!("cannot fetch {url}: {other}"),
            )
            .with_hint("check the network connection; the cached or bundled catalog stays in use"),
        })?;
        let buf = response
            .body_mut()
            .with_config()
            .limit(limit + 1)
            .read_to_vec()
            .map_err(|e| {
                KimError::new(
                    ErrorCode::NetworkUnavailable,
                    format!("cannot read {url}: {e}"),
                )
            })?;
        check_limit(buf, limit, url)
    }
}

/// Streaming download of a package with progress and cancellation (installer, Step 06).
pub trait Downloader {
    /// Writes `url` to `dest`. Fails with `integrity_failed` as soon as more than `expected`
    /// bytes arrive, and with `cancelled` when `cancel` becomes true.
    fn download(
        &self,
        url: &str,
        dest: &std::path::Path,
        expected: u64,
        on_progress: &mut dyn FnMut(u64),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<()>;
}

impl Downloader for DefaultFetcher {
    fn download(
        &self,
        url: &str,
        dest: &std::path::Path,
        expected: u64,
        on_progress: &mut dyn FnMut(u64),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<()> {
        check_source_url(url)?;
        let reader: Box<dyn Read> = if url.starts_with("file://") {
            let path = file_url_to_path(url).ok_or_else(|| {
                KimError::new(
                    ErrorCode::UnsupportedSource,
                    format!("bad file URL `{url}`"),
                )
            })?;
            Box::new(std::fs::File::open(&path).map_err(|e| {
                KimError::new(
                    ErrorCode::NetworkUnavailable,
                    format!("cannot open {}: {e}", path.display()),
                )
            })?)
        } else {
            let response = self
                .download_agent()
                .get(url)
                .call()
                .map_err(|e| map_http_error(url, e))?;
            Box::new(response.into_body().into_reader())
        };
        copy_with_progress(reader, dest, expected, on_progress, cancel, url)
    }
}

pub(crate) fn copy_with_progress(
    mut reader: Box<dyn Read>,
    dest: &std::path::Path,
    expected: u64,
    on_progress: &mut dyn FnMut(u64),
    cancel: &std::sync::atomic::AtomicBool,
    url: &str,
) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    let io = |e: std::io::Error| KimError::new(ErrorCode::Io, format!("{}: {e}", dest.display()));
    let mut out = std::fs::File::create(dest).map_err(io)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(KimError::new(ErrorCode::Cancelled, "download cancelled"));
        }
        let n = reader.read(&mut buf).map_err(|e| {
            KimError::new(
                ErrorCode::NetworkUnavailable,
                format!("download of {url} failed: {e}"),
            )
        })?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > expected {
            return Err(KimError::new(
                ErrorCode::IntegrityFailed,
                format!("{url} is larger than the {expected} bytes the signed catalog promises"),
            ));
        }
        out.write_all(&buf[..n]).map_err(io)?;
        on_progress(done);
    }
    out.sync_all().map_err(io)?;
    Ok(())
}

fn map_http_error(url: &str, e: ureq::Error) -> KimError {
    match e {
        ureq::Error::StatusCode(code) => KimError::new(
            ErrorCode::NetworkUnavailable,
            format!("the server answered HTTP {code} for {url}"),
        )
        .with_hint("the source URL may be wrong or not published yet; nothing was changed"),
        other => KimError::new(
            ErrorCode::NetworkUnavailable,
            format!("cannot fetch {url}: {other}"),
        )
        .with_hint("check the network connection; nothing was changed"),
    }
}

fn check_limit(buf: Vec<u8>, limit: u64, url: &str) -> Result<Vec<u8>> {
    if buf.len() as u64 > limit {
        Err(KimError::new(
            ErrorCode::CatalogInvalid,
            format!("{url} is larger than the {limit}-byte limit"),
        ))
    } else {
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_url_policy() {
        for ok in [
            "https://raw.githubusercontent.com/7bvcxz/kim/main/catalog/catalog.json",
            "http://127.0.0.1:8080/c.json",
            "http://localhost/c.json",
            "http://[::1]:9/c.json",
            "file:///C:/mirror/catalog.json",
        ] {
            assert!(check_source_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://example.com/c.json",
            "http://127.0.0.1.evil.com/c.json",
            "http://localhost@evil.com/c.json",
            "ftp://x/c.json",
            "https://",
            "C:\\catalog.json",
        ] {
            assert_eq!(
                check_source_url(bad).unwrap_err().code,
                ErrorCode::UnsupportedSource,
                "{bad}"
            );
        }
    }

    #[test]
    fn file_urls_map_to_paths() {
        assert_eq!(
            file_url_to_path("file:///C:/a%20b/c.json").unwrap(),
            std::path::PathBuf::from("C:/a b/c.json")
        );
        assert_eq!(
            file_url_to_path("file:///home/me/c.json").unwrap(),
            std::path::PathBuf::from("/home/me/c.json")
        );
        assert!(file_url_to_path("file://").is_none());
    }

    #[test]
    fn file_fetch_respects_limit_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("kim-fetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.json");
        std::fs::write(&f, b"0123456789").unwrap();
        let url = format!("file:///{}", f.display().to_string().replace('\\', "/"));
        let fetcher = DefaultFetcher::default();
        assert_eq!(fetcher.get(&url, 10).unwrap(), b"0123456789");
        assert_eq!(
            fetcher.get(&url, 9).unwrap_err().code,
            ErrorCode::CatalogInvalid
        );
        let missing = format!(
            "file:///{}",
            dir.join("nope.json")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        assert_eq!(
            fetcher.get(&missing, 10).unwrap_err().code,
            ErrorCode::NetworkUnavailable
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn http_fetch_against_a_local_server() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = s.read(&mut req).unwrap();
            let body = b"{\"ok\":1}";
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            s.write_all(body).unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        let got = DefaultFetcher::default()
            .get(&format!("http://127.0.0.1:{port}/catalog.json"), 1024)
            .unwrap();
        assert_eq!(got, b"{\"ok\":1}");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /catalog.json HTTP/1.1"),
            "{request}"
        );
        assert!(
            request.to_ascii_lowercase().contains("user-agent: kim/"),
            "{request}"
        );
    }

    #[test]
    fn http_error_statuses_are_reported_as_such() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = s.read(&mut req).unwrap();
            s.write_all(
                b"HTTP/1.1 404 Not Found
Content-Length: 0
Connection: close

",
            )
            .unwrap();
        });
        let e = DefaultFetcher::default()
            .get(&format!("http://127.0.0.1:{port}/missing.json"), 1024)
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(e.code, ErrorCode::NetworkUnavailable);
        assert!(e.message.contains("HTTP 404"), "{}", e.message);
        assert!(e.hint.unwrap().contains("not published yet"));
    }

    #[test]
    fn unreachable_hosts_map_to_network_unavailable() {
        // Port 9 (discard) on loopback is closed on CI and developer machines.
        let e = DefaultFetcher {
            timeout: Duration::from_secs(5),
        }
        .get("http://127.0.0.1:9/catalog.json", 1024)
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::NetworkUnavailable);
    }
}
