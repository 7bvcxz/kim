//! E2E — Step 55: Noeul's clipboard paste and rich source formats
//! (docs/adr/0047-noeul-clipboard-paste-and-rich-sources.md).
//! Requirements: NOEUL-FR-070, NOEUL-FR-071, NOEUL-FR-072, NOEUL-FR-073, NOEUL-FR-074,
//! NOEUL-SEC-002, NOEUL-SEC-008, NOEUL-SEC-009, NOEUL-NFR-008.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the paste
//! commands headlessly — `noeul pin <file>` and `--clipboard --as <kind>` in `--help`, the
//! `pin.copy-text` key in `hotkeys list`, the `[paste]` settings in `config show` — and the
//! commands fail honestly: no instance, a network path, an unknown kind. Nothing is started.
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the user pastes
//! a PNG, HTML naming remote images and styles (a listener on 127.0.0.1 proves that nothing is
//! requested), the same clipboard as plain text, a colour value, files copied in Explorer (two
//! images and a document), and pins a file by name; private content, a decompression bomb, a
//! malformed file and an empty clipboard pin nothing and say why. Under the hook a paste reads
//! the in-memory stand-in filled here (`NOEUL_DEV_CLIPBOARD_DIR`), **never the user's
//! clipboard**; every pin lies on a synthetic desktop far off-screen and is never topmost or
//! focused.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no input synthesized,
//! no hotkey registered (both global actions are disabled in the profile before anything
//! starts), no clipboard touched. Every process started here is ended.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn schema(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/contracts/schemas")
        .join(format!("{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn valid(name: &str, v: &Value) {
    let (file, def) = name
        .split_once('#')
        .map_or((name, None), |(f, d)| (f, Some(d)));
    let mut s = schema(file);
    if let Some(d) = def {
        s = json!({"$ref": format!("#/$defs/{d}"), "$defs": s["$defs"].clone()});
    }
    let errors = jsonschema::Validator::new(&s).validate(v).unwrap();
    assert!(errors.is_empty(), "violates {name}: {errors:?}\n{v:#}");
}

struct Noeul {
    sb: TempDir,
    exe: PathBuf,
    endpoint: String,
    synthetic: bool,
}

impl Noeul {
    fn new(tag: &str, exe: PathBuf, synthetic: bool) -> Noeul {
        let sb = TempDir::new(tag);
        let cfg = sb.path().join("Roaming").join("mouv").join("noeul");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(sb.path().join("Local")).unwrap();
        std::fs::create_dir_all(sb.path().join("Temp")).unwrap();
        std::fs::create_dir_all(sb.path().join("clipboard")).unwrap();
        std::fs::write(
            cfg.join("hotkeys.json"),
            serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "overrides": [
                    {"action": "capture.region", "disabled": true},
                    {"action": "paste.float", "disabled": true}]}))
            .unwrap(),
        )
        .unwrap();
        Noeul {
            endpoint: format!("noeul-e2e-{tag}-{}", std::process::id()),
            sb,
            exe,
            synthetic,
        }
    }

    fn raw(&self, args: &[&str]) -> std::process::Output {
        let p = self.sb.path();
        let mut c = Command::new(&self.exe);
        c.args(args)
            .args(["--json", "--endpoint", &self.endpoint])
            .env("APPDATA", p.join("Roaming"))
            .env("LOCALAPPDATA", p.join("Local"))
            .env("TEMP", p.join("Temp"))
            .env("TMP", p.join("Temp"))
            .env("KIM_NO_INPUT", "1")
            .env("NOEUL_DEV_CLIPBOARD_DIR", p.join("clipboard"))
            .env_remove("NOEUL_DEV_CLIPBOARD")
            .env_remove("KIM_NO_HOTKEYS")
            .stdin(Stdio::null());
        if self.synthetic {
            c.env("NOEUL_DEV_SYNTHETIC_DESKTOP", "1");
        } else {
            c.env_remove("NOEUL_DEV_SYNTHETIC_DESKTOP");
        }
        c.output().unwrap()
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let out = self.raw(args);
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        valid("cli-envelope-v1", &doc);
        let code = out.status.code().unwrap_or(-1);
        let want = if doc["ok"] == true {
            0
        } else {
            doc["error"]["exit_code"].as_i64().unwrap()
        };
        assert_eq!(i64::from(code), want, "{args:?}: {doc:#}");
        (code, doc)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, doc) = self.json(args);
        assert_eq!(code, 0, "{args:?}: {doc:#}");
        doc["data"].clone()
    }

    fn fails(&self, args: &[&str], code: i32, error: &str) -> Value {
        let (c, doc) = self.json(args);
        assert_eq!(
            (c, doc["error"]["code"].as_str()),
            (code, Some(error)),
            "{args:?}: {doc:#}"
        );
        doc
    }

    /// Replaces the stand-in clipboard's formats.
    fn clipboard(&self, formats: &[(&str, &[u8])]) {
        let dir = self.sb.path().join("clipboard");
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            std::fs::remove_file(e.path()).unwrap();
        }
        for (name, bytes) in formats {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
    }

    fn paste(&self, extra: &[&str]) -> Value {
        let mut args = vec!["pin", "--clipboard", "--at", "-19990,-19990"];
        args.extend_from_slice(extra);
        let r = self.ok(&args);
        valid("noeul-ipc-v1#pin_create_result", &r);
        r
    }
}

