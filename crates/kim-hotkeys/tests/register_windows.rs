//! Real `RegisterHotKey` tests on Windows (docs/HOTKEY_CONTRACT.md §9.2, §14; KIM-SEC-020,
//! KIM-NFR-020).
//!
//! The maintainer uses this machine while tests run, so these tests touch **only two obscure
//! chords nobody uses**: `Ctrl+Alt+Shift+F24` (first test) and `Ctrl+Alt+Shift+F23` (second
//! test), one chord per test so parallel tests never share one. They never register F1, F3,
//! PrintScreen or any common key ([`obscure`] refuses anything else). Every registration is
//! released in every path, failures included: a `Registrar` unregisters everything it holds when
//! it is dropped, which also happens while a failing assertion unwinds.
//!
//! If the session cannot register hotkeys at all (a service session without a desktop), the
//! tests say so on stderr and stop instead of failing; on an interactive desktop they must pass.
#![cfg(windows)]

use kim_hotkeys::keys::Chord;
use kim_hotkeys::register::{GlobalHotkeys, RegisterError, Registrar, Status};
use std::time::{Duration, Instant};

const F24: &str = "Ctrl+Alt+Shift+F24";
const F23: &str = "Ctrl+Alt+Shift+F23";

/// The only chords these tests may register.
fn obscure(s: &str) -> Chord {
    assert!(
        s == F24 || s == F23,
        "tests may only register {F24} or {F23}"
    );
    Chord::parse(s).unwrap()
}

/// Registers on `r`, or explains why the environment cannot and returns `None`.
fn first_registration(r: &Registrar, chord: &Chord) -> Option<kim_hotkeys::register::HotkeyId> {
    match r.register(chord) {
        Ok((Some(id), Status::Registered)) => Some(id),
        Ok((_, Status::Taken)) => {
            eprintln!("skipped: another program already holds {chord}; nothing was taken from it");
            None
        }
        Err(RegisterError::Os { code }) => {
            eprintln!("skipped: this session cannot register hotkeys (Windows error {code})");
            None
        }
        other => panic!("unexpected registration result {other:?}"),
    }
}

#[test]
fn a_second_registration_is_taken_and_the_first_is_released_on_drop() {
    let chord = obscure(F24);
    let started = Instant::now();
    let a = Registrar::start().expect("registration thread starts");
    let start_cost = started.elapsed();
    let t = Instant::now();
    let Some(_held) = first_registration(&a, &chord) else {
        return;
    };
    let register_cost = t.elapsed();
    // A second registrant (another thread, like another program) is refused: `taken`.
    let b = Registrar::start().unwrap();
    let (id, status) = b.register(&chord).unwrap();
    assert_eq!(
        status,
        Status::Taken,
        "ERROR_HOTKEY_ALREADY_REGISTERED is reported"
    );
    assert!(id.is_none());
    // Asking again changes nothing: the owner keeps it (never stolen, KIM-SEC-020).
    assert_eq!(b.register(&chord).unwrap().1, Status::Taken);
    // Dropping the owner unregisters its hotkeys and ends its thread.
    let t = Instant::now();
    drop(a);
    let drop_cost = t.elapsed();
    let (id, status) = b.register(&chord).unwrap();
    assert_eq!(status, Status::Registered, "the drop released the chord");
    // An explicit unregister releases it too.
    b.unregister(id.unwrap()).unwrap();
    let c = Registrar::start().unwrap();
    let (id, status) = c.register(&chord).unwrap();
    assert_eq!(status, Status::Registered, "unregister released the chord");
    c.unregister(id.unwrap()).unwrap();
    assert!(c.unregister(id.unwrap()).is_err(), "an id is released once");
    eprintln!(
        "timing: start {start_cost:?}, register {register_cost:?}, drop (unregister + join) {drop_cost:?}"
    );
}

#[test]
fn lost_registrations_stay_inactive_and_are_never_taken_back() {
    let chord = obscure(F23);
    let mut tool = GlobalHotkeys::new(Registrar::start().unwrap());
    let wanted = vec![("capture.region".to_string(), chord)];
    let first = tool.sync(&wanted);
    match &first[0].1 {
        Ok(Status::Registered) => {}
        Ok(Status::Taken) => {
            eprintln!("skipped: another program already holds {chord}");
            return;
        }
        Err(e) => {
            eprintln!("skipped: this session cannot register hotkeys ({e})");
            return;
        }
    }
    // Syncing the same bindings again keeps the registration (no release/re-register race).
    assert_eq!(tool.sync(&wanted)[0].1, Ok(Status::Registered));
    // A reload turns the binding off: it is released at once.
    tool.release_all();
    // Meanwhile another program registers the chord.
    let other = Registrar::start().unwrap();
    let (other_id, status) = other.register(&chord).unwrap();
    assert_eq!(status, Status::Registered, "release_all released it");
    // The tool wants it back: it is `taken` now and stays inactive; nothing is retried or stolen.
    assert_eq!(tool.sync(&wanted)[0].1, Ok(Status::Taken));
    assert_eq!(tool.action_for(other_id.unwrap()), None);
    assert!(
        tool.registrar()
            .next_press(Duration::from_millis(1))
            .is_none(),
        "no event without a key press"
    );
    // Only after the other program released it does an explicit sync register it again.
    drop(other);
    let again = tool.sync(&wanted);
    assert_eq!(again[0].1, Ok(Status::Registered));
    drop(tool); // unregisters
    let check = Registrar::start().unwrap();
    let (id, status) = check.register(&chord).unwrap();
    assert_eq!(
        status,
        Status::Registered,
        "dropping the tool released everything"
    );
    check.unregister(id.unwrap()).unwrap();
}

#[test]
fn chords_beyond_register_hotkey_are_refused_before_any_os_call() {
    let r = Registrar::start().unwrap();
    for bad in [
        "LCtrl+Alt+Shift+F24",
        "Ctrl+Alt+Shift+code:KeyQ",
        "Win+F24",
        "F12",
    ] {
        let c = Chord::parse(bad).unwrap();
        assert!(
            matches!(r.register(&c), Err(RegisterError::Unsupported(_))),
            "{bad}"
        );
    }
}
