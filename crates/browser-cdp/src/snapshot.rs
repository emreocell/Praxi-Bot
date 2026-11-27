// Adapted from vercel-labs/agent-browser (Apache-2.0): AX tree -> compact, ref-indexed text.
//! Page snapshots: accessibility tree + cursor-interactive elements rendered as `[e12] button "Sign in"`.
//!
//! The builder in this module is a pure function of recorded CDP data ([`FrameInput`]),
//! so it can be tested without a browser.

use std::borrow::Cow;
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
pub(crate) const MASK: &str = "[masked]";

/// Options for [`crate::Page::snapshot`] and [`build_snapshot`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// `name`, `id`, `aria-label` and `placeholder` attributes (page-chosen names that often reveal a secret field).
    pub name_hints: Vec<String>,
    /// The element takes text without being an `<input>` / `<textarea>`: `contenteditable` or a
    /// `textbox` / `searchbox` / `combobox` role. Its name hints count like an input's.
    pub editable: bool,
}

/// Words in a field's name / id / aria-label that mark it as holding a secret.
const SECRET_NAME_WORDS: &[&str] = &[
    "password",
    "passwd",
    "one-time",
    "one_time",
    "onetime",
    "cvv",
    "cvc",
    "card number",
    "iban",
];

/// Suffixes that make `hotp<suffix>` a one-time-code name (`hotpcode`) and not a word like `hotpot`.
const OTP_SUFFIXES: &[&str] = &["code", "input", "token", "field", "value", "key", "pin"];

/// Whether `h` names a one-time-code field: `otp` / `totp` / `hotp` as a word of its own
/// (`user_otp`, `mfaOtp`, `one-time-otp`), or glued to one end of an identifier segment
/// (`otpcode`, `otpinput`, `smsotp`, `emailotp`, `totpcode`). `footprint` and `hotpot` do not match.
/// Rare false positives are fine: they only make a field count as sensitive.
fn has_otp_word(h: &str) -> bool {
    // Split at non-alphanumerics and at camelCase boundaries (`mfaOtp` -> `mfa`, `otp`).
    let mut segments: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in h.chars() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                segments.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase()) && !cur.is_empty()
        {
            segments.push(std::mem::take(&mut cur));
        }
        cur.push(c.to_ascii_lowercase());
        prev = Some(c);
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    // The unsplit lower-case segments too, so `smsOtp`-style names also match as one glued word.
    let raw: Vec<String> = h
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .collect();
    segments.iter().chain(raw.iter()).any(|s| {
        matches!(s.as_str(), "otp" | "totp" | "hotp")
            || s.starts_with("otp")
            || s.starts_with("totp")
            || s.ends_with("otp")
            || s.ends_with("totp")
            || s.ends_with("hotp")
            || s.strip_prefix("hotp")
                .is_some_and(|rest| OTP_SUFFIXES.contains(&rest))
    })
}

/// Secret-field words matched on the normalised hint (lower-case, `-`, `_` and spaces removed),
/// so `card-number`, `Card_Number` and `cardnumber` are the same.
const SECRET_NORMALISED_WORDS: &[&str] = &[
    "cardnumber",
    "ccnumber",
    "cardnum",
    "securitycode",
    "verificationcode",
    "passcode",
];

/// Words that, glued to either side of `otp`, make it a one-time-code name (`emailotpcode`,
/// `verifyotpcode`, `enterotp`).
const OTP_NEIGHBOURS: &[&str] = &[
    "code",
    "pin",
    "token",
    "input",
    "number",
    "num",
    "digit",
    "field",
    "value",
    "key",
    "sms",
    "email",
    "mail",
    "verify",
    "verification",
    "enter",
    "mfa",
    "2fa",
    "auth",
    "login",
    "phone",
    "user",
    "confirm",
    "security",
];

