// Adapted from vercel-labs/agent-browser (Apache-2.0): AX tree -> compact, ref-indexed text.
//! Page snapshots: accessibility tree + cursor-interactive elements rendered as `[e12] button "Sign in"`.
//!
//! The builder in this module is a pure function of recorded CDP data ([`FrameInput`]),
//! so it can be tested without a browser.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::Value;

/// Roles that receive a ref because an agent can act on them.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "treeitem",
    "DisclosureTriangle",
    "PopUpButton",
    "TextField",
];

/// Roles that are always transparent (their children are hoisted).
const TRANSPARENT_ROLES: &[&str] = &[
    "none",
    "generic",
    "presentation",
    "RootWebArea",
    "WebArea",
    "InlineTextBox",
    "LineBreak",
    "GenericContainer",
];

/// Roles kept in compact mode even when unnamed.
const ALWAYS_KEEP: &[&str] = &["heading", "img", "separator"];

/// Roles whose accessible name is just their descendants' text; dropped in compact mode.
const NAME_FROM_CONTENT: &[&str] = &[
    "LayoutTable",
    "LayoutTableRow",
    "LayoutTableCell",
    "cell",
    "gridcell",
    "columnheader",
    "rowheader",
    "row",
    "listitem",
    "paragraph",
    "term",
    "definition",
];

const MAX_TEXT: usize = 160;
const MASK: &str = "[masked]";

/// Options for [`crate::Page::snapshot`] and [`build_snapshot`].
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    /// Only list elements that carry a ref, as a flat list.
    pub interactive_only: bool,
    /// Drop empty/unnamed structural nodes and text that merely repeats its parent's name.
    pub compact: bool,
    /// Maximum tree depth to render (`None` = unlimited).
    pub max_depth: Option<usize>,
}

/// Extra DOM facts for one element, gathered with `DOMSnapshot.captureSnapshot`.
#[derive(Debug, Clone, Default)]
pub struct DomExtra {
    /// Backend node id (matches AX `backendDOMNodeId`).
    pub backend_id: i64,
    /// Frame the element lives in.
    pub frame_id: Option<String>,
    /// Lower-case tag name.
    pub tag: String,
    /// Computed `cursor` is `pointer` (and the parent's is not).
    pub cursor_pointer: bool,
    /// Has an `onclick` attribute.
    pub onclick: bool,
    /// `tabindex` attribute when numeric and non-negative.
    pub tabindex: Option<i32>,
    /// `type` attribute of inputs.
    pub input_type: Option<String>,
    /// `autocomplete` attribute.
    pub autocomplete: Option<String>,
    /// Visible text / label for clickable candidates.
    pub text: String,
}

impl DomExtra {
    /// Whether the element looks clickable although it may have no interactive AX role.
    pub fn is_cursor_interactive(&self) -> bool {
        !matches!(self.tag.as_str(), "html" | "body")
            && (self.cursor_pointer || self.onclick || self.tabindex.is_some())
    }

    /// Whether the value of this element must never be shown.
    pub fn is_sensitive(&self) -> bool {
        self.input_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("password"))
            || self.autocomplete.as_deref().is_some_and(|a| {
                let a = a.to_ascii_lowercase();
                a.contains("password") || a.contains("cc-") || a.contains("one-time-code")
            })
    }
}

/// Everything known about one frame (document), in CDP terms.
#[derive(Debug, Clone, Default)]
pub struct FrameInput {
    /// CDP frame id.
    pub frame_id: String,
    /// Parent frame id (`None` for the main frame).
    pub parent_frame_id: Option<String>,
    /// Backend node id of the owning `<iframe>` element inside the parent frame's session.
    pub owner_backend_id: Option<i64>,
    /// Flattened session that serves this frame (`None` = the page's own session).
    pub session_id: Option<String>,
    /// Loader id: identifies the current document of the frame.
    pub loader_id: Option<String>,
    /// Raw `Accessibility.getFullAXTree` nodes.
    pub ax_nodes: Vec<Value>,
    /// DOM facts for inputs and cursor-interactive candidates.
    pub extras: Vec<DomExtra>,
    /// True when the DOM snapshot for this frame could not be captured. Without it sensitive
    /// inputs cannot be recognised, so every text-entry value is masked (fail closed).
    pub extras_failed: bool,
}

