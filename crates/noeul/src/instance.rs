//! CLI commands that concern the resident instance: `status`, `health`, `start`, `run`, `quit`,
//! `capture`, `pin …` (the pin engine's commands since Step 53, ADR 0029), and the instance
//! queries of `hotkeys …` (ADR 0023 §4).
//!
//! Everything here talks to the instance over the identity-checked pipe (`noeul::win::client`)
//! and never starts it implicitly: only `start`, `run` and `capture --start` do. `status` and
//! `health` are headless reads; when no instance runs, `status` reports what it can from files
//! and the desktop check alone.

#[cfg(windows)]
use crate::confirm;
use crate::{Ctx, Outcome, QuitArgs, StartArgs};
#[cfg(not(windows))]
use noeul::capture::DesktopState;
use noeul::error::{Code, NoeulError};
use noeul::frame::Rect;
use noeul::paths::Paths;
use noeul::{hotkeys, settings};
use serde_json::{Value, json};

#[cfg(windows)]
use noeul::win::{client, pipe};
#[cfg(windows)]
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, NoeulError>;

#[cfg(windows)]
fn endpoint(ctx: &Ctx) -> Result<String> {
    match &ctx.endpoint {
        Some(e) if !e.is_empty() && !e.contains(['\\', '/']) && e.len() <= 200 => Ok(e.clone()),
        Some(e) => Err(NoeulError::new(
            Code::Usage,
            format!("`{e}` is not a pipe name (no slashes, at most 200 characters)"),
        )),
        None => pipe::default_endpoint().map_err(|e| NoeulError::io("the current user", &e)),
    }
}