/// Strip everything but ASCII letters and digits, lower-cased.
fn normalise_hint(h: &str) -> String {
    h.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `otp` somewhere inside the normalised identifier with a one-time-code neighbour: at either end,
/// before a digit, or next to a word from [`OTP_NEIGHBOURS`]. Catches names with `otp` in the
/// middle (`emailotpcode`, `enterotp1`) while `footprint`, `hotpot` and `spotpicker` stay plain.
fn has_otp_inside(n: &str) -> bool {
    n.match_indices("otp").any(|(i, _)| {
        let left = &n[..i];
        let right = &n[i + 3..];
        left.is_empty()
            || right.is_empty()
            || right.starts_with(|c: char| c.is_ascii_digit())
            || OTP_NEIGHBOURS
                .iter()
                .any(|w| left.ends_with(w) || right.starts_with(w))
    })
}

/// `mfa` / `2fa` as a whole segment (split on non-alphanumerics and camelCase), optionally glued to
/// a code-ish suffix (`mfa-token`, `2fa_code`, `mfaOtp`). Never as a substring of the stripped text:
/// `Confirm family name` would otherwise contain `mfa`, and a short hex id can contain `2fa`.
fn has_mfa_word(h: &str) -> bool {
    let mut segs: Vec<String> = Vec::new();
    let mut seg = String::new();
    let mut prev: Option<char> = None;
    for c in h.chars() {
        if !c.is_ascii_alphanumeric() {
            if !seg.is_empty() {
                segs.push(std::mem::take(&mut seg));
            }
            prev = None;
            continue;
        }
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase()) && !seg.is_empty()
        {
            segs.push(std::mem::take(&mut seg));
        }
        seg.push(c.to_ascii_lowercase());
        prev = Some(c);
    }
    if !seg.is_empty() {
        segs.push(seg);
    }
    segs.iter().any(|s| {
        ["mfa", "2fa"].iter().any(|w| {
            s == w
                || s.strip_prefix(w).is_some_and(|rest| {
                    matches!(rest, "code" | "token" | "input" | "field" | "pin")
                })
        })
    })
}

/// `pin` as a word of its own (`pin`, `user-pin`, `cardPin`) or `pin` + code/number/input/field.
/// A standalone `pin` also catches postal PIN code and map pin fields: accepted, it fails closed.
fn has_pin_word(h: &str, n: &str) -> bool {
    let mut seg = String::new();
    let mut prev: Option<char> = None;
    let mut segs: Vec<String> = Vec::new();
    for c in h.chars() {
        if !c.is_ascii_alphanumeric() {
            if !seg.is_empty() {
                segs.push(std::mem::take(&mut seg));
            }
            prev = None;
            continue;
        }
        if c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase()) && !seg.is_empty()
        {
            segs.push(std::mem::take(&mut seg));
        }
        seg.push(c.to_ascii_lowercase());
        prev = Some(c);
    }
    if !seg.is_empty() {
        segs.push(seg);
    }
    segs.iter().any(|s| s == "pin")
        || ["pincode", "pinnumber", "pininput", "pinfield", "pinentry"]
            .iter()
            .any(|w| n == *w || n.starts_with(w))
        || [
            "userpin",
            "cardpin",
            "bankpin",
            "simpin",
            "atmpin",
            "mypin",
            "newpin",
            "oldpin",
            "confirmpin",
            "securitypin",
            "accountpin",
        ]
        .iter()
        .any(|w| n.ends_with(w))
}

/// Whether a name / id / aria-label / placeholder hint marks a field as holding a secret.
pub fn name_hint_is_secret(h: &str) -> bool {
    let l = h.to_ascii_lowercase();
    let n = normalise_hint(h);
    SECRET_NAME_WORDS.iter().any(|w| l.contains(w))
        || SECRET_NORMALISED_WORDS.iter().any(|w| n.contains(w))
        || has_otp_word(h)
        || has_mfa_word(h)
        || has_otp_inside(&n)
        || has_pin_word(h, &n)
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
            || ((self.input_type.is_some() || self.editable)
                && self.name_hints.iter().any(|h| name_hint_is_secret(h)))
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
    /// Backend ids of elements and text a person looking at the page cannot see (opacity 0, tiny
    /// font, off-screen, same colour as the background, clipped away: see [`crate::hidden`]).
    /// They are left out of the snapshot and counted.
    pub hidden: HashSet<i64>,
    /// Backend ids of elements whose `title` / `alt` / `aria-label` is a long, instruction-like
    /// text: their accessible name is left out.
    pub suspicious_names: HashSet<i64>,
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
    /// The element holds a secret (password, one-time code, card number).
    pub sensitive: bool,
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
    /// `(session, backend node id)` of fields that hold a secret typed by the daemon:
    /// their value and the text the browser exposes under them are never rendered.
    masked: Vec<(Option<String>, i64)>,
}