/// What a ref points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    /// Session to address CDP commands to (`None` = the page's session).
    pub session_id: Option<String>,
    /// Backend node id.
    pub backend_id: i64,
    /// Frame the node lives in.
    pub frame_id: String,
    /// Role as displayed.
    pub role: String,
    /// Accessible name.
    pub name: String,
}

type RefKey = (Option<String>, String, i64);

/// Issues stable refs: the same node of the same document keeps its ref across snapshots.
/// Counters never restart, so a ref from before a navigation can never alias a new element.
#[derive(Debug, Default)]
pub struct RefMap {
    next: u64,
    epoch: u64,
    prev_keys: HashMap<RefKey, u64>,
    keys: HashMap<RefKey, u64>,
    entries: HashMap<String, RefEntry>,
}

impl RefMap {
    /// Empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Strip `@`, `[`, `]` decorations: `@e3`, `[e3]` and `e3` are the same ref.
    pub fn normalize(r: &str) -> &str {
        r.trim().trim_start_matches('@').trim_matches(['[', ']'])
    }

    /// Look up a ref issued by the latest snapshot.
    pub fn get(&self, r: &str) -> Option<&RefEntry> {
        self.entries.get(Self::normalize(r))
    }

    /// Number of live refs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no refs are live.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every ref (e.g. after navigation).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.keys.clear();
        self.prev_keys.clear();
    }

    fn begin(&mut self) {
        self.epoch += 1;
        self.prev_keys = std::mem::take(&mut self.keys);
        self.entries.clear();
    }

    fn assign(&mut self, loader: Option<&str>, entry: RefEntry) -> String {
        // Unknown document identity must break continuity: scope it to this snapshot.
        let loader = loader.map_or_else(|| format!("?{}", self.epoch), str::to_string);
        let key = (entry.session_id.clone(), loader, entry.backend_id);
        let n = if let Some(n) = self.prev_keys.get(&key).or_else(|| self.keys.get(&key)) {
            *n
        } else {
            self.next += 1;
            self.next
        };
        self.keys.insert(key, n);
        let id = format!("e{n}");
        self.entries.insert(id.clone(), entry);
        id
    }
}

/// One rendered line of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapNode {
    /// Indentation level.
    pub depth: usize,
    /// Ref (`e12`) when the element is actionable.
    pub ref_id: Option<String>,
    /// Role (`button`, `link`, `text`, `clickable`, ...).
    pub role: String,
    /// Accessible name or text.
    pub name: String,
    /// Current value (masked for sensitive inputs).
    pub value: Option<String>,
    /// State flags such as `checked`, `disabled`, `level=2`.
    pub states: Vec<String>,
}

impl SnapNode {
    /// Render as `  [e12] button "Sign in" value="x" [disabled]`.
    pub fn line(&self) -> String {
        let mut s = "  ".repeat(self.depth);
        if let Some(r) = &self.ref_id {
            let _ = write!(s, "[{r}] ");
        }
        s.push_str(&self.role);
        if !self.name.is_empty() {
            let _ = write!(s, " \"{}\"", clean(&self.name));
        }
        if let Some(v) = &self.value {
            let _ = write!(s, " value=\"{}\"", clean(v));
        }
        if !self.states.is_empty() {
            let _ = write!(s, " [{}]", self.states.join(", "));
        }
        s
    }
}

fn clean(s: &str) -> String {
    let flat: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "\\\"");
    if flat.chars().count() > MAX_TEXT {
        let cut: String = flat.chars().take(MAX_TEXT).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// A rendered view of the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Page URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Tree lines.
    pub nodes: Vec<SnapNode>,
}

impl Snapshot {
    /// The tree as text, one line per node.
    pub fn to_text(&self) -> String {
        self.nodes
            .iter()
            .map(SnapNode::line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `# title (url)` line followed by the tree.
    pub fn render(&self) -> String {
        format!("# {} ({})\n{}", self.title, self.url, self.to_text())
    }

    /// Number of nodes carrying a ref.
    pub fn ref_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.ref_id.is_some()).count()
    }
}

