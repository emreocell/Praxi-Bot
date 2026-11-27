//! "Is something else on top at that point?" for pointer actions.
//!
//! A click by coordinate lands on whatever window is on top at the point, which may not be the
//! window that was observed (a dialog opened, another program came to the front). The check is
//! an additional refusal: it only ever stops an action that would have gone to another
//! program's window, and says nothing when it cannot tell.

use crate::snapshot::WindowInfo;

/// The window that covers the observed window `expected` at the point, if there is one that can
/// be named: the backend said a different top-level window is at the point, that window is in
/// the listing, and it belongs to another process (a popup or dialog of the same program is
/// part of the observed app and never counts). `None`: not covered, or cannot tell.
#[must_use]
pub fn covering_window(
    expected: u64,
    at_point: Option<u64>,
    windows: &[WindowInfo],
) -> Option<&WindowInfo> {
    let at = at_point.filter(|w| *w != expected)?;
    let top = windows.iter().find(|w| w.id == at)?;
    let observed_pid = windows.iter().find(|w| w.id == expected).map(|w| w.pid)?;
    (observed_pid == 0 || top.pid != observed_pid).then_some(top)
}

/// The refusal text for a pointer action covered by `top` at `(x, y)`.
#[must_use]
pub fn covered_message(top: &WindowInfo, x: f64, y: f64) -> String {
    let title: String = top
        .title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(60)
        .collect();
    format!(
        "another window (\"{}\", id {}) is on top at ({x:.0}, {y:.0}); the action was NOT made. Call desktop_list_apps, bring the window you mean to the front with desktop_focus, and look again",
        title.trim().replace('"', "'"),
        top.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Rect;

    fn win(id: u64, pid: u32, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            title: title.into(),
            pid,
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            focused: false,
        }
    }

    #[test]
    fn another_programs_window_on_top_covers_the_observed_one() {
        let ws = [win(1, 10, "Editor"), win(2, 20, "Save changes?")];
        let top = covering_window(1, Some(2), &ws).unwrap();
        assert_eq!(top.id, 2);
        let m = covered_message(top, 12.4, 30.0);
        assert!(
            m.contains("Save changes?") && m.contains("(12, 30)") && m.contains("NOT made"),
            "{m}"
        );
    }

    #[test]
    fn the_observed_window_itself_the_same_program_and_unknowns_never_refuse() {
        let ws = [
            win(1, 10, "Editor"),
            win(2, 10, "Editor dialog"),
            win(3, 30, "x"),
        ];
        assert!(
            covering_window(1, Some(1), &ws).is_none(),
            "the same window"
        );
        assert!(
            covering_window(1, Some(2), &ws).is_none(),
            "a popup of the same program"
        );
        assert!(
            covering_window(1, None, &ws).is_none(),
            "the backend cannot tell"
        );
        assert!(
            covering_window(1, Some(99), &ws).is_none(),
            "an unlisted window cannot be named"
        );
        assert!(
            covering_window(7, Some(3), &ws).is_none(),
            "the observed window is not listed"
        );
    }

    #[test]
    fn the_title_is_one_clean_line() {
        let w = win(5, 1, "bad\n\"title\"\u{7}");
        let m = covered_message(&w, 0.0, 0.0);
        assert!(!m.contains('\n') && !m.contains('\u{7}'), "{m}");
    }
}