/// Connects to the running instance; `Ok(None)` when none runs.
#[cfg(windows)]
fn connect(ctx: &Ctx) -> Result<Option<client::Session>> {
    let ep = endpoint(ctx)?;
    match client::open(&ep) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.code == Code::NoeulNotRunning => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn session(ctx: &Ctx) -> Result<client::Session> {
    connect(ctx)?.ok_or_else(NoeulError::not_running)
}

#[cfg(not(windows))]
fn unsupported() -> NoeulError {
    NoeulError::unsupported_platform("the Noeul instance")
}

fn desktop_json() -> Value {
    noeul::capture::system().desktop().to_json()
}

fn hotkeys_offline(paths: &Paths) -> Value {
    let (state, loaded) = hotkeys::load(paths);
    json!({
        "enabled": state.enabled() && !loaded.state.safe_mode(),
        "config": {"path": loaded.path.display().to_string(), "state": loaded.state.as_str()},
        "problem": hotkeys::safe_mode_warning(&loaded),
        "registrations": [],
    })
}

fn status_human(v: &Value) -> String {
    if v["running"] != true {
        return format!(
            "Noeul is not running (start it with `noeul start`).\nDesktop: {}\nShortcuts file: {} ({})\n",
            v["desktop"]["state"].as_str().unwrap_or("?"),
            v["hotkeys"]["config"]["path"].as_str().unwrap_or(""),
            v["hotkeys"]["config"]["state"].as_str().unwrap_or("")
        );
    }
    let count = |key: &str, word: &str| match v["pins"][key].as_u64() {
        Some(n) if n > 0 => format!(", {n} {word}"),
        _ => String::new(),
    };
    let mut s = format!(
        "Noeul {} is running (pid {}, up {} s).\nDesktop: {}\nPins: {} open{}{}{}\nImage memory: {} bytes in {} picture(s)\n",
        v["instance"]["version"].as_str().unwrap_or("?"),
        v["instance"]["pid"],
        v["instance"]["uptime_s"],
        v["desktop"]["state"].as_str().unwrap_or("?"),
        v["pins"]["open"],
        count("hidden", "hidden"),
        count(
            "off_screen",
            "off-screen (`noeul pin show --all --on-screen`)"
        ),
        v["pins"]["reopenable"]
            .as_str()
            .map_or(String::new(), |p| format!(", {p} can be reopened")),
        v["memory"]["bytes"],
        v["memory"]["frames"],
    );
    let regs = v["hotkeys"]["registrations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(off) = v["hotkeys"]["off_reason"].as_str() {
        s.push_str(&format!("Global hotkeys: off ({off})\n"));
    } else if regs.is_empty() {
        s.push_str("Global hotkeys: none bound\n");
    }
    for r in regs {
        s.push_str(&format!(
            "  {} {}: {}{}\n",
            r["action"].as_str().unwrap_or(""),
            r["keys"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            r["message"]
                .as_str()
                .map_or(String::new(), |m| format!(" — {m}"))
        ));
    }
    for c in v["hotkeys"]["conflicts"].as_array().into_iter().flatten() {
        if c["code"] != "taken" {
            continue;
        }
        let action = c["action"].as_str().unwrap_or("");
        let rebind = c["offers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|o| o["kind"] == "rebind")
            .and_then(|o| o["keys"].as_str());
        let fix = match rebind {
            Some(k) => format!("`noeul hotkeys set {action} {k}`"),
            None => {
                format!("`noeul hotkeys set {action} <keys>` or `noeul hotkeys disable {action}`")
            }
        };
        s.push_str(&format!(
            "  fix: {fix} (a key another program registered is never taken over)\n"
        ));
    }
    s.push_str(&format!(
        "Tray icon: {}\n",
        if v["tray"]["shown"] == true {
            "shown"
        } else {
            "not shown"
        }
    ));
    s
}

/// `noeul status`: never starts anything.
pub fn status(ctx: &Ctx) -> Result<Outcome> {
    #[cfg(windows)]
    if let Some(mut s) = connect(ctx)? {
        let v = s.request("status", json!({}))?;
        let human = status_human(&v);
        return Ok(Outcome::new(v, human));
    }
    #[cfg(not(windows))]
    let _ = ctx;
    let paths = Paths::from_env()?;
    let v = json!({
        "running": false,
        "desktop": desktop_json(),
        "capture_backend": noeul::capture::system().name(),
        "hotkeys": hotkeys_offline(&paths),
        "settings": settings::load(&paths.config_file()).to_json(),
    });
    let human = status_human(&v);
    Ok(Outcome::new(v, human))
}

/// `noeul health`: the heartbeat (identity checks, handshake, one answer).
pub fn health(ctx: &Ctx) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let t = Instant::now();
        let mut s = session(ctx)?;
        let st = s.request("status", json!({}))?;
        let ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
        let v = json!({
            "healthy": true,
            "pid": s.pid,
            "endpoint": pipe::path(&s.endpoint),
            "protocol": s.welcome["protocol"],
            "answer_ms": ms,
            "identity": {"same_user": true, "image": noeul::IMAGE},
            "version": st["instance"]["version"],
        });
        let human = format!(
            "healthy: Noeul pid {} answered in {ms} ms (same user, {})\n",
            s.pid,
            noeul::IMAGE
        );
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Err(unsupported())
    }
}

/// `noeul start`: start detached, or hand over to the running instance (NOEUL-FR-003).
pub fn start(ctx: &Ctx, a: &StartArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let ep = endpoint(ctx)?;
        if let Some(mut s) = connect(ctx)? {
            if a.dry_run {
                return Ok(Outcome::new(
                    json!({"dry_run": true, "started": false, "already_running": true, "pid": s.pid}),
                    format!("Noeul already runs (pid {}); nothing would start.\n", s.pid),
                ));
            }
            let act = s.request("activate", json!({"reason": "start"}))?;
            return Ok(Outcome::new(
                json!({"dry_run": false, "started": false, "already_running": true, "pid": s.pid, "activate": act}),
                format!("Noeul already runs (pid {}).\n", s.pid),
            ));
        }
        if a.dry_run {
            return Ok(Outcome::new(
                json!({"dry_run": true, "started": false, "already_running": false,
                       "endpoint": pipe::path(&ep), "tray": !a.no_tray,
                       "hotkeys": hotkeys::off_reason(a.no_hotkeys).is_none()}),
                "Would start Noeul detached (dry run: nothing was started).\n",
            ));
        }
        let mut args = vec!["--endpoint".to_string(), ep.clone()];
        if a.no_tray {
            args.push("--no-tray".into());
        }
        if a.no_hotkeys {
            args.push("--no-hotkeys".into());
        }
        let child = noeul::win::spawn::instance(&args).map_err(|e| {
            NoeulError::new(Code::NoeulStartFailed, format!("cannot start Noeul: {e}"))
        })?;
        let pid = child.pid;
        // Wait until an instance answers. Ours may have lost a start race: then it handed over
        // to the winner and exited 0, and the winner answers; any other exit is a failure.
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match client::open(&ep) {
                Ok(s) => {
                    let started = s.pid == pid;
                    return Ok(Outcome::new(
                        json!({"dry_run": false, "started": started, "already_running": !started,
                               "pid": s.pid, "endpoint": pipe::path(&ep)}),
                        format!("Noeul is running (pid {}).\n", s.pid),
                    ));
                }
                Err(e) if e.code != Code::NoeulNotRunning => return Err(e),
                Err(_) => {}
            }
            if let Some(status) = child.try_wait().filter(|c| *c != 0) {
                return Err(NoeulError::new(
                    Code::NoeulStartFailed,
                    format!("Noeul exited during start-up (exit code {status})"),
                )
                .hint("run `noeul run --verbose` to see why"));
            }
            if Instant::now() >= until {
                return Err(NoeulError::new(
                    Code::NoeulUnresponsive,
                    format!("Noeul (pid {pid}) did not answer within 10 s of starting"),
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

/// `noeul run`: the instance in this process.
pub fn run(ctx: &Ctx, a: &StartArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let ep = endpoint(ctx)?;
        if a.dry_run {
            return Ok(Outcome::new(
                json!({"dry_run": true, "endpoint": pipe::path(&ep), "tray": !a.no_tray,
                       "hotkeys": hotkeys::off_reason(a.no_hotkeys).is_none()}),
                "Would run Noeul in this process (dry run).\n",
            ));
        }
        let v = noeul::win::runtime::run(&noeul::win::runtime::RunOpts {
            endpoint: ep,
            no_tray: a.no_tray,
            no_hotkeys: a.no_hotkeys,
        })?;
        let human = if v["handed_over"] == true {
            format!("Noeul already runs (pid {}); handed over.\n", v["pid"])
        } else {
            "Noeul ended.\n".to_string()
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

/// `noeul quit`: ends the instance (open pins need `--yes`, FR-002).
pub fn quit(ctx: &Ctx, a: &QuitArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let mut s = session(ctx)?;
        let pid = s.pid;
        let pins = s.request("pin.list", json!({}))?["pins"]
            .as_array()
            .map_or(0, Vec::len);
        let mut force = a.yes;
        if pins > 0 && !force && !a.dry_run {
            match confirm(
                ctx,
                &format!("Quit Noeul and close {pins} pin(s)? Pins are not backed up. [y/N]"),
                "y",
            ) {
                Some(true) => force = true,
                Some(false) => {
                    return Err(NoeulError::new(
                        Code::Cancelled,
                        "quit cancelled; nothing was changed",
                    ));
                }
                None => {}
            }
        }
        let r = s.request(
            "quit",
            json!({"force": force || a.dry_run, "dry_run": a.dry_run}),
        )?;
        drop(s);
        let exited = !a.dry_run && noeul::win::wait_exit(pid, Duration::from_secs(5));
        let mut v = r;
        v["pid"] = json!(pid);
        v["exited"] = json!(exited);
        let human = if a.dry_run {
            format!("Would quit Noeul (pid {pid}) and close {pins} pin(s) (dry run).\n")
        } else {
            format!("Noeul (pid {pid}) quit; its hotkeys were released and {pins} pin(s) closed.\n")
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

/// The running instance, started first when `--start` asks for it (not for a dry run).
#[cfg(windows)]
fn session_or_start(ctx: &Ctx, start_it: bool) -> Result<client::Session> {
    match connect(ctx)? {
        Some(s) => Ok(s),
        None if start_it => {
            start(
                ctx,
                &StartArgs {
                    no_tray: false,
                    no_hotkeys: false,
                    dry_run: false,
                },
            )?;
            session(ctx)
        }
        None => Err(NoeulError::not_running()),
    }
}

/// `noeul capture --interactive`: the instance opens the selection overlay, as `F1` does
/// (NOEUL-FR-010). The overlay is a visible window on the user's desktop; this command returns
/// as soon as it is shown (or says why not).
fn capture_interactive(ctx: &Ctx, a: &crate::CaptureArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let mut s = session_or_start(ctx, a.start && !a.dry_run)?;
        // This console normally has the user's focus: let the instance bring its overlay to
        // the foreground (harmless when this process is in the background).
        client::allow_foreground(s.pid);
        let v = s.request("capture.region", json!({"dry_run": a.dry_run}))?;
        let picture = Rect::from_json(&v["picture"]).map_or_else(String::new, |r| r.to_string());
        let human = if a.dry_run {
            format!(
                "Would open the selection overlay over {picture} ({} monitor(s)) (dry run: nothing was captured).\n",
                v["monitors"]
            )
        } else if v["opened"] == true {
            format!(
                "The selection overlay is open over {picture}: drag a rectangle, Enter pins it, Esc cancels.\n"
            )
        } else {
            "A selection overlay is already open; nothing changed.\n".to_string()
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(DesktopState::Unsupported
            .error()
            .unwrap_or_else(unsupported))
    }
}

/// `noeul capture --cancel`: closes an open overlay without any output.
fn capture_cancel(ctx: &Ctx, dry_run: bool) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let v = session(ctx)?.request("capture.cancel", json!({"dry_run": dry_run}))?;
        let human = match (v["session"].as_u64(), dry_run) {
            (None, _) => "No selection overlay is open.\n".to_string(),
            (Some(id), true) => format!("Would cancel capture session {id} (dry run).\n"),
            (Some(id), false) => format!("Cancelled capture session {id}; nothing was kept.\n"),
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, dry_run);
        Err(unsupported())
    }
}

/// `noeul capture`: `--region …`, `--monitor <n>` or `--window active` with `--pin` (the instance
/// captures and pins; `--pointer` draws the pointer in; monitors, windows and the pointer since
/// Step 58), `--interactive` (the selection overlay) or `--cancel`.
pub fn capture(ctx: &Ctx, a: &crate::CaptureArgs) -> Result<Outcome> {
    if a.cancel {
        return capture_cancel(ctx, a.dry_run);
    }
    if a.interactive {
        return capture_interactive(ctx, a);
    }
    let mut params = json!({"pin": true, "dry_run": a.dry_run, "pointer": a.pointer});
    if let Some(m) = &a.monitor {
        let spec = noeul::displays::MonitorSpec::parse(m).ok_or_else(|| {
            NoeulError::new(
                Code::Usage,
                format!("`{m}` is not a monitor: use a number from 1, `primary` or `pointer`"),
            )
            .hint("`noeul status` lists the monitors in order (1 is the leftmost)")
        })?;
        params["monitor"] = spec.to_json();
    } else if a.window.is_some() {
        params["window"] = json!("active");
    } else {
        let region = Rect::parse(a.region.as_deref().unwrap_or("")).map_err(|e| {
            NoeulError::new(Code::Usage, e)
                .hint("for example: noeul capture --region 0,0,400,300 --pin")
        })?;
        params["region"] = region.to_json();
    }
    if !a.pin {
        return Err(NoeulError::new(
            Code::Usage,
            "nothing to do: add --pin (capture to a file or the clipboard is planned, NOEUL-FR-112)",
        ));
    }
    #[cfg(windows)]
    {
        let mut s = session_or_start(ctx, a.start && !a.dry_run)?;
        let v = s.request("capture", params)?;
        let mut out = Outcome::new(v.clone(), capture_human(&v));
        out.warnings = v["warnings"]
            .as_array()
            .map(|w| {
                w.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(out)
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, params);
        Err(DesktopState::Unsupported
            .error()
            .unwrap_or_else(unsupported))
    }
}

/// One line about a `capture` result: what was taken, its device and logical size, the pin.
#[cfg(windows)]
fn capture_human(v: &Value) -> String {
    let region = Rect::from_json(&v["region"]).map_or_else(String::new, |r| r.to_string());
    let what = match v["target"].as_str() {
        Some("monitor") => format!("monitor {}", v["monitor"]["number"]),
        Some("window") => "the active window".to_string(),
        _ => "the region".to_string(),
    };
    let m = &v["mapping"];
    let scale = if m.is_object() {
        format!(
            " ({}×{} logical at {} % on monitor {})",
            m["logical"]["width"], m["logical"]["height"], m["scale_percent"], m["monitor"]
        )
    } else {
        String::new()
    };
    let pointer = match v["pointer"]["drawn"].as_bool() {
        Some(true) => ", with the pointer",
        _ => "",
    };
    match v["pin"]["id"].as_str() {
        Some(id) => format!("Pinned {what} {region}{scale}{pointer} as {id}.\n"),
        None => format!("Would capture {what} {region}{scale} and pin it (dry run).\n"),
    }
}

pub fn pin_list(ctx: &Ctx) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let v = session(ctx)?.request("pin.list", json!({}))?;
        let mut human = String::new();
        let pins = v["pins"].as_array().cloned().unwrap_or_default();
        if pins.is_empty() {
            human.push_str("No pins are open.\n");
        } else {
            let mut rows = vec![vec![
                "PIN".to_string(),
                "RECT".into(),
                "VIEW".into(),
                "STATE".into(),
                "GROUP".into(),
                "SOURCE".into(),
            ]];
            for p in &pins {
                let r = &p["rect"];
                let mut state = vec![if p["visible"] == false {
                    "hidden"
                } else {
                    "shown"
                }];
                if p["on_screen"] == false {
                    state.push("off-screen");
                }
                rows.push(vec![
                    p["id"].as_str().unwrap_or("").into(),
                    format!("{},{},{},{}", r["x"], r["y"], r["width"], r["height"]),
                    view_words(p),
                    state.join(", "),
                    p["group"].as_str().unwrap_or("").into(),
                    p["source"].as_str().unwrap_or("").into(),
                ]);
            }
            human.push_str(&noeul::util::table(&rows));
        }
        if let Some(r) = v["reopenable"].as_str() {
            human.push_str(&format!("{r} can be reopened (`noeul pin reopen`).\n"));
        }
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Err(unsupported())
    }
}

/// `noeul pin --clipboard [--as <kind>]` and `noeul pin <file>`: the instance reads the clipboard
/// once, or the image file, and pins what it holds, as `F3` does but without focus (NOEUL-SEC-002:
/// an explicit paste by the user at their own unlocked desktop, announced by a notice of the
/// running Noeul, NOEUL-SEC-010). A dry run reads nothing. The path is made absolute here and
/// must be on a local drive; the result never names it (NOEUL-SEC-008).
pub fn pin_create(ctx: &Ctx, a: &crate::PinCreateArgs) -> Result<Outcome> {
    if !a.clipboard && a.file.is_none() {
        return Err(NoeulError::new(
            Code::Usage,
            "nothing to pin: give an image file, add --clipboard, or use a subcommand (list, hide, show, close, reopen)",
        )
        .hint("a screen region is pinned with `noeul capture --region x,y,w,h --pin`"));
    }
    let kind = match &a.as_kind {
        None => None,
        Some(k) => Some(noeul::paste::Kind::parse(k).ok_or_else(|| {
            NoeulError::new(Code::Usage, format!("`{k}` is not a kind to pin"))
                .hint("--as takes image, image_file, color, html, text or files")
        })?),
    };
    let file = match &a.file {
        None => None,
        Some(f) => {
            let abs = if f.is_absolute() {
                f.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| NoeulError::io("the current folder", &e))?
                    .join(f)
            };
            if !noeul::paste::is_local_path(&abs) {
                return Err(NoeulError::new(
                    Code::Usage,
                    "the file must be on a local drive (C:\\…)",
                )
                .hint("network paths (\\\\server\\share) are never opened"));
            }
            Some(abs)
        }
    };
    let at = match &a.at {
        None => None,
        Some(text) => Some(parse_point(text).ok_or_else(|| {
            NoeulError::new(Code::Usage, format!("`{text}` is not a point; use x,y"))
                .hint("for example: noeul pin --clipboard --at 100,200")
        })?),
    };
    #[cfg(windows)]
    {
        let mut s = session_or_start(ctx, a.start && !a.dry_run)?;
        let mut params = match &file {
            Some(p) => {
                json!({"source": "file", "path": p.display().to_string(), "dry_run": a.dry_run})
            }
            None => json!({"source": "clipboard", "dry_run": a.dry_run}),
        };
        if let Some(at) = at {
            params["at"] = at;
        }
        if let Some(k) = kind {
            params["as"] = json!(k.as_str());
        }
        let v = s.request("pin.create", params)?;
        let what = match v["kind"].as_str() {
            Some("image") => "the clipboard image",
            Some("image_file") if file.is_some() => "the image file",
            Some("image_file") => "the copied image file(s)",
            Some("color") => "the colour value as a swatch card",
            Some("html") => "the HTML as a card",
            Some("text") => "the text as a card",
            Some("files") => "the copied file names as a card",
            _ => "the clipboard",
        };
        let count = v["pins"].as_array().map_or(0, Vec::len);
        let human = match (v["pin"]["id"].as_str(), Rect::from_json(&v["pin"]["rect"])) {
            (Some(id), Some(r)) => {
                let more = if count > 1 {
                    format!(" and {} more pin(s)", count - 1)
                } else {
                    String::new()
                };
                format!(
                    "Pinned {what} as {id} at {r} ({} %){more}.\n",
                    v["pin"]["zoom"]
                )
            }
            _ if file.is_some() => {
                "Would read the file once and pin it (dry run: nothing was read).\n".to_string()
            }
            _ => {
                "Would read the clipboard once and pin what it holds (dry run: nothing was read).\n"
                    .to_string()
            }
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, at, kind, file);
        Err(unsupported())
    }
}

/// `x,y` in device pixels as `{"x", "y"}`.
fn parse_point(text: &str) -> Option<Value> {
    let (x, y) = text.split_once(',')?;
    let coord = |s: &str| {
        s.trim()
            .parse::<i32>()
            .ok()
            .filter(|v| v.unsigned_abs() <= 1_000_000)
    };
    Some(json!({"x": coord(x)?, "y": coord(y)?}))
}

/// The `id`/`group`/`all` parameters of a pin request.
#[cfg(windows)]
fn targets(id: Option<&String>, group: Option<&String>, dry_run: bool) -> Value {
    match (id, group) {
        (Some(id), _) => json!({"id": id, "dry_run": dry_run}),
        (None, Some(g)) => json!({"group": g, "dry_run": dry_run}),
        (None, None) => json!({"all": true, "dry_run": dry_run}),
    }
}

/// One pin's view in a line: `100 %, 90°, mirrored, 50 % opaque, thumbnail, click-through`.
#[cfg(windows)]
fn view_words(p: &Value) -> String {
    let mut w = vec![format!("{} %", p["zoom"])];
    let rot = p["orientation"]["rotation"].as_u64().unwrap_or(0);
    if rot != 0 {
        w.push(format!("{rot}\u{b0}"));
    }
    if p["orientation"]["mirrored"] == true {
        w.push("mirrored".into());
    }
    let op = p["opacity"].as_u64().unwrap_or(100);
    if op != 100 {
        w.push(format!("{op} % opaque"));
    }
    if p["thumbnail"] == true {
        w.push("thumbnail".into());
    }
    if p["click_through"] == true {
        w.push("click-through".into());
    }
    if let Some(n) = p["animation"]["frames"].as_u64() {
        w.push(format!("first of {n} GIF frames"));
    }
    w.join(", ")
}

/// Pin IDs of a result list, comma-separated.
#[cfg(windows)]
fn names(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `noeul pin hide`: hides pins without closing them (reversible, so no confirmation).
pub fn pin_hide(ctx: &Ctx, a: &crate::PinHideArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let v = session(ctx)?.request(
            "pin.hide",
            targets(a.id.as_ref(), a.group.as_ref(), a.dry_run),
        )?;
        let list = names(&v["hidden"]);
        let human = match (list.is_empty(), a.dry_run) {
            (true, _) => "Nothing to hide.\n".to_string(),
            (false, true) => format!("Would hide: {list} (dry run).\n"),
            (false, false) => format!("Hidden: {list} (`noeul pin show` brings them back).\n"),
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

/// `noeul pin show`: shows hidden pins; `--on-screen` also brings lost pins back.
pub fn pin_show(ctx: &Ctx, a: &crate::PinShowArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let mut params = targets(a.id.as_ref(), a.group.as_ref(), a.dry_run);
        params["on_screen"] = json!(a.on_screen);
        let v = session(ctx)?.request("pin.show", params)?;
        let (shown, moved) = (names(&v["shown"]), names(&v["moved"]));
        let verb = if a.dry_run { "Would show" } else { "Shown" };
        let mut human = String::new();
        if !shown.is_empty() {
            human.push_str(&format!("{verb}: {shown}.\n"));
        }
        if !moved.is_empty() {
            let verb = if a.dry_run {
                "Would bring on screen"
            } else {
                "Brought on screen"
            };
            human.push_str(&format!("{verb}: {moved}.\n"));
        }
        if human.is_empty() {
            human.push_str("Nothing to show: the pins are shown and on screen.\n");
        }
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

pub fn pin_close(ctx: &Ctx, a: &crate::PinCloseArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let mut s = session(ctx)?;
        if !a.yes && !a.dry_run {
            let what = match (&a.id, &a.group) {
                (Some(id), _) => id.clone(),
                (None, Some(g)) => format!("every pin of group {g}"),
                (None, None) => "every pin".into(),
            };
            match confirm(
                ctx,
                &format!("Close {what}? Only the last closed pin can be reopened. [y/N]"),
                "y",
            ) {
                Some(true) => {}
                Some(false) => {
                    return Err(NoeulError::new(Code::Cancelled, "nothing was closed"));
                }
                None => {
                    return Err(NoeulError::new(
                        Code::ConfirmationRequired,
                        "closing pins needs confirmation",
                    )
                    .hint("re-run with --yes (or --dry-run to preview); nothing was closed"));
                }
            }
        }
        let params = targets(a.id.as_ref(), a.group.as_ref(), a.dry_run);
        let v = s.request("pin.close", params)?;
        let names: Vec<&str> = v["closed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let human = if a.dry_run {
            format!("Would close: {} (dry run).\n", names.join(", "))
        } else if names.is_empty() {
            "No pins were open.\n".into()
        } else {
            format!("Closed: {}.\n", names.join(", "))
        };
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

pub fn pin_reopen(ctx: &Ctx, dry_run: bool) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let v = session(ctx)?.request("pin.reopen", json!({"dry_run": dry_run}))?;
        let human = format!(
            "{} {}.\n",
            if dry_run { "Would reopen" } else { "Reopened" },
            v["pin"].as_str().unwrap_or("")
        );
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, dry_run);
        Err(unsupported())
    }
}

/// `noeul pin set`: changes pins precisely (Step 54). Every value is checked by the instance
/// before anything changes; a dry run changes nothing.
pub fn pin_set(ctx: &Ctx, a: &crate::PinSetArgs) -> Result<Outcome> {
    let on = |v: &Option<String>| v.as_deref().map(|v| v == "on");
    let mut params = json!({"dry_run": a.dry_run});
    match (&a.id, &a.group) {
        (Some(id), _) => params["id"] = json!(id),
        (None, Some(g)) => params["group"] = json!(g),
        (None, None) => params["all"] = json!(true),
    }
    if let Some(text) = &a.at {
        params["at"] = parse_point(text).ok_or_else(|| {
            NoeulError::new(Code::Usage, format!("`{text}` is not a point; use x,y"))
                .hint("for example: noeul pin set pin-1 --at 100,200")
        })?;
    }
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            params[k] = v;
        }
    };
    put("zoom", a.zoom.map(|z| json!(z)));
    put("opacity", a.opacity.map(|o| json!(o)));
    put("rotate", a.rotate.as_ref().map(|r| json!(r)));
    put(
        "rotation",
        a.rotation
            .as_deref()
            .and_then(|r| r.parse::<u32>().ok())
            .map(|r| json!(r)),
    );
    put("flip", a.flip.as_ref().map(|f| json!(f)));
    put("mirrored", on(&a.mirrored).map(|b| json!(b)));
    put("thumbnail", on(&a.thumbnail).map(|b| json!(b)));
    put("click_through", on(&a.click_through).map(|b| json!(b)));
    put("move_to_group", a.move_to_group.as_ref().map(|g| json!(g)));
    let given = params.as_object().map_or(0, |o| o.len());
    if given <= 2 {
        return Err(NoeulError::new(Code::Usage, "nothing to change").hint(
            "give --at, --zoom, --opacity, --rotate, --rotation, --flip, --mirrored, --thumbnail, --click-through or --move-to-group",
        ));
    }
    #[cfg(windows)]
    {
        let v = session(ctx)?.request("pin.set", params)?;
        let mut human = String::new();
        let changed = names(&v["changed"]);
        if a.dry_run {
            human.push_str(&format!(
                "Would change: {} (dry run).\n",
                names(&v["targets"])
            ));
        } else if changed.is_empty() {
            human.push_str("Nothing changed.\n");
        } else {
            human.push_str(&format!("Changed: {changed}.\n"));
        }
        for p in v["pins"].as_array().into_iter().flatten() {
            let r = &p["rect"];
            human.push_str(&format!(
                "  {} at {},{} {}\u{d7}{}: {} (group {})\n",
                p["id"].as_str().unwrap_or(""),
                r["x"],
                r["y"],
                r["width"],
                r["height"],
                view_words(p),
                p["group"].as_str().unwrap_or("")
            ));
        }
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Err(unsupported())
    }
}

/// `noeul pin group list`: the groups and the current one.
pub fn pin_group_list(ctx: &Ctx) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let v = session(ctx)?.request("group.list", json!({}))?;
        let current = v["current"].as_str().unwrap_or("");
        let mut rows = vec![vec![
            "GROUP".to_string(),
            "PINS".into(),
            "HIDDEN".into(),
            String::new(),
        ]];
        for g in v["groups"].as_array().into_iter().flatten() {
            let name = g["name"].as_str().unwrap_or("");
            rows.push(vec![
                name.to_string(),
                g["pins"].to_string(),
                g["hidden"].to_string(),
                if name == current {
                    "current".into()
                } else {
                    String::new()
                },
            ]);
        }
        Ok(Outcome::new(v, noeul::util::table(&rows)))
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Err(unsupported())
    }
}

/// `noeul pin group switch <name>`: its pins show, every other pin hides, new pins go to it.
pub fn pin_group_switch(ctx: &Ctx, a: &crate::PinGroupSwitchArgs) -> Result<Outcome> {
    #[cfg(windows)]
    {
        let v = session(ctx)?.request(
            "group.switch",
            json!({"group": a.name, "dry_run": a.dry_run}),
        )?;
        let (shown, hidden) = (names(&v["shown"]), names(&v["hidden"]));
        let none = |s: &str| {
            if s.is_empty() {
                "none".to_string()
            } else {
                s.to_string()
            }
        };
        let human = format!(
            "{} group {}: shown {}, hidden {}.\n",
            if a.dry_run {
                "Would switch to"
            } else {
                "Switched to"
            },
            v["group"].as_str().unwrap_or(""),
            none(&shown),
            none(&hidden)
        );
        Ok(Outcome::new(v, human))
    }
    #[cfg(not(windows))]
    {
        let _ = (ctx, a);
        Err(unsupported())
    }
}

/// `noeul config show`: settings and the stable paths (reads files only).
pub fn config_show() -> Result<Outcome> {
    let paths = Paths::from_env()?;
    let s = settings::load(&paths.config_file());
    let v = json!({
        "settings": s.to_json(),
        "paths": {
            "config_dir": paths.config_dir.display().to_string(),
            "config_file": paths.config_file().display().to_string(),
            "hotkeys_file": paths.hotkeys_file().display().to_string(),
            "data_dir": paths.data_dir.display().to_string(),
        },
    });
    let st = &s.settings;
    let preset = if st.preset == settings::Preset::Snipaste {
        format!("{} (on)", st.preset.title())
    } else {
        "off".to_string()
    };
    let human = format!(
        "Settings: {} ({})\n  general.tray_icon = {}\n  capture.keyboard_help = {}\n  capture.quick_save_folder = {}\n  capture.detection = {}\n  capture.magnifier = {}\n  capture.pointer = {}\n  gestures.double_click = {}\n  gestures.middle_click = {}\n  gestures.preset = {} (Snipaste-style gestures: {preset})\n  paste.paths = {}\n  paste.file_cards = {}\nShortcuts: {}\nData folder (ui-state.json only, never pictures): {}\nChange a setting: noeul config set <key> <value>; reset: noeul config reset [<section>] --yes\n",
        paths.config_file().display(),
        s.state.as_str(),
        st.tray_icon,
        st.keyboard_help.as_str(),
        st.quick_save_folder.as_ref().map_or_else(
            || "(default: the Pictures folder + Noeul)".to_string(),
            |p| p.display().to_string()
        ),
        st.detection.as_str(),
        if st.magnifier { "on" } else { "off" },
        if st.pointer { "on" } else { "off" },
        st.gestures.double_click.as_str(),
        st.gestures.middle_click.as_str(),
        st.preset.as_str(),
        if st.paste.paths { "on" } else { "off" },
        if st.paste.file_cards { "on" } else { "off" },
        paths.hotkeys_file().display(),
        paths.data_dir.display()
    );
    let mut out = Outcome::new(v, human);
    out.warnings = s.problems;
    Ok(out)
}

/// The running instance's global registrations `(action, keys, status)`; `Ok(None)` when no
/// instance runs.
pub fn registrations(ctx: &Ctx) -> Result<Option<Vec<(String, String, String)>>> {
    #[cfg(windows)]
    {
        let Some(mut s) = connect(ctx)? else {
            return Ok(None);
        };
        let v = s.request("status", json!({}))?;
        Ok(Some(
            v["hotkeys"]["registrations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    (
                        r["action"].as_str().unwrap_or("").to_string(),
                        r["keys"].as_str().unwrap_or("").to_string(),
                        r["status"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect(),
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Err(unsupported())
    }
}

/// Tells a running instance to re-read `hotkeys.json`; a sentence when that did not work.
/// Asks a running instance to read `config.toml` again (IPC `settings.reload`). `Ok(true)` when
/// it did, `Ok(false)` when none runs; the file watcher would also notice the change.
pub fn reload_settings(ctx: &Ctx) -> std::result::Result<bool, String> {
    #[cfg(windows)]
    {
        match connect(ctx) {
            Ok(None) => Ok(false),
            Ok(Some(mut s)) => s
                .request("settings.reload", json!({}))
                .map(|_| true)
                .map_err(|e| {
                    format!(
                        "the running Noeul could not reload its settings: {}",
                        e.message
                    )
                }),
            Err(e) => Err(format!(
                "the running Noeul could not be told to reload: {}",
                e.message
            )),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Ok(false)
    }
}

pub fn reload_hotkeys(ctx: &Ctx) -> Option<String> {
    #[cfg(windows)]
    {
        match connect(ctx) {
            Ok(None) => None,
            Ok(Some(mut s)) => match s.request("hotkeys.reload", json!({})) {
                Ok(v) => v["problem"].as_str().map(str::to_string),
                Err(e) => Some(format!(
                    "the running Noeul could not reload its shortcuts: {}",
                    e.message
                )),
            },
            Err(e) => Some(format!(
                "the running Noeul could not be told to reload: {}",
                e.message
            )),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        None
    }
}
