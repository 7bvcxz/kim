//! Step 72 mode-state tests: chains and parents, every policy (exclusive, layer, temporary hold,
//! one-shot, timeout), the safe reset, reloads, previews, summaries and the indicator
//! (`DDALGAK-FR-016`, `DDALGAK-FR-020`, `DDALGAK-SEC-005`).

use super::*;
use crate::model::{Mode, Profile};
use serde_json::json;

fn mode(id: &str, title: &str) -> Mode {
    Mode::new(id, title)
}

/// base (default), nav (layer over base), sym (layer over nav), game (exclusive), quick
/// (one-shot), timed (5 s timeout).
fn profile() -> Profile {
    let mut nav = mode("nav", "Navigation");
    nav.parent = Some("base".into());
    nav.icon = Some("N".into());
    nav.color = Some("#B7A6F5".into());
    let mut sym = mode("sym", "Symbols");
    sym.parent = Some("nav".into());
    let mut quick = mode("quick", "Quick");
    quick.one_shot = Some(true);
    let mut timed = mode("timed", "Timed");
    timed.timeout_ms = Some(5000);
    Profile {
        id: "p".into(),
        title: "P".into(),
        description: None,
        default_mode: "base".into(),
        enabled: None,
        modes: vec![
            mode("base", "Base"),
            nav,
            sym,
            mode("game", "Gaming"),
            quick,
            timed,
        ],
        rules: Vec::new(),
        extra: serde_json::Map::new(),
    }
}

fn set() -> ModeSet {
    ModeSet::from_profile(&profile())
}

fn id(s: &ModeSet, m: u8) -> &str {
    s.id(m)
}

fn ix(s: &ModeSet, id: &str) -> u8 {
    s.index(id).unwrap()
}

