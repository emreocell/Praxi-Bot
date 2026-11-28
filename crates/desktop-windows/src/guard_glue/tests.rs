//! The Windows glue's decisions against a fake desktop: no Win32, no UIA.
#![allow(clippy::assert_is_empty)]

use std::collections::HashMap;

use desktop_api::{
    ActGuard, Action, ClickMethod, DesktopError, GuardFailure, InputProbe, InputTarget,
    ProcessIdentity, Refusal, Target, guard, guarded,
};

use super::*;

type W = u32;

fn ident(pid: u32) -> ProcessIdentity {
    ProcessIdentity {
        image_path: "C:\\Windows\\notepad.exe".into(),
        pid,
        start_time: 5,
    }
}

fn facts(window: u32, pid: u32, class: &str, owned: bool) -> InputTarget {
    InputTarget {
        window_id: u64::from(window),
        title: "t".into(),
        class: class.into(),
        identity: ident(pid),
        elevated: Some(false),
        password_focus: false,
        owned,
    }
}

/// A small fake desktop: window handles are numbers.
#[derive(Default)]
struct Fake {
    foreground: Option<W>,
    roots: HashMap<W, W>,
    top: HashMap<W, InputTarget>,
    child: HashMap<W, InputTarget>,
    procs: HashMap<u32, ProcessIdentity>,
    elevated: HashMap<u32, bool>,
    hit: Option<(W, W)>,
}

impl Fake {
    /// Notepad (pid 10) in the foreground: window 1, an edit child 2.
    fn notepad() -> Self {
        let mut f = Self {
            foreground: Some(1),
            hit: Some((2, 1)),
            ..Self::default()
        };
        f.roots.insert(1, 1);
        f.roots.insert(2, 1);
        f.top.insert(1, facts(1, 10, "Notepad", false));
        f.child.insert(2, facts(2, 10, "Edit", false));
        f.procs.insert(10, ident(10));
        f.elevated.insert(10, false);
        f
    }
}

impl WinFacts for Fake {
    type Window = W;
    fn foreground(&self) -> Option<W> {
        self.foreground
    }
    fn root_of(&self, w: W) -> Option<W> {
        self.roots.get(&w).copied()
    }
    fn top_level_facts(&self, w: W) -> Option<InputTarget> {
        self.top.get(&w).cloned()
    }
    fn child_facts(&self, w: W) -> Option<InputTarget> {
        self.child.get(&w).cloned()
    }
    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        self.procs.get(&pid).cloned()
    }
    fn process_elevated(&self, pid: u32) -> Option<bool> {
        self.elevated.get(&pid).copied()
    }
    fn point_hit(&self, _: f64, _: f64) -> Option<(W, W)> {
        self.hit
    }
}

fn raw(pid: i32, native: Option<W>) -> ElementRaw<W> {
    ElementRaw {
        pid: Ok(pid),
        password: Ok(false),
        native: Ok(native),
    }
}

fn guard_for(pid: u32) -> ActGuard {
    ActGuard { expect: ident(pid) }
}

/// What the guard says about every fact the glue produced for Notepad's grant.
fn judged(e: Result<Vec<InputTarget>, Refusal>) -> Result<(), Refusal> {
    e?.iter()
        .try_for_each(|f| guard::check_guard(&guard_for(10), f))
}

#[test]
fn an_element_without_a_window_handle_is_refused_never_the_foreground_window() {
    // The foreground is a perfectly eligible Notepad; the element has no handle.
    let os = Fake::notepad();
    assert_eq!(
        element_fact(&os, raw(10, None)).unwrap_err(),
        Refusal::NoIdentity
    );
    // And so is every set of facts that contains such a focused element.
    assert_eq!(
        keyboard_facts(&os, Ok(raw(10, None))).unwrap_err(),
        Refusal::NoIdentity
    );
    assert_eq!(
        write_facts(&os, raw(10, None), Ok(raw(10, Some(2)))).unwrap_err(),
        Refusal::NoIdentity
    );
    assert_eq!(
        write_facts(&os, raw(10, Some(2)), Ok(raw(10, None))).unwrap_err(),
        Refusal::NoIdentity
    );
}

