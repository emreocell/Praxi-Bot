//! Multi-strategy locators.
//!
//! Resolution is ordered; the first strategy that finds *anything* decides.
//! Several matches are an [`Drift::Ambiguous`] drift, never a silent first pick.
//!
//! Desktop: role+name exact -> (automation id / class: reserved, the backend
//! does not expose them yet) -> role+name fuzzy -> position hint.
//! Browser: role+name exact -> text exact -> aria-label -> css (the replayer
//! runs the css strategy, it needs the live page).

use desktop_api::{Fingerprint, Snapshot, UiNode};

use crate::format::{BrowserLocator, DesktopLocator};

/// Similarity needed for the fuzzy strategy (bigram Dice coefficient).
pub const FUZZY_THRESHOLD: f64 = 0.85;
/// Similarity the name must still have for a position-hint match to count.
pub const POSITION_NAME_THRESHOLD: f64 = 0.5;
/// How far (pixels) a node may have moved for a position-hint match.
pub const POSITION_RADIUS_PX: f64 = 24.0;

/// Which strategy found an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Same role and exactly the same name.
    RoleNameExact,
    /// Same role, near-identical name.
    RoleNameFuzzy,
    /// Same role at (nearly) the recorded place.
    Position,
    /// Exactly the same text, any role.
    TextExact,
    /// Accessible name equals the recorded `aria-label`.
    AriaLabel,
    /// CSS selector (evaluated in the page).
    Css,
}

impl Strategy {
    /// Stable name used in events and reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoleNameExact => "role_name_exact",
            Self::RoleNameFuzzy => "role_name_fuzzy",
            Self::Position => "position",
            Self::TextExact => "text_exact",
            Self::AriaLabel => "aria_label",
            Self::Css => "css",
        }
    }
}

/// Why a locator did not resolve.
#[derive(Debug, Clone, PartialEq)]
pub enum Drift {
    /// Nothing matched any strategy.
    NotFound {
        /// Strategies that were tried.
        tried: Vec<&'static str>,
    },
    /// More than one element matched a strategy.
    Ambiguous {
        /// Strategy that matched several.
        strategy: Strategy,
        /// How many.
        count: usize,
    },
    /// An element was found but it is not the recorded one.
    FingerprintMismatch {
        /// Recorded fingerprint (hex).
        expected: String,
        /// Found fingerprint (hex).
        actual: String,
    },
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { tried } => {
                write!(f, "element not found (tried: {})", tried.join(", "))
            }
            Self::Ambiguous { strategy, count } => write!(
                f,
                "ambiguous: {count} elements match by {}",
                strategy.as_str()
            ),
            Self::FingerprintMismatch { expected, actual } => {
                write!(
                    f,
                    "fingerprint mismatch: recorded {expected}, found {actual}"
                )
            }
        }
    }
}

/// A resolved element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Position in the node list that was searched.
    pub node: usize,
    /// Strategy that found it.
    pub strategy: Strategy,
    /// The element's fingerprint differs from the recorded one (only possible
    /// for non-exact strategies when not in strict mode).
    pub fingerprint_changed: bool,
}

/// Hex form of a fingerprint, as stored in workflow files.
#[must_use]
pub fn fingerprint_hex(role: &str, name: &str) -> String {
    format!("{:016x}", Fingerprint::compute(role, name).0)
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Bigram Dice similarity of two normalised strings, `0.0..=1.0`.
#[must_use]
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize(a), normalize(b));
    if a == b {
        return 1.0;
    }
    let bigrams = |s: &str| -> Vec<(char, char)> {
        let c: Vec<char> = s.chars().collect();
        c.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let (ba, mut bb) = (bigrams(&a), bigrams(&b));
    if ba.is_empty() || bb.is_empty() {
        return 0.0;
    }
    let total = ba.len() + bb.len();
    let mut hits = 0usize;
    for g in &ba {
        if let Some(p) = bb.iter().position(|x| x == g) {
            bb.swap_remove(p);
            hits += 1;
        }
    }
    // Counts are tiny; the conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    {
        2.0 * hits as f64 / total as f64
    }
}