#[test]
fn chains_follow_parents_and_broken_parents_are_dropped() {
    let s = set();
    let chain = |m: &str| {
        s.chain(ix(&s, m))
            .iter()
            .map(|c| s.id(*c).to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(chain("sym"), ["sym", "nav", "base"]);
    assert_eq!(chain("nav"), ["nav", "base"]);
    assert_eq!(chain("game"), ["game"], "an exclusive mode has no fallback");
    assert_eq!(s.get(ix(&s, "nav")).unwrap().policy(), "layer");
    assert_eq!(s.get(ix(&s, "game")).unwrap().policy(), "exclusive");
    // Precedence ranks: the mode itself, then its parent, …; rules without modes last.
    let bit = |m: &str| 1u32 << ix(&s, m);
    let sym = ix(&s, "sym");
    assert_eq!(s.rank(sym, bit("sym")), Some(0));
    assert_eq!(s.rank(sym, bit("nav")), Some(1));
    assert_eq!(s.rank(sym, bit("base") | bit("game")), Some(2));
    assert_eq!(s.rank(sym, bit("game")), None, "not in this chain");
    assert_eq!(s.rank(sym, 0), Some(MAX_MODES));
    assert_eq!(
        s.mask(&["nav".into(), "sym".into()]),
        Some(bit("nav") | bit("sym"))
    );
    assert_eq!(s.mask(&["nope".into()]), None);

    // A cycle (a → b → a) and a missing parent are dropped; a mode leading into the cycle keeps
    // its parent, which now ends there. The default mode never has a parent.
    let pairs = [
        ("base", Some("a")),
        ("a", Some("b")),
        ("b", Some("a")),
        ("c", Some("a")),
        ("d", Some("missing")),
    ];
    assert_eq!(
        effective_parents(&pairs, 0),
        [None, None, None, Some(1), None]
    );
    let mut p = profile();
    p.modes[0].parent = Some("nav".into());
    p.modes[0].one_shot = Some(true);
    p.modes[0].timeout_ms = Some(2000);
    let s2 = ModeSet::from_profile(&p);
    let base = s2.get(s2.default_mode()).unwrap();
    assert!(base.parent.is_none() && !base.one_shot && base.timeout_ms.is_none());
    assert_eq!(ModeSet::single().id(0), "base");
}

#[test]
fn exclusive_switches_set_toggle_cycle_previous_and_default() {
    let s = set();
    let mut st = ModeState::new(&s);
    assert_eq!(id(&s, st.active()), "base");
    let game = ix(&s, "game");
    let c = st.apply(&s, &ModeAction::Set(game), 0x7C, 3, 10).unwrap();
    assert_eq!(
        c,
        Change {
            state: true,
            active: true
        }
    );
    assert_eq!(
        (id(&s, st.active()), id(&s, st.previous())),
        ("game", "base")
    );
    assert_eq!(st.view().origin, ModeOrigin::Rule(3));
    // Setting the active mode again changes nothing.
    assert_eq!(
        st.apply(&s, &ModeAction::Set(game), 0x7C, 3, 20).unwrap(),
        Change::default()
    );
    // Toggle: on goes to the mode, off returns to the previous one.
    let nav = ix(&s, "nav");
    st.apply(&s, &ModeAction::Toggle(nav), 0x7D, 4, 30).unwrap();
    assert_eq!(id(&s, st.active()), "nav");
    st.apply(&s, &ModeAction::Toggle(nav), 0x7D, 4, 40).unwrap();
    assert_eq!(id(&s, st.active()), "game", "back to the mode before");
    // Cycle: the next after the active one; from outside the list, the first.
    let list: Arc<[u8]> = vec![ix(&s, "base"), nav, ix(&s, "sym")].into();
    st.apply(&s, &ModeAction::Cycle(Arc::clone(&list)), 0x7E, 5, 50)
        .unwrap();
    assert_eq!(id(&s, st.active()), "base", "game is not in the list");
    st.apply(&s, &ModeAction::Cycle(Arc::clone(&list)), 0x7E, 5, 60)
        .unwrap();
    st.apply(&s, &ModeAction::Cycle(Arc::clone(&list)), 0x7E, 5, 70)
        .unwrap();
    assert_eq!(id(&s, st.active()), "sym");
    st.apply(&s, &ModeAction::Cycle(list), 0x7E, 5, 80).unwrap();
    assert_eq!(id(&s, st.active()), "base", "wraps around");
    // Previous swaps back and forth; default goes home.
    st.apply(&s, &ModeAction::Previous, 0x7F, 6, 90).unwrap();
    assert_eq!(id(&s, st.active()), "sym");
    st.apply(&s, &ModeAction::Default, 0x80, 7, 100).unwrap();
    assert_eq!(id(&s, st.active()), "base");
    assert!(st.view().changes >= 8);
}

#[test]
fn held_layers_stack_end_with_their_keys_and_never_outlive_a_switch() {
    let s = set();
    let (nav, sym, game) = (ix(&s, "nav"), ix(&s, "sym"), ix(&s, "game"));
    let mut st = ModeState::new(&s);
    // CapsLock held: Navigation while held.
    let c = st.apply(&s, &ModeAction::Hold(nav), 0x14, 1, 10).unwrap();
    assert!(c.active);
    assert_eq!((id(&s, st.active()), id(&s, st.latched())), ("nav", "base"));
    assert!(st.holds_key(0x14));
    // Tab held as well: Symbols on top.
    st.apply(&s, &ModeAction::Hold(sym), 0x09, 2, 20).unwrap();
    assert_eq!(id(&s, st.active()), "sym");
    assert_eq!(st.view().held_len, 2);
    // CapsLock released first: Symbols stays (Tab is still held); then Tab: back to Base.
    let c = st.release(0x14);
    assert_eq!(
        c,
        Change {
            state: true,
            active: false
        }
    );
    assert_eq!(id(&s, st.active()), "sym");
    let c = st.release(0x09);
    assert!(c.active);
    assert_eq!(id(&s, st.active()), "base");
    assert_eq!(st.view().origin, ModeOrigin::Released(2));
    // A latched switch while a layer is held ends the layer; its key-up then changes nothing.
    st.apply(&s, &ModeAction::Hold(nav), 0x14, 1, 30).unwrap();
    st.apply(&s, &ModeAction::Set(game), 0x7C, 3, 40).unwrap();
    assert_eq!(st.held(), 0);
    assert_eq!(st.release(0x14), Change::default(), "no unexpected return");
    assert_eq!(id(&s, st.active()), "game");
    // Toggling a held layer off ends the layers.
    st.apply(&s, &ModeAction::Hold(nav), 0x14, 1, 50).unwrap();
    st.apply(&s, &ModeAction::Toggle(nav), 0x7D, 4, 60).unwrap();
    assert_eq!(id(&s, st.active()), "game");
    // Previous while held ends the layers too (the last change was the hold).
    st.apply(&s, &ModeAction::Hold(sym), 0x14, 1, 70).unwrap();
    st.apply(&s, &ModeAction::Previous, 0x7F, 6, 80).unwrap();
    assert_eq!(id(&s, st.active()), "game");
    // At most four layers; a fifth key gets `None` and passes unchanged.
    for (i, vk) in [0x41u16, 0x42, 0x43, 0x44].into_iter().enumerate() {
        assert!(
            st.apply(
                &s,
                &ModeAction::Hold(nav),
                vk,
                1,
                90 + u32::try_from(i).unwrap()
            )
            .is_some()
        );
    }
    assert!(st.apply(&s, &ModeAction::Hold(sym), 0x45, 1, 99).is_none());
    // A key reset ends every layer (their ups may never come); the latched mode stays.
    let c = st.drop_holds(ModeOrigin::KeyReset);
    assert!(c.active);
    assert_eq!(id(&s, st.active()), "game");
    // An outside request cannot hold (no key to hold).
    assert!(
        st.request(&s, &ModeAction::Hold(nav), ModeOrigin::Cli, 100)
            .is_none()
    );
}

#[test]
fn a_one_shot_mode_ends_after_the_up_of_its_one_key() {
    let s = set();
    let quick = ix(&s, "quick");
    let mut st = ModeState::new(&s);
    st.apply(&s, &ModeAction::Set(ix(&s, "nav")), 0x7C, 1, 0)
        .unwrap();
    // F14 switches to the one-shot mode; its own up does not end it.
    st.key_down(0x7D, false, 10);
    st.apply(&s, &ModeAction::Set(quick), 0x7D, 2, 10).unwrap();
    assert_eq!(st.key_up(&s, 0x7D, 20), Change::default());
    // Shift is a modifier: it does not count; A is the one key.
    st.key_down(0xA0, true, 30);
    st.key_down(0x41, false, 40);
    assert_eq!(
        id(&s, st.active()),
        "quick",
        "A is decided in the one-shot mode"
    );
    assert_eq!(st.key_up(&s, 0xA0, 50), Change::default());
    let c = st.key_up(&s, 0x41, 60);
    assert!(c.active);
    assert_eq!(id(&s, st.active()), "nav", "back to the mode it came from");
    assert_eq!(st.view().origin, ModeOrigin::OneShot);
    // A switch while waiting cancels the one-shot exit.
    st.apply(&s, &ModeAction::Set(quick), 0x7D, 2, 70).unwrap();
    st.key_down(0x7E, false, 80);
    st.apply(&s, &ModeAction::Set(ix(&s, "game")), 0x7E, 3, 80)
        .unwrap();
    assert_eq!(st.key_up(&s, 0x7E, 90), Change::default());
    assert_eq!(id(&s, st.active()), "game");
}

#[test]
fn a_timed_mode_returns_to_the_default_mode_after_idle_time() {
    let s = set();
    let timed = ix(&s, "timed");
    let mut st = ModeState::new(&s);
    assert_eq!(st.due_in(&s, 0), None, "no timer without a timed mode");
    st.apply(&s, &ModeAction::Set(timed), 0x7C, 1, 1000)
        .unwrap();
    assert_eq!(st.due_in(&s, 1000), Some(5000));
    assert_eq!(st.tick(&s, 3000), Change::default(), "too early");
    // A key press starts the idle time again.
    st.key_down(0x41, false, 4000);
    assert_eq!(st.due_in(&s, 8000), Some(1000));
    assert_eq!(st.tick(&s, 8000), Change::default());
    // While a layer is held the user is still at it.
    st.apply(&s, &ModeAction::Hold(ix(&s, "nav")), 0x14, 2, 8500)
        .unwrap();
    assert_eq!(st.tick(&s, 9000), Change::default());
    st.release(0x14);
    assert_eq!(st.due_in(&s, 9000), Some(5000));
    let c = st.tick(&s, 14_000);
    assert!(c.active);
    assert_eq!(id(&s, st.active()), "base");
    assert_eq!(st.view().origin, ModeOrigin::Timeout);
    assert_eq!(st.due_in(&s, 14_000), None, "the timer ends with the mode");
    // The event clock wraps around after 49 days.
    st.apply(&s, &ModeAction::Set(timed), 0x7C, 1, u32::MAX - 1000)
        .unwrap();
    assert_eq!(st.due_in(&s, 3000), Some(999));
}

#[test]
fn the_safe_reset_and_reloads_by_mode_id() {
    let s = set();
    let mut st = ModeState::new(&s);
    st.apply(&s, &ModeAction::Set(ix(&s, "game")), 0x7C, 1, 0)
        .unwrap();
    st.apply(&s, &ModeAction::Hold(ix(&s, "nav")), 0x14, 2, 0)
        .unwrap();
    let c = st.reset(&s, ModeOrigin::Pause);
    assert!(c.active);
    assert_eq!(
        (id(&s, st.active()), id(&s, st.previous()), st.held()),
        ("base", "base", 0)
    );
    assert_eq!(st.reset(&s, ModeOrigin::Pause), Change::default());

    // A reload keeps the latched mode by ID (even at another index) and ends held layers.
    st.apply(&s, &ModeAction::Set(ix(&s, "game")), 0x7C, 1, 0)
        .unwrap();
    st.apply(&s, &ModeAction::Hold(ix(&s, "nav")), 0x14, 2, 0)
        .unwrap();
    let mut p = profile();
    p.modes.swap(1, 3);
    let s2 = ModeSet::from_profile(&p);
    let st2 = st.carry(&s, &s2, 0);
    assert_eq!(s2.id(st2.active()), "game");
    assert_eq!(st2.held(), 0);
    assert_eq!(st2.view().origin, ModeOrigin::Reload);
    // A deleted mode falls back to the default mode.
    p.modes.retain(|m| m.id != "game");
    let s3 = ModeSet::from_profile(&p);
    let st3 = st.carry(&s, &s3, 0);
    assert_eq!(s3.id(st3.active()), "base");
    // The status mirror does the same.
    let status = s.status(&st.view(), &|_| None);
    assert_eq!(status.carried(&s3).active, "base");
    assert_eq!(status.carried(&s2).active, "game");
}

#[test]
fn status_previews_and_summaries_name_modes_never_keys() {
    let s = set();
    let mut st = ModeState::new(&s);
    st.apply(&s, &ModeAction::Hold(ix(&s, "nav")), 0x14, 7, 0)
        .unwrap();
    let status = s.status(&st.view(), &|b| (b == 7).then(|| "hold-nav".to_string()));
    assert_eq!(status.active, "nav");
    assert_eq!(status.title, "Navigation");
    assert_eq!(status.icon.as_deref(), Some("N"));
    assert_eq!(status.held, ["nav"]);
    assert_eq!(status.chain, ["nav", "base"]);
    assert_eq!(status.origin, "rule:hold-nav");
    let j = status.to_json(true, Some("2026-11-04T10:00:00Z"));
    assert_eq!(j["latched"], "base");
    assert_eq!(j["live"], true);
    let text = j.to_string();
    assert!(!text.contains("0x14") && !text.contains("CapsLock"));
    // The playground preview fits ddalgak-simulation-v1 `preview_mode`.
    let base = ModeState::new(&s);
    let p = preview(&s, &base, &ModeAction::Hold(ix(&s, "nav")));
    assert_eq!(
        p,
        json!({ "from": "base", "to": "nav", "until_release": true })
    );
    let sim: Value = serde_json::from_str(include_str!(
        "../../../../docs/contracts/schemas/ddalgak-simulation-v1.schema.json"
    ))
    .unwrap();
    let mut shape = sim["$defs"]["preview_mode"].clone();
    shape["$defs"] = sim["$defs"].clone();
    let v = crate::jsonschema::Validator::new(&shape);
    for a in [
        ModeAction::Set(ix(&s, "game")),
        ModeAction::Toggle(ix(&s, "nav")),
        ModeAction::Cycle(vec![0, 1].into()),
        ModeAction::Hold(ix(&s, "sym")),
        ModeAction::Previous,
        ModeAction::Default,
    ] {
        let p = preview(&s, &base, &a);
        assert!(v.validate(&p).unwrap().is_empty(), "{p}");
    }
    // The simulation example's sentence.
    assert_eq!(
        summary(&s, &ModeAction::Hold(ix(&s, "nav")), "CapsLock"),
        "Navigation is active while CapsLock is held, then the previous mode returns"
    );
    assert_eq!(
        summary(
            &s,
            &ModeAction::Cycle(vec![0, 1, 3].into()),
            "Ctrl+Shift+F2"
        ),
        "Ctrl+Shift+F2 cycles through Base, Navigation, Gaming"
    );
    assert_eq!(
        summary(&s, &ModeAction::Default, "Escape"),
        "Escape returns to Base"
    );
}

#[test]
fn the_indicator_is_off_where_not_needed_and_never_color_only() {
    let s = set();
    let mut st = ModeState::new(&s);
    let home = s.status(&st.view(), &|_| None);
    let auto = indicator(None, &s, &home);
    assert!(auto.visible, "a profile with several modes shows the mode");
    assert!(!auto.away);
    let one = ModeSet::single();
    let single = indicator(None, &one, &one.initial());
    assert!(
        !single.visible,
        "one mode: nothing to show (off by default)"
    );
    assert!(indicator(Some(ModeIndicator::Tray), &one, &one.initial()).visible);
    assert!(!indicator(Some(ModeIndicator::Off), &s, &home).visible);
    st.apply(&s, &ModeAction::Set(ix(&s, "nav")), 0x7C, 1, 0)
        .unwrap();
    let nav = indicator(None, &s, &s.status(&st.view(), &|_| None));
    assert!(nav.away);
    assert_eq!(nav.label, "N Navigation", "the title is always there");
    assert_eq!(nav.color.as_deref(), Some("#B7A6F5"));
    assert_eq!(
        tooltip(State::Running, Some(&nav)),
        "Ddalgak: engine on · mode N Navigation"
    );
    assert_eq!(tooltip(State::Off, Some(&nav)), "Ddalgak: engine off");
    assert_eq!(tooltip(State::Running, Some(&single)), "Ddalgak: engine on");
    let mut long = nav;
    long.label = "x".repeat(200);
    assert_eq!(tooltip(State::Running, Some(&long)).chars().count(), 127);
    let m = menu(&s, &s.status(&st.view(), &|_| None));
    let labels: Vec<&str> = m.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Base (default)",
            "N Navigation (layer over Base)",
            "Symbols (layer over Navigation)",
            "Gaming",
            "Quick",
            "Timed"
        ]
    );
    assert!(m[1].checked && !m[0].checked);
    assert!(indicator(None, &s, &home).to_json()["label"] == "Base");
}