#[test]
fn a_handle_without_a_root_or_facts_is_refused() {
    let mut os = Fake::notepad();
    assert_eq!(
        element_fact(&os, raw(10, Some(99))).unwrap_err(),
        Refusal::NoIdentity,
        "no root window"
    );
    os.roots.insert(99, 98);
    assert_eq!(
        element_fact(&os, raw(10, Some(99))).unwrap_err(),
        Refusal::NoIdentity,
        "the root has no readable facts (class lookup failed)"
    );
}

#[test]
fn a_negative_or_unreadable_pid_or_password_state_is_refused() {
    let os = Fake::notepad();
    assert_eq!(
        element_fact(&os, raw(-1, Some(2))).unwrap_err(),
        Refusal::NoIdentity
    );
    assert_eq!(
        element_fact(&os, raw(77, Some(2))).unwrap_err(),
        Refusal::NoIdentity,
        "no identity for that process"
    );
    let mut r = raw(10, Some(2));
    r.pid = Err(Refusal::NoIdentity);
    assert_eq!(element_fact(&os, r).unwrap_err(), Refusal::NoIdentity);
    let mut r = raw(10, Some(2));
    r.password = Err(Refusal::PasswordField);
    assert_eq!(element_fact(&os, r).unwrap_err(), Refusal::PasswordField);
}

#[test]
fn the_element_is_judged_by_its_own_process_not_its_windows() {
    let mut os = Fake::notepad();
    // A child of the Notepad window that belongs to another program.
    os.procs.insert(
        20,
        ProcessIdentity {
            image_path: "C:\\x\\other.exe".into(),
            pid: 20,
            start_time: 6,
        },
    );
    os.elevated.insert(20, false);
    let f = element_fact(&os, raw(20, Some(2))).unwrap();
    assert_eq!(
        f.identity.pid, 20,
        "the element's process, not the window's"
    );
    assert_eq!(
        judged(keyboard_facts(&os, Ok(raw(20, Some(2))))),
        Err(Refusal::ForegroundChanged),
        "a foreign process inside the granted window is not the granted program"
    );
}

#[test]
fn an_elevated_or_unknown_elevation_process_is_refused() {
    let mut os = Fake::notepad();
    os.elevated.insert(10, true);
    assert_eq!(
        judged(keyboard_facts(&os, Ok(raw(10, Some(2))))),
        Err(Refusal::Elevated)
    );
    // The window's own facts say "not elevated" but the element's token is unreadable.
    let mut os = Fake::notepad();
    os.elevated.remove(&10);
    let f = element_fact(&os, raw(10, Some(2))).unwrap();
    assert_eq!(f.elevated, None);
    assert_eq!(
        guard::check_guard(&guard_for(10), &f),
        Err(Refusal::Elevated),
        "an unreadable token is not 'not elevated'"
    );
}

#[test]
fn a_password_element_is_refused() {
    let os = Fake::notepad();
    let mut r = raw(10, Some(2));
    r.password = Ok(true);
    assert_eq!(
        judged(keyboard_facts(&os, Ok(r))),
        Err(Refusal::PasswordField)
    );
}

#[test]
fn an_owned_popup_or_a_failed_class_lookup_is_refused() {
    let mut os = Fake::notepad();
    os.top.insert(1, facts(1, 10, "Notepad", true));
    assert_eq!(
        judged(keyboard_facts(&os, Ok(raw(10, Some(2))))),
        Err(Refusal::Dialog)
    );
    // A failed class lookup yields no facts at all for that window.
    let mut os = Fake::notepad();
    os.top.remove(&1);
    assert_eq!(
        keyboard_facts(&os, Ok(raw(10, Some(2)))).unwrap_err(),
        Refusal::NoIdentity
    );
}

