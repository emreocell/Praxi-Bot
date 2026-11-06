//! Snapshot and UI-node types.
//!
//! Index and fingerprint design adapted from kirocrew computer-use module (MIT).

use serde::{Deserialize, Serialize};

// ── Fingerprint ───────────────────────────────────────────────────────────────

/// A lightweight identity token for a [`UiNode`].
///
/// Computed as a stable hash over `(role, name)`.  Used to verify that an
/// element the agent targeted has not been replaced between the snapshot and the
/// action dispatch.
///
/// # Algorithm
///
/// We use a simple FNV-1a 64-bit hash over the concatenated UTF-8 bytes of
/// `role` and `name`.  This avoids pulling in a heavy dependency and is
/// sufficient for identity checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint(pub u64);

impl Fingerprint {
    /// Compute a fingerprint from role and name strings.
    #[must_use]
    pub fn compute(role: &str, name: &str) -> Self {
        // Adapted from public-domain FNV-1a reference implementation.
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = OFFSET;
        for b in role
            .bytes()
            .chain(std::iter::once(b'\0'))
            .chain(name.bytes())
        {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
        Self(h)
    }

    /// Return `true` if `before` and `after` represent the same logical element.
    #[must_use]
    pub fn verify(before: Fingerprint, after: Fingerprint) -> bool {
        before == after
    }
}

// ── UI node ───────────────────────────────────────────────────────────────────

/// A rectangle in screen-pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Return the centre point.
    #[must_use]
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// Semantic role of a UI node (accessibility role string).
pub type Role = String;

/// A single node in the accessibility tree.
///
/// Index + fingerprint design from kirocrew computer-use (MIT).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiNode {
    /// Stable index within this snapshot generation.
    pub index: u32,
    /// Accessibility role (e.g. `"button"`, `"textbox"`).
    pub role: Role,
    /// Human-readable name / label.
    pub name: String,
    /// Current value (for text fields, sliders, etc.).
    pub value: Option<String>,
    /// Bounding box in screen-pixel coordinates.
    pub bounds: Option<Rect>,
    /// Set of accessibility states (e.g. `"focused"`, `"checked"`).
    pub states: Vec<String>,
    /// Identity token derived from role + name.
    pub fingerprint: Fingerprint,
}

impl UiNode {
    /// Create a node and auto-compute its fingerprint.
    pub fn new(
        index: u32,
        role: impl Into<String>,
        name: impl Into<String>,
        value: Option<String>,
        bounds: Option<Rect>,
        states: Vec<String>,
    ) -> Self {
        let role = role.into();
        let name = name.into();
        let fingerprint = Fingerprint::compute(&role, &name);
        Self {
            index,
            role,
            name,
            value,
            bounds,
            states,
            fingerprint,
        }
    }
}

// ── Tree quality ──────────────────────────────────────────────────────────────

/// A heuristic assessment of the accessibility tree's quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeQuality {
    /// The tree is rich; standard element-targeted actions work well.
    Good,
    /// The tree is sparse or has many unnamed nodes; OCR fallback recommended.
    Weak,
}

impl TreeQuality {
    /// Decide tree quality from a node list (pure function; no OS calls).
    ///
    /// Heuristics (ADR-0003):
    /// - Fewer than 5 nodes → `Weak`.
    /// - More than 50 % of nodes have an empty name → `Weak`.
    #[must_use]
    pub fn assess(nodes: &[UiNode]) -> Self {
        if nodes.len() < 5 {
            return Self::Weak;
        }
        let unnamed = nodes.iter().filter(|n| n.name.is_empty()).count();
        if unnamed * 2 > nodes.len() {
            Self::Weak
        } else {
            Self::Good
        }
    }
}

// ── Window / image / quality ──────────────────────────────────────────────────