// ---------------------------------------------------------------------------------------------
// AX parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct AxNode {
    ignored: bool,
    role: String,
    name: String,
    value: Option<String>,
    states: Vec<String>,
    backend: Option<i64>,
    children: Vec<usize>,
}

fn ax_str(v: Option<&Value>) -> Option<String> {
    let v = v?.get("value")?;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn ax_states(props: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    for p in props.and_then(Value::as_array).into_iter().flatten() {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let val = ax_str(p.get("value")).unwrap_or_default();
        match (name, val.as_str()) {
            ("checked", "true") => out.push("checked".into()),
            ("checked", "mixed") => out.push("mixed".into()),
            ("expanded", "true") => out.push("expanded".into()),
            ("expanded", "false") => out.push("collapsed".into()),
            ("level", v) if !v.is_empty() => out.push(format!("level={v}")),
            ("disabled" | "selected" | "pressed" | "required" | "readonly" | "focused", "true") => {
                out.push(name.to_string());
            }
            ("invalid", v) if v != "false" && !v.is_empty() => out.push("invalid".into()),
            _ => {}
        }
    }
    out
}

/// Parse raw AX nodes into an arena; returns `(nodes, roots)`.
fn parse_ax(raw: &[Value]) -> (Vec<AxNode>, Vec<usize>) {
    let index: HashMap<String, usize> = raw
        .iter()
        .enumerate()
        .filter_map(|(i, n)| Some((n.get("nodeId")?.as_str()?.to_string(), i)))
        .collect();
    let mut has_parent = vec![false; raw.len()];
    let mut nodes = Vec::with_capacity(raw.len());
    for n in raw {
        let children: Vec<usize> = n
            .get("childIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| index.get(c.as_str()?).copied())
            .collect();
        nodes.push(AxNode {
            ignored: n.get("ignored").and_then(Value::as_bool).unwrap_or(false),
            role: ax_str(n.get("role")).unwrap_or_default(),
            name: ax_str(n.get("name")).unwrap_or_default(),
            value: ax_str(n.get("value")).filter(|v| !v.is_empty()),
            states: ax_states(n.get("properties")),
            backend: n.get("backendDOMNodeId").and_then(Value::as_i64),
            children,
        });
    }
    for n in &nodes {
        for &c in &n.children {
            has_parent[c] = true;
        }
    }
    let roots = (0..nodes.len()).filter(|&i| !has_parent[i]).collect();
    (nodes, roots)
}

// ---------------------------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------------------------

struct Parsed {
    nodes: Vec<AxNode>,
    roots: Vec<usize>,
    extras: HashMap<i64, usize>,
    ax_backends: HashSet<i64>,
}

struct Builder<'a> {
    frames: &'a [FrameInput],
    parsed: Vec<Parsed>,
    opts: &'a SnapshotOptions,
    refs: &'a mut RefMap,
    out: Vec<SnapNode>,
    truncated: bool,
}

/// Maximum AX nesting (hidden levels included) followed before the walk is cut off. Guards the
/// worker stack against hostile pages that nest thousands of elements.
const MAX_NEST: usize = 256;

/// Roles whose value is masked when sensitivity information is unavailable.
const FREE_TEXT_ROLES: &[&str] = &["textbox", "searchbox", "combobox", "spinbutton"];

/// Build the snapshot lines for a page. `frames[0]` must be the main frame; iframes are spliced
/// under the `Iframe` AX node whose backend id equals the child's `owner_backend_id`.
pub fn build_snapshot(
    frames: &[FrameInput],
    opts: &SnapshotOptions,
    refs: &mut RefMap,
) -> Vec<SnapNode> {
    refs.begin();
    let parsed = frames
        .iter()
        .map(|f| {
            let (nodes, roots) = parse_ax(&f.ax_nodes);
            let ax_backends = nodes.iter().filter_map(|n| n.backend).collect();
            let extras = f
                .extras
                .iter()
                .enumerate()
                .map(|(i, e)| (e.backend_id, i))
                .collect();
            Parsed {
                nodes,
                roots,
                extras,
                ax_backends,
            }
        })
        .collect();
    let mut b = Builder {
        frames,
        parsed,
        opts,
        refs,
        out: Vec::new(),
        truncated: false,
    };
    if !frames.is_empty() {
        b.emit_frame(0, 0, 0);
    }
    if b.truncated {
        b.out.push(SnapNode {
            depth: 0,
            ref_id: None,
            role: "text".to_string(),
            name: "[snapshot truncated: nesting too deep]".to_string(),
            value: None,
            states: Vec::new(),
        });
    }
    let mut out = b.out;
    if opts.compact && !opts.interactive_only {
        prune_empty(&mut out);
    }
    out
}

impl Builder<'_> {
    fn depth_ok(&self, depth: usize) -> bool {
        self.opts.interactive_only || self.opts.max_depth.is_none_or(|m| depth <= m)
    }

    fn emit_frame(&mut self, f: usize, depth: usize, nest: usize) {
        let roots = self.parsed[f].roots.clone();
        for r in roots {
            self.emit_node(f, r, depth, "", nest);
        }
        // Cursor-interactive elements the AX tree does not know about.
        let frame = &self.frames[f];
        let extras: Vec<DomExtra> = frame
            .extras
            .iter()
            .filter(|e| {
                e.is_cursor_interactive()
                    && !e.text.trim().is_empty()
                    && !self.parsed[f].ax_backends.contains(&e.backend_id)
                    && e.frame_id.as_deref().is_none_or(|id| id == frame.frame_id)
            })
            .cloned()
            .collect();
        for e in extras {
            let d = if self.opts.interactive_only { 0 } else { depth };
            if self.depth_ok(d) {
                self.push(
                    f,
                    d,
                    "clickable",
                    &e.text,
                    e.backend_id,
                    None,
                    Vec::new(),
                    true,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        f: usize,
        depth: usize,
        role: &str,
        name: &str,
        backend: i64,
        value: Option<String>,
        states: Vec<String>,
        with_ref: bool,
    ) {
        let frame = &self.frames[f];
        let ref_id = with_ref.then(|| {
            self.refs.assign(
                frame.loader_id.as_deref(),
                RefEntry {
                    session_id: frame.session_id.clone(),
                    backend_id: backend,
                    frame_id: frame.frame_id.clone(),
                    role: role.to_string(),
                    name: name.to_string(),
                },
            )
        });
        self.out.push(SnapNode {
            depth,
            ref_id,
            role: role.to_string(),
            name: name.to_string(),
            value,
            states,
        });
    }

    fn emit_node(&mut self, f: usize, n: usize, depth: usize, parent_name: &str, nest: usize) {
        if nest > MAX_NEST {
            self.truncated = true;
            return;
        }
        let node = self.parsed[f].nodes[n].clone();
        let extra: Option<DomExtra> = node
            .backend
            .and_then(|b| self.parsed[f].extras.get(&b))
            .map(|&i| self.frames[f].extras[i].clone());
        let role = node.role.as_str();
        let interactive = INTERACTIVE_ROLES.contains(&role);
        let label = if node.name.trim().is_empty() {
            extra.as_ref().map_or("", |e| e.text.as_str()).to_string()
        } else {
            node.name.clone()
        };
        let clickable = !interactive
            && !node.ignored
            && role != "StaticText"
            && extra
                .as_ref()
                .is_some_and(|e| e.is_cursor_interactive() && !label.trim().is_empty());

        let label = if self.opts.compact
            && !interactive
            && !clickable
            && NAME_FROM_CONTENT.contains(&role)
            && !node.children.is_empty()
        {
            String::new()
        } else {
            label
        };

        // Transparent nodes: hoist children.
        if node.ignored || (TRANSPARENT_ROLES.contains(&role) && !clickable) {
            for c in node.children {
                self.emit_node(f, c, depth, parent_name, nest + 1);
            }
            return;
        }
        let nd = if self.opts.interactive_only { 0 } else { depth };
        if !self.depth_ok(depth) {
            return;
        }

        if role == "StaticText" {
            let t = node.name.trim();
            let repeats_parent = self.opts.compact && t == parent_name.trim();
            if !(self.opts.interactive_only || t.is_empty() || repeats_parent) {
                self.push(f, nd, "text", t, 0, None, Vec::new(), false);
            }
            return;
        }

        let is_iframe = role == "Iframe" || role == "iframe";
        let unnamed_container = !interactive
            && !clickable
            && !is_iframe
            && label.trim().is_empty()
            && !ALWAYS_KEEP.contains(&role);
        let hidden_by_mode = self.opts.interactive_only && !(interactive || clickable);
        let skip_line = hidden_by_mode
            || (self.opts.compact && unnamed_container)
            || is_iframe && self.opts.interactive_only;

        // Mask sensitive values.
        let mut value = node.value.clone();
        let unknown_sensitivity = self.frames[f].extras_failed && FREE_TEXT_ROLES.contains(&role);
        if (extra.as_ref().is_some_and(DomExtra::is_sensitive) || unknown_sensitivity)
            && value.is_some()
        {
            value = Some(MASK.to_string());
        }

        let child_depth = if skip_line { depth } else { depth + 1 };
        if !skip_line {
            let display_role = if clickable && TRANSPARENT_ROLES.contains(&role) {
                "clickable"
            } else {
                role
            };
            self.push(
                f,
                nd,
                display_role,
                &label,
                node.backend.unwrap_or(0),
                value,
                node.states.clone(),
                interactive || clickable,
            );
        }
        // Interactive leaves with the same text children would only repeat the name.
        let parent = if skip_line {
            parent_name
        } else {
            label.as_str()
        }
        .to_string();
        for c in node.children {
            self.emit_node(f, c, child_depth, &parent, nest + 1);
        }
        if is_iframe && let Some(b) = node.backend {
            let child = self.frames.iter().position(|c| {
                c.parent_frame_id.as_deref() == Some(self.frames[f].frame_id.as_str())
                    && c.owner_backend_id == Some(b)
            });
            if let Some(cf) = child {
                self.emit_frame(cf, child_depth, nest + 1);
            }
        }
    }
}

/// Compact mode: remove container lines that ended up with no content beneath them.
fn prune_empty(out: &mut Vec<SnapNode>) {
    let is_leaf_worthy = |n: &SnapNode| {
        n.ref_id.is_some()
            || n.role == "text"
            || n.role == "img"
            || n.role == "heading"
            || n.role == "separator"
    };
    let mut keep = vec![true; out.len()];
    for i in (0..out.len()).rev() {
        if is_leaf_worthy(&out[i]) {
            continue;
        }
        let has_child = out
            .get(i + 1)
            .is_some_and(|next| keep[i + 1] && next.depth > out[i].depth);
        if !has_child && out[i].role != "iframe" && out[i].role != "Iframe" {
            keep[i] = false;
        }
    }
    let mut it = keep.iter();
    out.retain(|_| *it.next().unwrap_or(&true));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXAMPLE: &str = include_str!("../tests/fixtures/example_com.json");
    const LOGIN: &str = include_str!("../tests/fixtures/login_form.json");
    const IFRAME_CHILD: &str = include_str!("../tests/fixtures/iframe_child.json");

    fn nodes(s: &str) -> Vec<Value> {
        serde_json::from_str::<Value>(s).unwrap()["nodes"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn main_frame(ax: &str) -> FrameInput {
        FrameInput {
            frame_id: "main".into(),
            loader_id: Some("L1".into()),
            ax_nodes: nodes(ax),
            ..Default::default()
        }
    }

    fn text(frames: &[FrameInput], opts: &SnapshotOptions, refs: &mut RefMap) -> String {
        build_snapshot(frames, opts, refs)
            .iter()
            .map(SnapNode::line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn example_com_full_tree() {
        let mut refs = RefMap::new();
        let t = text(
            &[main_frame(EXAMPLE)],
            &SnapshotOptions::default(),
            &mut refs,
        );
        assert!(t.contains("heading \"Example Domain\" [level=1]"), "{t}");
        assert!(t.contains("[e1] link \"Learn more\""), "{t}");
        assert_eq!(refs.len(), 1);
        let e = refs.get("@e1").unwrap();
        assert_eq!(e.backend_id, 20);
        assert_eq!(e.role, "link");
    }

    #[test]
    fn example_com_compact_drops_repeated_text() {
        let mut refs = RefMap::new();
        let opts = SnapshotOptions {
            compact: true,
            ..Default::default()
        };
        let t = text(&[main_frame(EXAMPLE)], &opts, &mut refs);
        // Link text child equals the link name and is dropped; heading text likewise.
        assert!(!t.contains("text \"Learn more\""), "{t}");
        assert!(!t.contains("text \"Example Domain\""), "{t}");
        assert!(t.contains("text \"This domain is for use"), "{t}");
        assert!(!t.contains("InlineTextBox"));
    }

    #[test]
    fn interactive_only_is_flat() {
        let mut refs = RefMap::new();
        let opts = SnapshotOptions {
            interactive_only: true,
            compact: true,
            ..Default::default()
        };
        let t = text(&[main_frame(LOGIN)], &opts, &mut refs);
        let lines: Vec<_> = t.lines().collect();
        assert_eq!(lines.len(), 5, "{t}");
        assert!(lines.iter().all(|l| l.starts_with("[e")), "{t}");
        assert!(t.contains("checkbox \"Remember me\" [checked]"), "{t}");
        assert!(t.contains("button \"Sign in\" [disabled]"), "{t}");
    }

    #[test]
    fn sensitive_values_are_masked() {
        let mut f = main_frame(LOGIN);
        f.extras.push(DomExtra {
            backend_id: 12,
            tag: "input".into(),
            input_type: Some("password".into()),
            ..Default::default()
        });
        let mut refs = RefMap::new();
        let t = text(&[f], &SnapshotOptions::default(), &mut refs);
        assert!(t.contains("textbox \"Password\" value=\"[masked]\""), "{t}");
        assert!(!t.contains("hunter2"), "{t}");
        // A normal field keeps its value.
        assert!(t.contains("value=\"alice@example.com\""), "{t}");
    }

    #[test]
    fn refs_are_stable_and_invalidated_on_new_document() {
        let mut refs = RefMap::new();
        let opts = SnapshotOptions::default();
        let a = text(&[main_frame(LOGIN)], &opts, &mut refs);
        let b = text(&[main_frame(LOGIN)], &opts, &mut refs);
        assert_eq!(a, b, "same document keeps refs");
        let old = refs.get("e1").cloned().unwrap();

        // Navigation: new loader id for the same backend ids -> new refs, old ones dead.
        let mut nav = main_frame(LOGIN);
        nav.loader_id = Some("L2".into());
        let c = text(&[nav], &opts, &mut refs);
        assert_ne!(a, c);
        assert!(refs.get("e1").is_none(), "stale ref must not resolve");
        assert!(refs.get("e6").is_some(), "{c}");
        assert_ne!(refs.get("e6").unwrap().backend_id, 0);
        assert_eq!(old.backend_id, refs.get("e6").unwrap().backend_id);
    }

    #[test]
    fn unknown_loader_never_shares_refs() {
        let mut refs = RefMap::new();
        let mut f = main_frame(LOGIN);
        f.loader_id = None;
        let a = text(
            std::slice::from_ref(&f),
            &SnapshotOptions::default(),
            &mut refs,
        );
        let b = text(
            std::slice::from_ref(&f),
            &SnapshotOptions::default(),
            &mut refs,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn cursor_interactive_elements_are_found() {
        let mut f = main_frame(EXAMPLE);
        // Promote an existing generic AX node and add one the AX tree does not contain.
        f.extras.push(DomExtra {
            backend_id: 11,
            tag: "div".into(),
            cursor_pointer: true,
            text: "Menu toggle".into(),
            ..Default::default()
        });
        f.extras.push(DomExtra {
            backend_id: 999,
            frame_id: Some("main".into()),
            tag: "span".into(),
            onclick: true,
            text: "Hidden from AX".into(),
            ..Default::default()
        });
        let mut refs = RefMap::new();
        let t = text(&[f], &SnapshotOptions::default(), &mut refs);
        assert!(t.contains("clickable \"Menu toggle\""), "{t}");
        assert!(t.contains("clickable \"Hidden from AX\""), "{t}");
        assert_eq!(refs.len(), 3);
    }

    #[test]
    fn iframe_content_is_spliced_under_owner() {
        let main = FrameInput {
            frame_id: "main".into(),
            loader_id: Some("L1".into()),
            ax_nodes: nodes(EXAMPLE),
            ..Default::default()
        };
        let child = FrameInput {
            frame_id: "child".into(),
            parent_frame_id: Some("main".into()),
            owner_backend_id: Some(30),
            session_id: Some("OOPIF-1".into()),
            loader_id: Some("C1".into()),
            ax_nodes: nodes(IFRAME_CHILD),
            ..Default::default()
        };
        let mut refs = RefMap::new();
        let t = text(&[main, child], &SnapshotOptions::default(), &mut refs);
        let lines: Vec<_> = t.lines().collect();
        let i = lines.iter().position(|l| l.contains("Iframe")).unwrap();
        assert!(
            lines[i + 1].starts_with("  ") && lines[i + 1].contains("button \"Pay\""),
            "{t}"
        );
        let pay = refs
            .get("e2")
            .unwrap_or_else(|| panic!("pay ref missing: {t}"));
        assert_eq!(pay.session_id.as_deref(), Some("OOPIF-1"));
        assert_eq!(pay.frame_id, "child");
    }

    #[test]
    fn max_depth_limits_tree() {
        let mut refs = RefMap::new();
        let opts = SnapshotOptions {
            max_depth: Some(0),
            ..Default::default()
        };
        let shallow = text(&[main_frame(EXAMPLE)], &opts, &mut refs);
        let full = text(
            &[main_frame(EXAMPLE)],
            &SnapshotOptions::default(),
            &mut refs,
        );
        assert!(shallow.lines().count() < full.lines().count());
    }

    #[test]
    fn normalize_ref_forms() {
        assert_eq!(RefMap::normalize("@e3"), "e3");
        assert_eq!(RefMap::normalize("[e3]"), "e3");
        assert_eq!(RefMap::normalize(" e3 "), "e3");
    }

    fn textbox_frame(value: &str, failed: bool) -> FrameInput {
        let ax = json!([
            {"nodeId":"1","role":{"value":"RootWebArea"},"childIds":["2"],"backendDOMNodeId":1},
            {"nodeId":"2","parentId":"1","role":{"value":"textbox"},"name":{"value":"Card"},
             "value":{"value":value},"childIds":[],"backendDOMNodeId":2}
        ]);
        FrameInput {
            frame_id: "main".into(),
            loader_id: Some("L1".into()),
            ax_nodes: ax.as_array().unwrap().clone(),
            extras_failed: failed,
            ..Default::default()
        }
    }

    #[test]
    fn masks_text_values_when_dom_snapshot_failed() {
        let mut refs = RefMap::new();
        let t = text(
            &[textbox_frame("4111111111111111", true)],
            &SnapshotOptions::default(),
            &mut refs,
        );
        assert!(!t.contains("4111"), "{t}");
        assert!(t.contains("[masked]"), "{t}");
        let t = text(
            &[textbox_frame("hello", false)],
            &SnapshotOptions::default(),
            &mut RefMap::new(),
        );
        assert!(t.contains("hello"), "{t}");
    }

    #[test]
    fn deep_nesting_is_truncated_without_overflow() {
        let depth = 20_000;
        let mut ax = vec![
            json!({"nodeId":"0","role":{"value":"generic"},"ignored":true,
            "childIds":["1"],"backendDOMNodeId":1}),
        ];
        for i in 1..depth {
            ax.push(
                json!({"nodeId":i.to_string(),"role":{"value":"generic"},"ignored":true,
                "childIds":[(i+1).to_string()],"backendDOMNodeId":i+1}),
            );
        }
        ax.push(
            json!({"nodeId":depth.to_string(),"role":{"value":"button"},"name":{"value":"deep"},
            "childIds":[],"backendDOMNodeId":depth+1}),
        );
        let frame = FrameInput {
            frame_id: "main".into(),
            loader_id: Some("L1".into()),
            ax_nodes: ax,
            ..Default::default()
        };
        // Run on a small stack like the real worker to prove no overflow.
        let t = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || text(&[frame], &SnapshotOptions::default(), &mut RefMap::new()))
            .unwrap()
            .join()
            .expect("no stack overflow");
        assert!(t.contains("truncated"), "{t}");
    }
}