#[test]
fn no_foreground_is_a_refusal() {
    let mut os = Fake::notepad();
    os.foreground = None;
    assert_eq!(
        keyboard_facts(&os, Ok(raw(10, Some(2)))).unwrap_err(),
        Refusal::NoIdentity
    );
}

#[test]
fn a_clean_notepad_passes_and_reports_window_and_element() {
    let os = Fake::notepad();
    let f = keyboard_facts(&os, Ok(raw(10, Some(2)))).unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(judged(Ok(f)), Ok(()));
    let w = write_facts(&os, raw(10, Some(2)), Ok(raw(10, Some(2)))).unwrap();
    assert_eq!(w.len(), 3, "foreground, focus and the written element");
}

#[test]
fn a_click_checks_the_hit_child_and_its_root() {
    let mut os = Fake::notepad();
    let f = point_facts(&os, 1.0, 1.0).unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].class, "Edit", "the hit child");
    assert_eq!(f[1].class, "Notepad", "its root");
    // The hit child belongs to another program.
    os.child
        .insert(2, facts(2, 20, "Chrome_RenderWidgetHostHWND", false));
    let f = point_facts(&os, 1.0, 1.0).unwrap();
    assert_eq!(judged(Ok(f)), Err(Refusal::ForegroundChanged));
    // Nothing under the point, no facts for the child, none for the root.
    let mut os = Fake::notepad();
    os.hit = None;
    assert_eq!(point_facts(&os, 1.0, 1.0).unwrap_err(), Refusal::NoIdentity);
    let mut os = Fake::notepad();
    os.child.clear();
    assert_eq!(point_facts(&os, 1.0, 1.0).unwrap_err(), Refusal::NoIdentity);
    let mut os = Fake::notepad();
    os.top.clear();
    assert_eq!(point_facts(&os, 1.0, 1.0).unwrap_err(), Refusal::NoIdentity);
}

#[test]
fn the_probe_reads_live_focus_every_time() {
    let os = Fake::notepad();
    let calls = std::cell::Cell::new(0);
    let focused = || {
        calls.set(calls.get() + 1);
        Ok(raw(10, Some(2)))
    };
    let mods = || true;
    let mut p = Probe {
        os: &os,
        focused: &focused,
        modifiers_down: &mods,
    };
    p.keyboard_receivers().unwrap();
    p.keyboard_receivers().unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(p.modifiers_down(), Ok(true));
}

#[derive(Default)]
struct Api {
    typed: Vec<Vec<u16>>,
    fail: bool,
    released: bool,
}

impl InputApi for &mut Api {
    fn type_units(&mut self, units: &[u16]) -> Result<(), DesktopError> {
        if self.fail {
            return Err(DesktopError::Accessibility("SendInput blocked".into()));
        }
        self.typed.push(units.to_vec());
        Ok(())
    }
    fn key_chord(&mut self, _: &[String]) -> Result<(), DesktopError> {
        Ok(())
    }
    fn click_at(&mut self, _: f64, _: f64) -> Result<(), DesktopError> {
        Ok(())
    }
    fn release_all(&mut self) -> bool {
        self.released = true;
        true
    }
}

#[test]
fn the_sink_reports_the_os_error_and_the_guarded_type_stops_on_it() {
    let os = Fake::notepad();
    let focused = || Ok(raw(10, Some(2)));
    let mods = || false;
    let mut probe = Probe {
        os: &os,
        focused: &focused,
        modifiers_down: &mods,
    };
    let mut api = Api {
        fail: true,
        ..Api::default()
    };
    let err = guarded::guarded_type(&guard_for(10), "hello", &mut probe, &mut Sink(&mut api))
        .unwrap_err();
    assert!(
        err.os_error
            .as_deref()
            .unwrap()
            .contains("SendInput blocked")
    );
    assert!(api.released, "keys are released on the failure exit");
    assert!(matches!(to_error(&err), DesktopError::GuardRefused(_)));
    assert!(api.typed.is_empty());
}