impl RefMap {
    /// Fields whose content must not be rendered in the next snapshots (`None`: none).
    pub(crate) fn set_masked(&mut self, fields: Option<Vec<(Option<String>, i64)>>) {
        self.masked = fields.unwrap_or_default();
    }

    /// A typed secret is on the page: every text-entry value is masked (a page may mirror
    /// the value into sibling boxes or re-create the field) and short code-like text is dropped.
    fn secret_on_page(&self) -> bool {
        !self.masked.is_empty()
    }

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
        let mut s = String::with_capacity(self.depth * 2 + self.role.len() + self.name.len() + 16);
        for _ in 0..self.depth {
            s.push_str("  ");
        }
        if let Some(r) = &self.ref_id {
            let _ = write!(s, "[{r}] ");
        }
        s.push_str(&self.role);
        if !self.name.is_empty() {
            s.push_str(" \"");
            write_clean(&mut s, &self.name);
            s.push('"');
        }
        if let Some(v) = &self.value {
            s.push_str(" value=\"");
            write_clean(&mut s, v);
            s.push('"');
        }
        if !self.states.is_empty() {
            let _ = write!(s, " [{}]", self.states.join(", "));
        }
        s
    }
}

fn write_clean(out: &mut String, s: &str) {
    let mut count = 0usize;
    let mut push_ch = |ch: char| -> bool {
        if count == MAX_TEXT {
            out.push('…');
            return false;
        }
        out.push(ch);
        count += 1;
        true
    };
    let mut first = true;
    for word in s.split_whitespace() {
        if !first && !push_ch(' ') {
            return;
        }
        first = false;
        for ch in word.chars() {
            if ch == '"' {
                if !push_ch('\\') || !push_ch('"') {
                    return;
                }
            } else if !push_ch(ch) {
                return;
            }
        }
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
    /// Items (a hidden block of text, a hidden link, a long instruction-like `alt` / `title`)
    /// that were left out because a person looking at the page cannot see them.
    pub hidden_removed: usize,
}

impl Snapshot {
    /// The note that tells the reader some page content was left out, if any was.
    pub fn hidden_note(&self) -> Option<String> {
        (self.hidden_removed > 0).then(|| {
            format!(
                "[{} hidden item{} removed: content a person looking at this page cannot see]",
                self.hidden_removed,
                if self.hidden_removed == 1 { "" } else { "s" }
            )
        })
    }

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
        let mut out = format!("# {} ({})\n{}", self.title, self.url, self.to_text());
        if let Some(note) = self.hidden_note() {
            out.push('\n');
            out.push_str(&note);
        }
        out
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
struct AxNode<'a> {
    ignored: bool,
    role: Cow<'a, str>,
    name: Cow<'a, str>,
    value: Option<Cow<'a, str>>,
    states: Vec<String>,
    backend: Option<i64>,
    children: Vec<usize>,
}

fn ax_str(v: Option<&Value>) -> Option<Cow<'_, str>> {
    let v = v?.get("value")?;
    match v {
        Value::String(s) => Some(Cow::Borrowed(s.as_str())),
        Value::Number(n) => Some(Cow::Owned(n.to_string())),
        Value::Bool(b) => Some(Cow::Owned(b.to_string())),
        _ => None,
    }
}

