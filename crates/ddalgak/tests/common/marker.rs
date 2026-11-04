//! The only program Ddalgak's automated tests ever start (Step 70): a tiny console program built
//! by the test itself with the toolchain's `rustc`. It writes what it received — its process ID,
//! working folder, `DDALGAK_TEST_VAR` and arguments — into the marker file named by its first
//! argument (atomically: a temporary file, then a rename) and exits at once. It opens no window;
//! the tests start it hidden (`CREATE_NO_WINDOW`, `SW_HIDE`) and kill it if it is still running.

use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = r#"
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(marker) = args.first() else { std::process::exit(2) };
    let mut out = format!("pid={}\n", std::process::id());
    let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    out.push_str(&format!("cwd={cwd}\n"));
    out.push_str(&format!("env={}\n", std::env::var("DDALGAK_TEST_VAR").unwrap_or_default()));
    for a in &args[1..] {
        out.push_str(&format!("arg={a}\n"));
    }
    let tmp = format!("{marker}.tmp");
    std::fs::write(&tmp, out).expect("write the marker");
    std::fs::rename(&tmp, marker).expect("rename the marker");
}
"#;

/// Builds the marker program into `dir` (ASCII path) and returns it.
pub fn build(dir: &Path) -> PathBuf {
    use std::os::windows::process::CommandExt;
    std::fs::create_dir_all(dir).unwrap();
    let src = dir.join("launch_marker.rs");
    std::fs::write(&src, SOURCE).unwrap();
    let exe = dir.join("launch_marker.exe");
    let cargo = Path::new(env!("CARGO"));
    let sibling = cargo.with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    let rustc = if sibling.exists() {
        sibling
    } else {
        PathBuf::from("rustc")
    };
    let out = Command::new(&rustc)
        .args(["--edition", "2021", "-C", "debuginfo=0", "-o"])
        .arg(&exe)
        .arg(&src)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "building the marker program with {} failed: {}",
        rustc.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    exe
}

/// What the marker program wrote.
#[derive(Debug, Default)]
pub struct Marker {
    pub pid: u32,
    pub cwd: String,
    pub env: String,
    pub args: Vec<String>,
}

/// Waits up to `limit` for the marker file and parses it.
pub fn wait(file: &Path, limit: std::time::Duration) -> Option<Marker> {
    let until = std::time::Instant::now() + limit;
    loop {
        if let Ok(text) = std::fs::read_to_string(file) {
            let mut m = Marker::default();
            for line in text.lines() {
                match line.split_once('=') {
                    Some(("pid", v)) => m.pid = v.parse().unwrap_or(0),
                    Some(("cwd", v)) => m.cwd = v.to_string(),
                    Some(("env", v)) => m.env = v.to_string(),
                    Some(("arg", v)) => m.args.push(v.to_string()),
                    _ => {}
                }
            }
            return Some(m);
        }
        if std::time::Instant::now() > until {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Kills process `pid` only if it still runs `exe` (a reused process ID is left alone).
pub fn kill_if_running(pid: u32, exe: &Path) {
    let want = std::fs::canonicalize(exe).ok();
    for _ in 0..40 {
        match ddalgak::win::process_image(pid) {
            Ok(image) if std::fs::canonicalize(&image).ok() == want => {}
            _ => return, // exited (or the ID belongs to another program now)
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Ok(image) = ddalgak::win::process_image(pid)
        && std::fs::canonicalize(&image).ok() == want
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status();
    }
}