/// Basic metadata for an on-screen window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Platform-specific window identifier (opaque).
    pub id: u64,
    /// Window title.
    pub title: String,
    /// Process ID of the owning application.
    pub pid: u32,
    /// Window bounds.
    pub bounds: Rect,
    /// Whether the window is currently focused.
    pub focused: bool,
}

/// A captured image (screenshot).
#[derive(Debug, Clone)]
pub struct Image {
    /// Encoded image bytes in the format named by `mime_type`
    /// (JPEG for lossy qualities, PNG for `Lossless`).
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// MIME type (e.g. `"image/png"` or `"image/jpeg"`).
    pub mime_type: String,
}

/// Screenshot quality / compression setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Low,
    Medium,
    High,
    Lossless,
}

/// A region of the screen to capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Region {
    /// The full screen (all monitors).
    FullScreen,
    /// A specific window.
    Window { window_id: u64 },
    /// An arbitrary rectangle.
    Rect(Rect),
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

/// A point-in-time view of a window's accessibility tree.
///
/// `generation` increments each time a new snapshot is taken and can be used to
/// detect staleness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// The window that was snapshotted.
    pub window: WindowInfo,
    /// All accessibility nodes in the window, indexed by [`UiNode::index`].
    pub nodes: Vec<UiNode>,
    /// Monotonically increasing generation counter.
    pub generation: u64,
    /// Heuristic quality assessment of the tree.
    pub quality: TreeQuality,
}

impl Snapshot {
    /// Look up a node by index.
    #[must_use]
    pub fn node_by_index(&self, index: u32) -> Option<&UiNode> {
        self.nodes.iter().find(|n| n.index == index)
    }
}

// ── AppSpec / Pid ─────────────────────────────────────────────────────────────

/// Specification for launching an application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSpec {
    /// Executable path or application bundle identifier (macOS).
    pub executable: String,
    /// Command-line arguments.
    pub args: Vec<String>,
}

/// A process identifier (OS PID).
pub type Pid = u32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_same_role_name_produces_same_hash() {
        let f1 = Fingerprint::compute("button", "OK");
        let f2 = Fingerprint::compute("button", "OK");
        assert_eq!(f1, f2);
    }

    #[test]
    fn fingerprint_different_name_produces_different_hash() {
        let f1 = Fingerprint::compute("button", "OK");
        let f2 = Fingerprint::compute("button", "Cancel");
        assert_ne!(f1, f2);
    }

    #[test]
    fn fingerprint_verify_matches_equal_fingerprints() {
        let f = Fingerprint::compute("link", "Home");
        assert!(Fingerprint::verify(f, f));
    }

    #[test]
    fn fingerprint_verify_rejects_changed_element() {
        let before = Fingerprint::compute("button", "Save");
        let after = Fingerprint::compute("button", "Delete");
        assert!(!Fingerprint::verify(before, after));
    }

    #[test]
    fn tree_quality_weak_when_few_nodes() {
        let nodes: Vec<UiNode> = (0..4)
            .map(|i| UiNode::new(i, "button", "OK", None, None, vec![]))
            .collect();
        assert_eq!(TreeQuality::assess(&nodes), TreeQuality::Weak);
    }

    #[test]
    fn tree_quality_weak_when_many_unnamed() {
        let nodes: Vec<UiNode> = (0..10)
            .map(|i| {
                let name = if i < 7 { "" } else { "Button" };
                UiNode::new(i, "generic", name, None, None, vec![])
            })
            .collect();
        assert_eq!(TreeQuality::assess(&nodes), TreeQuality::Weak);
    }

    #[test]
    fn tree_quality_good_with_rich_named_tree() {
        let nodes: Vec<UiNode> = (0..10)
            .map(|i| UiNode::new(i, "button", format!("Button {i}"), None, None, vec![]))
            .collect();
        assert_eq!(TreeQuality::assess(&nodes), TreeQuality::Good);
    }

    #[test]
    fn rect_center() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(r.center(), (60.0, 45.0));
    }
}