fn ax_states(props: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    for p in props.and_then(Value::as_array).into_iter().flatten() {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let val = ax_str(p.get("value")).unwrap_or_default();
        match (name, val.as_ref()) {
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
fn parse_ax<'a>(
    raw: &'a [Value],
    extras: &HashMap<i64, usize>,
) -> (Vec<AxNode<'a>>, Vec<usize>, HashSet<i64>) {
    let mut index: HashMap<&'a str, usize> = HashMap::with_capacity(raw.len());
    for (i, n) in raw.iter().enumerate() {
        if let Some(id) = n.get("nodeId").and_then(Value::as_str) {
            index.insert(id, i);
        }
    }
    let mut has_parent = vec![false; raw.len()];
    let mut nodes = Vec::with_capacity(raw.len());
    let mut ax_backends = HashSet::new();
    for n in raw {
        let ignored = n.get("ignored").and_then(Value::as_bool).unwrap_or(false);
        let backend = n.get("backendDOMNodeId").and_then(Value::as_i64);
        if let Some(b) = backend {
            ax_backends.insert(b);
        }
        if ignored {
            let children: Vec<usize> = n
                .get("childIds")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| index.get(c.as_str()?).copied())
                .collect();
            nodes.push(AxNode {
                ignored: true,
                children,
                ..Default::default()
            });
            continue;
        }
        let role = ax_str(n.get("role")).unwrap_or_default();
        if role.as_ref() == "StaticText" {
            // StaticText only emits its own trimmed name and never visits children/states/value.
            nodes.push(AxNode {
                ignored: false,
                role,
                name: ax_str(n.get("name")).unwrap_or_default(),
                value: None,
                states: Vec::new(),
                backend,
                children: Vec::new(),
            });
            continue;
        }
        let children: Vec<usize> = n
            .get("childIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| index.get(c.as_str()?).copied())
            .collect();
        let transparent_no_extra = TRANSPARENT_ROLES.contains(&role.as_ref())
            && !backend.is_some_and(|b| extras.contains_key(&b));
        if transparent_no_extra {
            nodes.push(AxNode {
                ignored: false,
                role,
                name: Cow::Borrowed(""),
                value: None,
                states: Vec::new(),
                backend,
                children,
            });
            continue;
        }
        nodes.push(AxNode {
            ignored: false,
            role,
            name: ax_str(n.get("name")).unwrap_or_default(),
            value: ax_str(n.get("value")).filter(|v| !v.is_empty()),
            states: ax_states(n.get("properties")),
            backend,
            children,
        });
    }
    for n in &nodes {
        for &c in &n.children {
            has_parent[c] = true;
        }
    }
    let roots = (0..nodes.len()).filter(|&i| !has_parent[i]).collect();
    (nodes, roots, ax_backends)
}

// ---------------------------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------------------------

struct Parsed<'a> {
    nodes: Vec<AxNode<'a>>,
    roots: Vec<usize>,
    extras: HashMap<i64, usize>,
    ax_backends: HashSet<i64>,
}

struct SnapshotCtx<'a> {
    frames: &'a [FrameInput],
    parsed: Vec<Parsed<'a>>,
    opts: &'a SnapshotOptions,
}

struct Builder<'a, 'b> {
    ctx: &'b SnapshotCtx<'a>,
    refs: &'b mut RefMap,
    out: Vec<SnapNode>,
    truncated: bool,
    node_capped: bool,
    /// Whether the node being pushed holds a secret (set around `push`).
    cur_sensitive: bool,
    /// Hidden items left out so far.
    hidden_removed: usize,
}

/// Maximum AX nesting (hidden levels included) followed before the walk is cut off. Guards the
/// worker stack against hostile pages that nest thousands of elements.
const MAX_NEST: usize = 256;

/// Maximum emitted snapshot nodes per snapshot to guard against runaway DOMs.
const MAX_SNAPSHOT_NODES: usize = 25_000;