impl Drop for Noeul {
    fn drop(&mut self) {
        let out = self.raw(&["status"]);
        let pid = serde_json::from_slice::<Value>(&out.stdout)
            .ok()
            .and_then(|v| v["data"]["instance"]["pid"].as_u64());
        let _ = self.raw(&["quit", "--yes"]);
        std::thread::sleep(Duration::from_millis(500));
        if let Some(pid) = pid {
            let _ = Command::new("taskkill")
                .args([
                    "/F",
                    "/FI",
                    &format!("PID eq {pid}"),
                    "/FI",
                    "IMAGENAME eq noeul.exe",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// A `w` × `h` RGBA PNG.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        let px: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i * 7) as u8, 90, 200, 255])
            .collect();
        wr.write_image_data(&px).unwrap();
    }
    out
}

/// A 24-bit BMP file of one colour.
fn bmp(w: u32, h: u32) -> Vec<u8> {
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut v = b"BM".to_vec();
    v.extend_from_slice(&size.to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&54u32.to_le_bytes());
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&(w as i32).to_le_bytes());
    v.extend_from_slice(&(h as i32).to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&24u16.to_le_bytes());
    v.extend_from_slice(&[0; 24]);
    for _ in 0..h {
        for _ in 0..w {
            v.extend_from_slice(&[30, 120, 220]);
        }
        v.extend(std::iter::repeat_n(0u8, (row - w * 3) as usize));
    }
    v
}

fn cf_html(fragment: &str) -> String {
    format!(
        "Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:-1\r\nEndFragment:-1\r\n<html><body><!--StartFragment-->{fragment}<!--EndFragment--></body></html>"
    )
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_paste_commands_are_discoverable_headlessly() {
    let n = Noeul::new("s55a", bin("noeul"), false);
    let help = String::from_utf8_lossy(&n.raw(&["pin", "--help"]).stdout).to_string();
    for word in ["FILE", "--clipboard", "--as", "--at", "--dry-run"] {
        assert!(help.contains(word), "pin --help mentions {word}:\n{help}");
    }
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    let copy_text = list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "pin.copy-text")
        .expect("pin.copy-text is listed");
    assert_eq!(copy_text["bindings"][0]["keys"], "Ctrl+Shift+C");
    let shown = n.ok(&["config", "show"]);
    assert_eq!(
        shown["settings"]["paste"],
        json!({"paths": "on", "file_cards": "off"})
    );
    n.fails(&["pin", "C:\\shots\\a.png"], 3, "noeul_not_running");
    n.fails(&["pin", "--clipboard"], 3, "noeul_not_running");
    n.fails(&["pin", "\\\\server\\share\\a.png"], 2, "usage");
    n.fails(&["pin", "--clipboard", "--as", "pdf"], 2, "usage");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_the_user_pastes_every_source_off_screen_without_touching_the_clipboard() {
    let n = Noeul::new("s55b", bin_debug("noeul"), true);
    let (code, start) = n.json(&["start", "--no-tray"]);
    assert_eq!(code, 0, "{start:#}");
    let st = n.ok(&["status"]);
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the debug test hook is on"
    );
    assert_eq!(
        st["hotkeys"]["registrations"],
        json!([]),
        "nothing registered"
    );
    // A picture.
    n.clipboard(&[("PNG", &png(32, 20))]);
    let r = n.paste(&[]);
    assert_eq!(
        (r["kind"].as_str(), r["pin"]["rect"]["width"].as_u64()),
        (Some("image"), Some(32))
    );
    // HTML naming remote resources on a local listener: nothing is requested.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let html = cf_html(&format!(
        "<h1>Notes</h1><p>See <img src=\"http://127.0.0.1:{port}/a.png\" alt=\"chart\"> and \
         <span style=\"background:url(http://127.0.0.1:{port}/b.png)\">this</span>.</p>\
         <link rel=stylesheet href=\"http://127.0.0.1:{port}/c.css\"><script src=\"http://127.0.0.1:{port}/d.js\"></script>"
    ));
    n.clipboard(&[
        ("HTML Format", html.as_bytes()),
        ("CF_UNICODETEXT", b"Notes\nSee chart and this."),
    ]);
    let r = n.paste(&[]);
    assert_eq!(r["kind"], "html");
    assert_eq!(r["offered"], json!(["html", "text"]));
    let r = n.paste(&["--as", "text"]);
    assert_eq!(r["kind"], "text");
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        if let Ok((_, from)) = listener.accept() {
            panic!("Noeul made a request from {from}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // A colour value.
    n.clipboard(&[("CF_UNICODETEXT", b"rgb(255, 136, 0)")]);
    assert_eq!(n.paste(&[])["kind"], "color");
    // Files copied in Explorer: two images and a document.
    let dir = n.sb.path().join("files");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("one.png"), png(12, 12)).unwrap();
    std::fs::write(dir.join("two.bmp"), bmp(9, 7)).unwrap();
    std::fs::write(dir.join("notes.txt"), "text").unwrap();
    let drop = ["one.png", "two.bmp", "notes.txt"]
        .map(|f| dir.join(f).display().to_string())
        .join("\r\n");
    n.clipboard(&[("CF_HDROP", drop.as_bytes())]);
    let r = n.paste(&[]);
    assert_eq!(r["kind"], "image_file");
    assert_eq!(r["pins"].as_array().unwrap().len(), 2, "{r:#}");
    assert_eq!(r["skipped"], 1);
    assert!(!r.to_string().contains("one.png"), "no path is reported");
    // A file by name.
    let r = n.ok(&[
        "pin",
        &dir.join("two.bmp").display().to_string(),
        "--at",
        "-19990,-19990",
    ]);
    valid("noeul-ipc-v1#pin_create_result", &r);
    assert_eq!(r["pin"]["source"], "file");
    let list = n.ok(&["pin", "list"]);
    valid("noeul-ipc-v1#pin_list_result", &list);
    let kinds: Vec<&str> = list["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["image", "html", "text", "color", "image", "image", "image"],
        "{list:#}"
    );
    // What pins nothing says why.
    let pin_clip = ["pin", "--clipboard", "--at", "-19990,-19990"];
    n.clipboard(&[
        ("CF_UNICODETEXT", b"a password"),
        ("ExcludeClipboardContentFromMonitorProcessing", b""),
    ]);
    let doc = n.fails(&pin_clip, 3, "nothing_to_pin");
    assert!(!doc.to_string().contains("a password"));
    let mut bomb = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bomb.extend_from_slice(&40_000u32.to_be_bytes());
    bomb.extend_from_slice(&40_000u32.to_be_bytes());
    bomb.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    n.clipboard(&[("PNG", &bomb)]);
    n.fails(&pin_clip, 4, "image_invalid");
    std::fs::write(dir.join("cut.png"), &png(30, 30)[..50]).unwrap();
    n.fails(
        &[
            "pin",
            &dir.join("cut.png").display().to_string(),
            "--at",
            "-19990,-19990",
        ],
        4,
        "image_invalid",
    );
    n.clipboard(&[]);
    n.fails(&pin_clip, 3, "nothing_to_pin");
    let st = n.ok(&["status"]);
    assert_eq!(st["pins"]["open"], 7);
    n.ok(&["quit", "--yes"]);
}