#[test]
fn mode_steps_compile_against_the_profile() {
    let s = set();
    let ok = |v: Value| ModeAction::compile(&v, &s).unwrap();
    assert_eq!(
        ok(json!({ "type": "mode", "op": "set", "mode": "nav" })),
        ModeAction::Set(1)
    );
    assert_eq!(
        ok(json!({ "type": "mode", "op": "cycle", "modes": ["base", "game"] })),
        ModeAction::Cycle(vec![0, 3].into())
    );
    assert!(ok(json!({ "type": "mode", "op": "hold", "mode": "sym" })).holds());
    assert_eq!(
        ok(json!({ "type": "mode", "op": "previous" })).op(),
        "previous"
    );
    for bad in [
        json!({ "type": "mode", "op": "set", "mode": "nope" }),
        json!({ "type": "mode", "op": "set" }),
        json!({ "type": "mode", "op": "cycle", "modes": ["base"] }),
        json!({ "type": "mode", "op": "fly" }),
    ] {
        assert!(ModeAction::compile(&bad, &s).is_err(), "{bad}");
    }
}

#[test]
fn nothing_switches_to_a_disabled_mode_and_a_reload_leaves_it() {
    // Step 73: `enabled: false` on a mode.
    let mut p = profile();
    p.modes[3].enabled = Some(false); // game
    p.modes[0].enabled = Some(false); // base, the default: ignored
    let s = ModeSet::from_profile(&p);
    let game = ix(&s, "game");
    let nav = ix(&s, "nav");
    assert!(!s.is_enabled(game));
    assert!(
        s.is_enabled(s.default_mode()),
        "the default mode is never disabled"
    );
    assert_eq!(s.enabled_mask((1 << game) | (1 << nav)), 1 << nav);
    let mut st = ModeState::new(&s);
    for a in [
        ModeAction::Set(game),
        ModeAction::Toggle(game),
        ModeAction::Hold(game),
        ModeAction::Cycle(vec![game].into()),
    ] {
        assert_eq!(st.apply(&s, &a, 0x7C, 0, 0), None, "{a:?}");
        assert_eq!(st.active(), s.default_mode());
        assert!(a.targets_disabled(&s));
    }
    // A cycle skips the disabled mode.
    let cycle = ModeAction::Cycle(vec![s.default_mode(), game, nav].into());
    assert!(!cycle.targets_disabled(&s));
    st.apply(&s, &cycle, 0x7C, 0, 0).unwrap();
    assert_eq!(
        id(&s, st.active()),
        "nav",
        "base, then game skipped, then nav"
    );
    // Enabled, then disabled by a reload while active: back to the default mode.
    let on = set();
    let mut st = ModeState::new(&on);
    st.apply(&on, &ModeAction::Set(ix(&on, "game")), 0x7C, 0, 0)
        .unwrap();
    let carried = st.carry(&on, &s, 0);
    assert_eq!(id(&s, carried.active()), "base");
    let status = on.status(&st.view(), &|_| None);
    assert_eq!(status.carried(&s).active, "base");
    // The mode menu does not offer it.
    assert!(menu(&s, &s.initial()).iter().all(|e| e.id != "game"));
}