/// Case-insensitive wildcard match: `*` any run, `?` any one character.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn check_fp(
    expected: &str,
    role: &str,
    name: &str,
    strategy: Strategy,
    node: usize,
    strict: bool,
) -> Result<Found, Drift> {
    let actual = fingerprint_hex(role, name);
    let same = actual.eq_ignore_ascii_case(expected);
    if !same && (strategy == Strategy::RoleNameExact || strict) {
        return Err(Drift::FingerprintMismatch {
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(Found {
        node,
        strategy,
        fingerprint_changed: !same,
    })
}

fn pick(strategy: Strategy, hits: &[usize]) -> Result<Option<usize>, Drift> {
    match hits {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        many => Err(Drift::Ambiguous {
            strategy,
            count: many.len(),
        }),
    }
}

/// Resolve a desktop locator in a snapshot.
///
/// `strict` turns a fingerprint difference on a non-exact strategy into a drift.
///
/// # Errors
/// The drift that stops the replay.
pub fn resolve_desktop(
    loc: &DesktopLocator,
    snap: &Snapshot,
    strict: bool,
) -> Result<Found, Drift> {
    let nodes: &[UiNode] = &snap.nodes;
    let mut tried = Vec::new();

    // 1. role + name exact.
    tried.push(Strategy::RoleNameExact.as_str());
    let exact: Vec<usize> = (0..nodes.len())
        .filter(|&i| nodes[i].role == loc.role && nodes[i].name == loc.name)
        .collect();
    let chosen = match loc.nth {
        Some(n) => exact.get(n).copied(),
        None => pick(Strategy::RoleNameExact, &exact)?,
    };
    if let Some(i) = chosen {
        return check_fp(
            &loc.fingerprint,
            &nodes[i].role,
            &nodes[i].name,
            Strategy::RoleNameExact,
            i,
            strict,
        );
    }

    // 2. automation id / class: the backend's UiNode does not carry them yet.

    // 3. role + name fuzzy (same window: the snapshot is that window).
    if !loc.name.is_empty() {
        tried.push(Strategy::RoleNameFuzzy.as_str());
        let fuzzy: Vec<usize> = (0..nodes.len())
            .filter(|&i| {
                nodes[i].role == loc.role
                    && !nodes[i].name.is_empty()
                    && similarity(&nodes[i].name, &loc.name) >= FUZZY_THRESHOLD
            })
            .collect();
        if let Some(i) = pick(Strategy::RoleNameFuzzy, &fuzzy)? {
            return check_fp(
                &loc.fingerprint,
                &nodes[i].role,
                &nodes[i].name,
                Strategy::RoleNameFuzzy,
                i,
                strict,
            );
        }
    }

    // 4. position hint.
    if let Some(pos) = &loc.position {
        tried.push(Strategy::Position.as_str());
        let same_role: Vec<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].role == loc.role)
            .collect();
        let near: Vec<usize> = same_role
            .iter()
            .copied()
            .filter(|&i| {
                nodes[i].bounds.is_some_and(|b| {
                    let (cx, cy) = b.center();
                    (cx - pos.x).hypot(cy - pos.y) <= POSITION_RADIUS_PX
                })
            })
            .collect();
        let candidate = if near.is_empty() {
            // No coordinates to go by: same rank among the same role, but only
            // when the number of such elements is unchanged.
            if same_role.len() == pos.role_count {
                same_role.get(pos.role_ordinal).copied()
            } else {
                None
            }
        } else {
            pick(Strategy::Position, &near)?
        };
        if let Some(i) = candidate {
            let name_ok = loc.name.is_empty()
                || similarity(&nodes[i].name, &loc.name) >= POSITION_NAME_THRESHOLD;
            if name_ok {
                return check_fp(
                    &loc.fingerprint,
                    &nodes[i].role,
                    &nodes[i].name,
                    Strategy::Position,
                    i,
                    strict,
                );
            }
        }
    }
    Err(Drift::NotFound { tried })
}

// ── Browser ──────────────────────────────────────────────────────────────────

/// One element line of a browser snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserNode {
    /// Element ref (`e12`).
    pub ref_id: String,
    /// Accessibility role.
    pub role: String,
    /// Accessible name.
    pub name: String,
}

/// A parsed browser snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserPage {
    /// Page title.
    pub title: String,
    /// Page URL (from the `# title (url)` header), if present.
    pub url: Option<String>,
    /// Elements that have a ref.
    pub nodes: Vec<BrowserNode>,
}

/// Parse the text of `browser_snapshot` / `browser_open` (`[e12] button "Sign in"`).
#[must_use]
pub fn parse_browser_snapshot(text: &str) -> BrowserPage {
    let mut page = BrowserPage::default();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("# ") {
            if let Some(open) = h.rfind(" (")
                && let Some(url) = h[open + 2..].strip_suffix(')')
            {
                h[..open].clone_into(&mut page.title);
                page.url = Some(url.to_owned());
            } else if page.title.is_empty() {
                h.clone_into(&mut page.title);
            }
            continue;
        }
        if let Some(n) = parse_browser_line(line) {
            page.nodes.push(n);
        }
    }
    page
}