/// Text that could be (a copy of) a typed one-time code: a short run of letters and digits, or
/// digits split over several boxes. Dropped from snapshots while a typed secret is on the page.
fn looks_like_a_code(t: &str) -> bool {
    let n = t.chars().count();
    (4..=12).contains(&n) && t.chars().all(|c| c.is_ascii_alphanumeric())
        || (1..=12).contains(&n) && t.chars().all(|c| c.is_ascii_digit())
}

/// Roles whose value is masked when sensitivity information is unavailable.
const FREE_TEXT_ROLES: &[&str] = &["textbox", "searchbox", "combobox", "spinbutton"];

/// Build the snapshot lines for a page. `frames[0]` must be the main frame; iframes are spliced
/// under the `Iframe` AX node whose backend id equals the child's `owner_backend_id`.
pub fn build_snapshot(
    frames: &[FrameInput],
    opts: &SnapshotOptions,
    refs: &mut RefMap,
) -> Vec<SnapNode> {
    build_snapshot_counted(frames, opts, refs).0
}

/// [`build_snapshot`] plus the number of hidden items that were left out.
pub fn build_snapshot_counted(
    frames: &[FrameInput],
    opts: &SnapshotOptions,
    refs: &mut RefMap,
) -> (Vec<SnapNode>, usize) {
    refs.begin();
    let parsed = frames
        .iter()
        .map(|f| {
            let mut extras = HashMap::with_capacity(f.extras.len());
            for (i, e) in f.extras.iter().enumerate() {
                extras.insert(e.backend_id, i);
            }
            let (nodes, roots, ax_backends) = parse_ax(&f.ax_nodes, &extras);
            Parsed {
                nodes,
                roots,
                extras,
                ax_backends,
            }
        })
        .collect();
    let ctx = SnapshotCtx {
        frames,
        parsed,
        opts,
    };
    let mut b = Builder {
        ctx: &ctx,
        refs,
        out: Vec::new(),
        truncated: false,
        node_capped: false,
        cur_sensitive: false,
        hidden_removed: 0,
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
    if b.node_capped {
        b.out.push(SnapNode {
            depth: 0,
            ref_id: None,
            role: "text".to_string(),
            name: "[snapshot truncated: node limit reached]".to_string(),
            value: None,
            states: Vec::new(),
        });
    }
    let hidden_removed = b.hidden_removed;
    let mut out = b.out;
    if opts.compact && !opts.interactive_only {
        prune_empty(&mut out);
    }
    (out, hidden_removed)
}

impl Builder<'_, '_> {
    fn depth_ok(&self, depth: usize) -> bool {
        self.ctx.opts.interactive_only || self.ctx.opts.max_depth.is_none_or(|m| depth <= m)
    }

    fn emit_frame(&mut self, f: usize, depth: usize, nest: usize) {
        for &r in &self.ctx.parsed[f].roots {
            if self.node_capped {
                return;
            }
            self.emit_node(f, r, depth, "", nest);
        }
        // Cursor-interactive elements the AX tree does not know about.
        let frame = &self.ctx.frames[f];
        for e in &frame.extras {
            if self.node_capped {
                return;
            }
            if !e.is_cursor_interactive()
                || e.text.trim().is_empty()
                || self.ctx.parsed[f].ax_backends.contains(&e.backend_id)
                || e.frame_id.as_deref().is_some_and(|id| id != frame.frame_id)
            {
                continue;
            }
            if frame.hidden.contains(&e.backend_id)
                || frame.suspicious_names.contains(&e.backend_id)
            {
                self.hidden_removed += 1;
                continue;
            }
            let d = if self.ctx.opts.interactive_only {
                0
            } else {
                depth
            };
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

    /// Whether the AX subtree under `n` would have shown any text (bounded walk).
    fn subtree_has_text(&self, f: usize, n: usize) -> bool {
        let nodes = &self.ctx.parsed[f].nodes;
        let mut stack = vec![n];
        let mut seen = 0usize;
        while let Some(i) = stack.pop() {
            seen += 1;
            if seen > 2_000 {
                return true;
            }
            let node = &nodes[i];
            if !node.ignored && !node.name.trim().is_empty() {
                return true;
            }
            stack.extend(node.children.iter().copied());
        }
        false
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
        if self.out.len() >= MAX_SNAPSHOT_NODES {
            self.node_capped = true;
            return;
        }
        let frame = &self.ctx.frames[f];
        let ref_id = with_ref.then(|| {
            self.refs.assign(
                frame.loader_id.as_deref(),
                RefEntry {
                    session_id: frame.session_id.clone(),
                    backend_id: backend,
                    frame_id: frame.frame_id.clone(),
                    role: role.to_string(),
                    name: name.to_string(),
                    sensitive: self.cur_sensitive,
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
        if self.node_capped {
            return;
        }
        if nest > MAX_NEST {
            self.truncated = true;
            return;
        }
        let node = &self.ctx.parsed[f].nodes[n];
        // Hidden from a person looking at the page: left out with everything below it.
        if node
            .backend
            .is_some_and(|b| self.ctx.frames[f].hidden.contains(&b))
        {
            if self.subtree_has_text(f, n) {
                self.hidden_removed += 1;
            }
            return;
        }
        let extra: Option<&DomExtra> = node
            .backend
            .and_then(|b| self.ctx.parsed[f].extras.get(&b))
            .map(|&i| &self.ctx.frames[f].extras[i]);
        let role = node.role.as_ref();
        let interactive = INTERACTIVE_ROLES.contains(&role);
        let label: &str = if node.name.trim().is_empty() {
            extra.map_or("", |e| e.text.as_str())
        } else {
            node.name.as_ref()
        };
        // A long, instruction-like `title` / `alt` / `aria-label` is not something a person
        // reads: the element keeps its role and ref but loses that name.
        let label: &str = if !label.trim().is_empty()
            && node
                .backend
                .is_some_and(|b| self.ctx.frames[f].suspicious_names.contains(&b))
        {
            self.hidden_removed += 1;
            ""
        } else {
            label
        };
        let clickable = !interactive
            && !node.ignored
            && role != "StaticText"
            && extra.is_some_and(|e| e.is_cursor_interactive() && !label.trim().is_empty());

        let label: &str = if self.ctx.opts.compact
            && !interactive
            && !clickable
            && NAME_FROM_CONTENT.contains(&role)
            && !node.children.is_empty()
        {
            ""
        } else {
            label
        };

        // Transparent nodes: hoist children.
        if node.ignored || (TRANSPARENT_ROLES.contains(&role) && !clickable) {
            for &c in &node.children {
                self.emit_node(f, c, depth, parent_name, nest + 1);
            }
            return;
        }
        let nd = if self.ctx.opts.interactive_only {
            0
        } else {
            depth
        };
        if !self.depth_ok(depth) {
            return;
        }

        if role == "StaticText" {
            let t = node.name.trim();
            if self.refs.secret_on_page() && looks_like_a_code(t) {
                return;
            }
            let repeats_parent = self.ctx.opts.compact && t == parent_name.trim();
            if !(self.ctx.opts.interactive_only || t.is_empty() || repeats_parent) {
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
        let hidden_by_mode = self.ctx.opts.interactive_only && !(interactive || clickable);
        let skip_line = hidden_by_mode
            || (self.ctx.opts.compact && unnamed_container)
            || is_iframe && self.ctx.opts.interactive_only;

        // Mask sensitive values.
        let mut value = node.value.as_deref().map(str::to_string);
        // When the DOM capture failed, a text field is unknown, and unknown counts as secret. A node
        // with no DOM facts in a successful capture is not an input / textarea (those always get
        // facts): a contenteditable editor or a native <select> cannot be a password field.
        let unknown_sensitivity =
            self.ctx.frames[f].extras_failed && FREE_TEXT_ROLES.contains(&role);
        // While a typed secret is on the page, no text-entry value is shown: the page may
        // have copied it into sibling boxes (one digit per box) or re-created the field.
        let free_text_during_secret = self.refs.secret_on_page() && FREE_TEXT_ROLES.contains(&role);
        // By backend id alone: a field found by selector has no session of its own, and a
        // collision with a node of another frame only hides more (fail closed).
        let latched = node
            .backend
            .is_some_and(|b| self.refs.masked.iter().any(|(_, id)| *id == b));
        let is_secret = latched || extra.is_some_and(DomExtra::is_sensitive);
        if (is_secret || unknown_sensitivity || free_text_during_secret) && value.is_some() {
            value = Some(MASK.to_string());
        }
        // Unknown sensitivity counts as secret: refuse rather than guess.
        self.cur_sensitive = is_secret || unknown_sensitivity;

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
                label,
                node.backend.unwrap_or(0),
                value,
                node.states.clone(),
                interactive || clickable,
            );
        }
        self.cur_sensitive = false;
        // Interactive leaves with the same text children would only repeat the name.
        let parent = if skip_line { parent_name } else { label };
        // The browser exposes a field's text as a child of the field: a latched field
        // shows nothing of its content there either.
        let hide_children = latched || free_text_during_secret;
        for &c in node.children.iter().filter(|_| !hide_children) {
            self.emit_node(f, c, child_depth, parent, nest + 1);
        }
        if is_iframe && let Some(b) = node.backend {
            let child = self.ctx.frames.iter().position(|c| {
                c.parent_frame_id.as_deref() == Some(self.ctx.frames[f].frame_id.as_str())
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
    fn hidden_text_and_links_are_left_out_and_counted() {
        // 15 = the paragraph's text, 20 = the "Learn more" link (with its text 21 below it).
        let mut frame = main_frame(EXAMPLE);
        frame.hidden = [15, 20].into_iter().collect();
        let mut refs = RefMap::new();
        let (nodes, removed) =
            build_snapshot_counted(&[frame], &SnapshotOptions::default(), &mut refs);
        let t = nodes.iter().map(SnapNode::line).collect::<Vec<_>>().join(
            "
",
        );
        assert!(t.contains("Example Domain"), "visible heading stays: {t}");
        assert!(!t.contains("This domain is for use"), "{t}");
        assert!(!t.contains("Learn more"), "{t}");
        assert_eq!(removed, 2);
        assert_eq!(refs.len(), 0, "a hidden link gets no ref");
        let snap = Snapshot {
            url: "https://example.com/".into(),
            title: "Example".into(),
            nodes,
            hidden_removed: removed,
        };
        assert!(
            snap.render().ends_with(
                "[2 hidden items removed: content a person looking at this page cannot see]"
            ),
            "{}",
            snap.render()
        );
        // Nothing hidden: no note.
        let none = Snapshot {
            hidden_removed: 0,
            ..snap
        };
        assert!(!none.render().contains("hidden item"));
    }

    #[test]
    fn a_long_instruction_like_name_is_dropped_but_the_link_stays() {
        let mut frame = main_frame(EXAMPLE);
        frame.suspicious_names = [20].into_iter().collect();
        let mut refs = RefMap::new();
        let (nodes, removed) =
            build_snapshot_counted(&[frame], &SnapshotOptions::default(), &mut refs);
        let t = nodes.iter().map(SnapNode::line).collect::<Vec<_>>().join(
            "
",
        );
        assert_eq!(removed, 1);
        assert!(
            t.contains("[e1] link"),
            "the link keeps its role and ref: {t}"
        );
        assert!(!t.contains("[e1] link \"Learn more\""), "{t}");
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
        for (backend_id, tag, input_type) in [(10, "input", Some("text")), (14, "select", None)] {
            f.extras.push(DomExtra {
                backend_id,
                tag: tag.into(),
                input_type: input_type.map(Into::into),
                ..Default::default()
            });
        }
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
            extras: if failed {
                Vec::new()
            } else {
                vec![DomExtra {
                    backend_id: 2,
                    tag: "input".into(),
                    input_type: Some("text".into()),
                    ..Default::default()
                }]
            },
            extras_failed: failed,
            ..Default::default()
        }
    }

    #[test]
    fn a_node_without_dom_facts_is_a_plain_field_but_a_failed_capture_is_unknown() {
        // Production never builds facts for a contenteditable editor or a native <select>.
        let mut f = textbox_frame("draft text", false);
        f.extras.clear();
        let mut refs = RefMap::new();
        let t = text(&[f], &SnapshotOptions::default(), &mut refs);
        assert!(t.contains("draft text"), "{t}");
        assert!(!refs.get("e1").unwrap().sensitive);
        let mut refs = RefMap::new();
        let t = text(
            &[textbox_frame("maybe-a-secret", true)],
            &SnapshotOptions::default(),
            &mut refs,
        );
        assert!(!t.contains("maybe-a-secret"), "{t}");
        assert!(refs.get("e1").unwrap().sensitive);
    }

    #[test]
    fn otp_names_match_compounds_but_not_ordinary_words() {
        for yes in [
            "otp",
            "user_otp",
            "mfaOtp",
            "totp",
            "otpcode",
            "otpinput",
            "smsotp",
            "emailotp",
            "otp-code",
            "hotpcode",
            "one-time-code",
            "onetimecode",
        ] {
            assert!(name_hint_is_secret(yes), "{yes}");
        }
        for no in ["footprint", "hotpot", "footpath", "email", "notpad"] {
            assert!(!name_hint_is_secret(no), "{no}");
        }
    }

    #[test]
    fn otp_in_the_middle_and_more_secret_hints() {
        for yes in [
            "emailotpcode",
            "verifyotpcode",
            "enterotp1",
            "emailOtpCode",
            "cardnumber",
            "card-number",
            "Card_Number",
            "cc-number",
            "securitycode",
            "verification-code",
            "2fa",
            "mfa-token",
            "passcode",
            "pin",
            "user_pin",
            "cardPin",
            "pincode",
        ] {
            assert!(name_hint_is_secret(yes), "{yes}");
        }
        for no in [
            "footprint",
            "hotpot",
            "spotpicker",
            "shipping",
            "pinterest",
            "spinner",
            "mapping",
            "username",
            "email",
            "address",
            "search",
            "zip",
            "phone",
            "first name",
            "last name",
            "Confirm family name",
            "Team fabric",
            "Room facility",
            "shipping address",
            "a1b2fa3c",
        ] {
            assert!(!name_hint_is_secret(no), "{no}");
        }
    }

    #[test]
    fn a_captured_field_is_masked_by_backend_id_whatever_session_the_latch_names() {
        // A field read by selector is latched as `(None, id)`; the snapshot frame may carry a
        // session of its own. The value (and the text the browser shows under it) stays out.
        let key = "sk-live-Zx81QpLm0AbCdEf";
        let ax = json!([
            {"nodeId":"1","role":{"value":"RootWebArea"},"childIds":["2"],"backendDOMNodeId":1},
            {"nodeId":"2","parentId":"1","role":{"value":"textbox"},"name":{"value":"API key"},
             "value":{"value":key},"childIds":["3"],"backendDOMNodeId":2},
            {"nodeId":"3","parentId":"2","role":{"value":"StaticText"},"name":{"value":key},
             "childIds":[],"backendDOMNodeId":3}
        ]);
        let frame = || FrameInput {
            frame_id: "main".into(),
            loader_id: Some("L1".into()),
            session_id: Some("S".into()),
            ax_nodes: ax.as_array().unwrap().clone(),
            extras: vec![DomExtra {
                backend_id: 2,
                tag: "input".into(),
                input_type: Some("text".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let plain = text(&[frame()], &SnapshotOptions::default(), &mut RefMap::new());
        assert!(
            plain.contains(key),
            "the page shows it in the clear: {plain}"
        );
        let mut refs = RefMap::new();
        refs.set_masked(Some(vec![(None, 2)]));
        let masked = text(&[frame()], &SnapshotOptions::default(), &mut refs);
        assert!(
            !masked.contains(key) && !masked.contains("Zx81"),
            "{masked}"
        );
        assert!(masked.contains(MASK), "{masked}");
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