#[test]
fn a_null_handle_focus_stops_typing_before_any_key_is_sent() {
    let os = Fake::notepad();
    let focused = || Ok(raw(10, None));
    let mods = || false;
    let mut probe = Probe {
        os: &os,
        focused: &focused,
        modifiers_down: &mods,
    };
    let mut api = Api::default();
    let err = guarded::guarded_type(&guard_for(10), "hello", &mut probe, &mut Sink(&mut api))
        .unwrap_err();
    assert_eq!(err.refusal, Some(Refusal::NoIdentity));
    assert!(api.typed.is_empty(), "nothing reached the keyboard");
}

#[test]
fn only_type_key_the_focusing_click_and_indexed_set_value_are_covered() {
    let click = |method| Action::Click {
        target: Target::ByCoordinate {
            x: 1.0,
            y: 1.0,
            space: desktop_api::CoordinateSpace::Absolute,
        },
        method,
    };
    assert!(matches!(
        plan(&click(ClickMethod::Global)),
        Ok(Plan::Click(_))
    ));
    for m in [ClickMethod::Accessibility, ClickMethod::AppPost] {
        assert!(matches!(
            plan(&click(m)),
            Err(DesktopError::GuardRefused(_))
        ));
    }
    let drag = Action::Drag {
        from: Target::ByCoordinate {
            x: 0.0,
            y: 0.0,
            space: desktop_api::CoordinateSpace::Absolute,
        },
        to: Target::ByCoordinate {
            x: 9.0,
            y: 9.0,
            space: desktop_api::CoordinateSpace::Absolute,
        },
        method: ClickMethod::Global,
    };
    assert!(matches!(plan(&drag), Err(DesktopError::GuardRefused(_))));
    let scroll = Action::Scroll {
        target: Target::ByIndex {
            index: 1,
            generation: 1,
        },
        delta_x: 0.0,
        delta_y: 1.0,
    };
    assert!(matches!(plan(&scroll), Err(DesktopError::GuardRefused(_))));
    // An accessibility action is never covered by a task-wide permission either.
    let element = Action::Element {
        target: Target::ByIndex {
            index: 1,
            generation: 1,
        },
        op: desktop_api::ElementOp::Invoke,
    };
    assert!(matches!(plan(&element), Err(DesktopError::GuardRefused(_))));
    let by_xy = Action::SetValue {
        target: Target::ByCoordinate {
            x: 1.0,
            y: 1.0,
            space: desktop_api::CoordinateSpace::Absolute,
        },
        value: "v".into(),
    };
    assert!(matches!(plan(&by_xy), Err(DesktopError::GuardRefused(_))));
    let by_index = Action::SetValue {
        target: Target::ByIndex {
            index: 3,
            generation: 4,
        },
        value: "v".into(),
    };
    assert!(matches!(
        plan(&by_index),
        Ok(Plan::SetValue {
            index: 3,
            generation: 4,
            value: "v"
        })
    ));
    assert!(matches!(
        plan(&Action::TypeText { text: "a".into() }),
        Ok(Plan::Type("a"))
    ));
}

#[test]
fn a_partial_send_is_not_called_not_sent() {
    let f = GuardFailure {
        refusal: Some(Refusal::ForegroundChanged),
        os_error: None,
        delivered_units: 24,
        total_units: 60,
        release_confirmed: true,
    };
    let text = to_error(&f).to_string();
    assert!(!text.starts_with("not sent"), "{text}");
    assert!(text.contains("24 of 60"), "{text}");
    let unconfirmed = GuardFailure {
        refusal: Some(Refusal::ReleaseUnconfirmed),
        os_error: None,
        delivered_units: 0,
        total_units: 0,
        release_confirmed: false,
    };
    let text = to_error(&unconfirmed).to_string();
    assert!(!text.starts_with("not sent"), "{text}");
    let clean = GuardFailure {
        refusal: Some(Refusal::Elevated),
        os_error: None,
        delivered_units: 0,
        total_units: 5,
        release_confirmed: true,
    };
    assert!(to_error(&clean).to_string().starts_with("not sent: "));
}