/// Parse one `  [e12] role "name" ...` line.
#[must_use]
pub fn parse_browser_line(line: &str) -> Option<BrowserNode> {
    let t = line.trim_start().strip_prefix("[e")?;
    let close = t.find(']')?;
    let digits = &t[..close];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = t[close + 1..].trim_start();
    let (role, after) = rest.split_once(' ').unwrap_or((rest, ""));
    if role.is_empty() {
        return None;
    }
    let name = after
        .strip_prefix('"')
        .map(|q| read_quoted(q).0)
        .unwrap_or_default();
    Some(BrowserNode {
        ref_id: format!("e{digits}"),
        role: role.to_owned(),
        name,
    })
}

/// Read a quoted string body (after the opening quote); returns the unescaped
/// text and the remainder after the closing quote.
#[must_use]
pub fn read_quoted(s: &str) -> (String, &str) {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, 't')) => out.push('\t'),
                Some((_, other)) => out.push(other),
                None => break,
            },
            '"' => return (out, &s[i + 1..]),
            c => out.push(c),
        }
    }
    (out, "")
}

/// Resolve a browser locator against a page (css is left to the replayer).
///
/// # Errors
/// The drift that stops the replay (`NotFound` also when only css is left to try).
pub fn resolve_browser(
    loc: &BrowserLocator,
    nodes: &[BrowserNode],
    strict: bool,
) -> Result<Found, Drift> {
    let mut tried = Vec::new();
    let fp = |n: &BrowserNode, s: Strategy, i: usize| {
        check_fp(&loc.fingerprint, &n.role, &n.name, s, i, strict)
    };

    if !loc.role.is_empty() {
        tried.push(Strategy::RoleNameExact.as_str());
        let exact: Vec<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].role == loc.role && nodes[i].name == loc.name)
            .collect();
        let chosen = match loc.nth {
            Some(n) => exact.get(n).copied(),
            None => pick(Strategy::RoleNameExact, &exact)?,
        };
        if let Some(i) = chosen {
            return fp(&nodes[i], Strategy::RoleNameExact, i);
        }
    }
    if !loc.name.is_empty() {
        tried.push(Strategy::TextExact.as_str());
        let hits: Vec<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].name == loc.name)
            .collect();
        if let Some(i) = pick(Strategy::TextExact, &hits)? {
            return fp(&nodes[i], Strategy::TextExact, i);
        }
    }
    if let Some(label) = loc.aria_label.as_deref().filter(|l| !l.is_empty()) {
        tried.push(Strategy::AriaLabel.as_str());
        let hits: Vec<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].name == label)
            .collect();
        if let Some(i) = pick(Strategy::AriaLabel, &hits)? {
            return fp(&nodes[i], Strategy::AriaLabel, i);
        }
    }
    if loc.css.is_some() {
        tried.push(Strategy::Css.as_str());
    }
    Err(Drift::NotFound { tried })
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_api::snapshot::{Rect, TreeQuality, WindowInfo};

    fn snap(nodes: Vec<UiNode>) -> Snapshot {
        Snapshot {
            window: WindowInfo {
                id: 1,
                title: "w".into(),
                pid: 1,
                bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                focused: true,
            },
            nodes,
            generation: 1,
            quality: TreeQuality::Good,
        }
    }

    fn at(i: u32, role: &str, name: &str, x: f64, y: f64) -> UiNode {
        UiNode::new(
            i,
            role,
            name,
            None,
            Some(Rect {
                x: x - 10.0,
                y: y - 10.0,
                width: 20.0,
                height: 20.0,
            }),
            vec![],
        )
    }

    fn loc(role: &str, name: &str) -> DesktopLocator {
        DesktopLocator {
            role: role.into(),
            name: name.into(),
            fingerprint: fingerprint_hex(role, name),
            automation_id: None,
            class_name: None,
            nth: None,
            position: None,
        }
    }

    #[test]
    fn exact_match_resolves() {
        let s = snap(vec![
            at(0, "button", "OK", 5.0, 5.0),
            at(1, "button", "Cancel", 50.0, 5.0),
        ]);
        let f = resolve_desktop(&loc("button", "Cancel"), &s, false).unwrap();
        assert_eq!((f.node, f.strategy), (1, Strategy::RoleNameExact));
    }

    #[test]
    fn two_exact_matches_are_ambiguous_unless_nth() {
        let s = snap(vec![
            at(0, "button", "OK", 5.0, 5.0),
            at(1, "button", "OK", 50.0, 5.0),
        ]);
        let e = resolve_desktop(&loc("button", "OK"), &s, false).unwrap_err();
        assert_eq!(
            e,
            Drift::Ambiguous {
                strategy: Strategy::RoleNameExact,
                count: 2
            }
        );
        let mut l = loc("button", "OK");
        l.nth = Some(1);
        assert_eq!(resolve_desktop(&l, &s, false).unwrap().node, 1);
    }

    #[test]
    fn fuzzy_accepts_near_identical_names_only() {
        let s = snap(vec![at(0, "button", "Save changes", 5.0, 5.0)]);
        let f = resolve_desktop(&loc("button", "Save  Changes!"), &s, false).unwrap();
        assert_eq!(f.strategy, Strategy::RoleNameFuzzy);
        assert!(f.fingerprint_changed);
        assert!(resolve_desktop(&loc("button", "Save changes"), &s, true).is_ok());
        assert!(resolve_desktop(&loc("button", "Save  Changes!"), &s, true).is_err());
        let s2 = snap(vec![at(0, "button", "Send", 5.0, 5.0)]);
        assert!(resolve_desktop(&loc("button", "Submit"), &s2, false).is_err());
    }

    #[test]
    fn position_needs_a_similar_name_unless_recorded_unnamed() {
        let mut l = loc("button", "Submit");
        l.position = Some(crate::format::PositionHint {
            x: 100.0,
            y: 100.0,
            role_ordinal: 0,
            role_count: 1,
        });
        let renamed = snap(vec![at(0, "button", "Send", 100.0, 100.0)]);
        assert!(resolve_desktop(&l, &renamed, false).is_err());
        let mut unnamed = loc("button", "");
        unnamed.position = l.position.clone();
        let s = snap(vec![at(0, "button", "Anything", 104.0, 98.0)]);
        let f = resolve_desktop(&unnamed, &s, false).unwrap();
        assert_eq!(f.strategy, Strategy::Position);
    }

    #[test]
    fn browser_snapshot_lines_parse() {
        let page = parse_browser_snapshot(
            "# Shop (https://shop.test/cart)\n  [e1] button \"Say \\\"hi\\\"\" [disabled]\n  heading \"x\"\n    [e22] link \"Home\"\n",
        );
        assert_eq!(page.url.as_deref(), Some("https://shop.test/cart"));
        assert_eq!(page.nodes.len(), 2);
        assert_eq!(page.nodes[0].name, "Say \"hi\"");
        assert_eq!(page.nodes[1].ref_id, "e22");
    }

    #[test]
    fn browser_strategies_and_ambiguity() {
        let n = |r: &str, role: &str, name: &str| BrowserNode {
            ref_id: r.into(),
            role: role.into(),
            name: name.into(),
        };
        let nodes = vec![n("e1", "link", "Buy"), n("e2", "button", "Buy")];
        let mut l = BrowserLocator {
            role: "button".into(),
            name: "Buy".into(),
            fingerprint: fingerprint_hex("button", "Buy"),
            aria_label: None,
            css: None,
            nth: None,
        };
        assert_eq!(resolve_browser(&l, &nodes, false).unwrap().node, 1);
        // Role changed: falls to text-exact, which is now ambiguous (two "Buy").
        l.role = "menuitem".into();
        assert!(matches!(
            resolve_browser(&l, &nodes, false),
            Err(Drift::Ambiguous { .. })
        ));
        let only = vec![n("e9", "link", "Buy")];
        let f = resolve_browser(&l, &only, false).unwrap();
        assert_eq!(f.strategy, Strategy::TextExact);
        l.aria_label = Some("Purchase".into());
        l.name = "gone".into();
        let labelled = vec![n("e5", "button", "Purchase")];
        assert_eq!(
            resolve_browser(&l, &labelled, false).unwrap().strategy,
            Strategy::AriaLabel
        );
    }

    #[test]
    fn glob_works() {
        assert!(glob_match("*Notepad", "*Untitled - Notepad"));
        assert!(glob_match("https://a.test/*", "https://A.test/x/y"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("*Notepad", "Notepad++ x"));
    }
}
