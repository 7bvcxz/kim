//! Shared helpers for integration tests: a loopback HTTP server and package builders.
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

// --- tiny HTTP server ------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct Server {
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Pause between 16 KiB chunks (for cancellation tests).
    pub slow: Arc<Mutex<Option<std::time::Duration>>>,
    pub port: u16,
}

impl Server {
    pub fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Server {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let state = s.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = state.clone();
                std::thread::spawn(move || state.serve(stream));
            }
        });
        s
    }

    fn serve(&self, mut s: std::net::TcpStream) {
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
        let body = self.files.lock().unwrap().get(&path).cloned();
        let Some(body) = body else {
            let _ = s.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        };
        let _ = write!(
            s,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let slow = *self.slow.lock().unwrap();
        for chunk in body.chunks(16 * 1024) {
            if s.write_all(chunk).is_err() {
                return;
            }
            if let Some(d) = slow {
                std::thread::sleep(d);
            }
        }
    }

    pub fn put(&self, path: &str, body: Vec<u8>) -> String {
        self.files.lock().unwrap().insert(path.into(), body);
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

/// A zip with `tool.exe` (a copy of kim.exe, so `--version` works) and extra files.
pub fn package(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("tool.exe", opts).unwrap();
    zip.write_all(&std::fs::read(env!("CARGO_BIN_EXE_kim")).unwrap())
        .unwrap();
    for (name, data) in extra {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

pub fn sha(b: &[u8]) -> String {
    kim::catalog::signature::hex(&Sha256::digest(b))
}

pub fn release(version: &str, url: &str, body: &[u8], entrypoint: &str) -> serde_json::Value {
    serde_json::json!({
        "version": version, "channel": "stable", "published_at": "2026-11-01T00:00:00Z",
        "release_notes": format!("notes {version}"),
        "assets": [{
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "url": url, "size": body.len(), "sha256": sha(body), "kind": "zip",
            "entrypoint": entrypoint
        }]
    })
}
