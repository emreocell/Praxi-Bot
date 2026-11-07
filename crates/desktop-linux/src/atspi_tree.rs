//! AT-SPI2 accessibility tree walker and action dispatcher over D-Bus (`atspi`).

use std::collections::HashSet;

use atspi::{
    AccessibilityConnection, CoordType, Interface, InterfaceSet, ObjectRefOwned, Role, State,
    StateSet,
    connection::set_session_accessibility,
    proxy::{
        accessible::{AccessibleProxy, ObjectRefExt as _},
        action::ActionProxy,
        component::ComponentProxy,
        editable_text::EditableTextProxy,
        text::TextProxy,
        value::ValueProxy,
    },
    zbus::{self, proxy::CacheProperties},
};
use desktop_api::{
    DesktopError,
    action::{Action, ActionResult, ClickMethod, CoordinateSpace, Target},
    snapshot::{Fingerprint, Rect, Snapshot, TreeQuality, WindowInfo},
};

use crate::{
    pure::{self, MAX_DEPTH, MAX_NODES, RawNode},
    x11,
};

type Res<T> = Result<T, DesktopError>;

fn a11y_err<E: std::fmt::Display>(ctx: &str) -> impl FnOnce(E) -> DesktopError + '_ {
    move |e| DesktopError::Accessibility(format!("{ctx}: {e}"))
}

#[derive(Debug, Clone)]
struct Retained {
    index: u32,
    object_ref: ObjectRefOwned,
    interfaces: InterfaceSet,
    actions: Vec<String>,
    bounds: Option<Rect>,
    fingerprint: Fingerprint,
}

/// Stateful AT-SPI worker holding the current snapshot generation and retained
/// object references.
#[derive(Debug, Default)]
pub struct AtspiWorker {
    generation: u64,
    retained: Vec<Retained>,
    window_bounds: Option<Rect>,
}

impl AtspiWorker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&mut self, scope: Option<u64>) -> Res<Snapshot> {
        x11::ensure_x11_session()?;

        // Best-effort enablement of session accessibility so toolkits expose bridges.
        let _ = set_session_accessibility(true).await;

        let a11y = AccessibilityConnection::new()
            .await
            .map_err(a11y_err("AccessibilityConnection::new"))?;
        let conn = a11y.connection();

        let x11_win = match scope {
            Some(id) => Some(
                x11::window_info(id)
                    .ok_or_else(|| DesktopError::Accessibility("window not found".into()))?,
            ),
            None => x11::active_window_info()?,
        };

        let (frame_ref, frame_title, frame_bounds) =
            find_target_frame(&a11y, x11_win.as_ref(), scope.is_some()).await?;

        let window = match x11_win {
            Some(mut w) => {
                if w.title.is_empty() && !frame_title.is_empty() {
                    w.title = frame_title;
                }
                if (w.bounds.width <= 1.0 || w.bounds.height <= 1.0)
                    && let Some(b) = frame_bounds
                {
                    w.bounds = b;
                }
                w
            }
            None => WindowInfo {
                id: 0,
                title: frame_title,
                pid: 0,
                bounds: frame_bounds.unwrap_or(Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1920.0,
                    height: 1080.0,
                }),
                focused: true,
            },
        };

        let walked = walk_subtree(conn, frame_ref).await;
        if walked.is_empty() {
            return Err(DesktopError::Accessibility(
                "AT-SPI walk produced no accessible nodes for target window".into(),
            ));
        }

        let raws: Vec<RawNode> = walked.iter().map(|item| item.raw.clone()).collect();
        let selected = pure::select(&raws, MAX_NODES);

        let mut nodes = Vec::with_capacity(selected.len());
        let mut retained = Vec::with_capacity(selected.len());

        for (n, &src) in selected.iter().enumerate() {
            let index = u32::try_from(n).unwrap_or(u32::MAX);
            let item = &walked[src];
            let node = pure::to_ui_node(index, &item.raw);
            retained.push(Retained {
                index,
                object_ref: item.object_ref.clone(),
                interfaces: item.interfaces,
                actions: item.raw.actions.clone(),
                bounds: item.raw.bounds,
                fingerprint: node.fingerprint,
            });
            nodes.push(node);
        }

        self.retained = retained;
        self.generation += 1;
        self.window_bounds = Some(window.bounds);
        let quality = TreeQuality::assess(&nodes);

        Ok(Snapshot {
            window,
            nodes,
            generation: self.generation,
            quality,
        })
    }

    fn lookup_retained(&self, index: u32, generation: u64) -> Res<Retained> {
        let not_found = DesktopError::ElementNotFound { index, generation };
        if generation != self.generation {
            return Err(not_found);
        }
        self.retained
            .iter()
            .find(|r| r.index == index)
            .cloned()
            .ok_or(not_found)
    }

    async fn resolve_verified(
        &self,
        conn: &zbus::Connection,
        index: u32,
        generation: u64,
    ) -> Res<Retained> {
        let r = self.lookup_retained(index, generation)?;
        let ap = r
            .object_ref
            .as_accessible_proxy(conn)
            .await
            .map_err(a11y_err("as_accessible_proxy"))?;
        let role = ap.get_role().await.unwrap_or(Role::Unknown);
        let states = ap.get_state().await.unwrap_or_default();
        let name = ap.name().await.map_err(a11y_err("Accessible::name"))?;
        let live_role = pure::role_name(role.name(), states.contains(State::Editable));
        let live_fp = Fingerprint::compute(live_role, &name);
        if !Fingerprint::verify(r.fingerprint, live_fp) {
            return Err(DesktopError::Accessibility(format!(
                "fingerprint mismatch for index {index}: element changed since snapshot; \
                 re-snapshot before acting"
            )));
        }
        Ok(r)
    }

    async fn element_center(conn: &zbus::Connection, r: &Retained) -> Res<(f64, f64)> {
        if r.interfaces.contains(Interface::Component)
            && let Some(rect) = read_extents(conn, &r.object_ref).await
        {
            return Ok(rect.center());
        }
        if let Some(rect) = r.bounds {
            return Ok(rect.center());
        }
        Err(DesktopError::Accessibility(
            "element has no on-screen bounds".into(),
        ))
    }

    fn current_window_rect(&self) -> Res<Rect> {
        if let Some(b) = self.window_bounds {
            return Ok(b);
        }
        if let Ok(Some(w)) = x11::active_window_info() {
            return Ok(w.bounds);
        }
        Err(DesktopError::Accessibility("no target window".into()))
    }

    async fn resolve_point(&self, conn: &zbus::Connection, target: &Target) -> Res<(f64, f64)> {
        match *target {
            Target::ByIndex { index, generation } => {
                let r = self.resolve_verified(conn, index, generation).await?;
                Self::element_center(conn, &r).await
            }
            Target::ByCoordinate { x, y, space } => match space {
                CoordinateSpace::Absolute => Ok((x, y)),
                CoordinateSpace::Normalized999 => {
                    Ok(pure::denormalize(x, y, &self.current_window_rect()?))
                }
            },
        }
    }

    pub async fn act(&mut self, action: &Action) -> Res<ActionResult> {
        x11::ensure_x11_session()?;

        match action {
            Action::TypeText { text } => {
                x11::type_text(text)?;
                return Ok(ActionResult {
                    dispatched: true,
                    verified: None,
                    new_snapshot: None,
                });
            }
            Action::KeyPress { keys } => {
                x11::key_chord(keys)?;
                return Ok(ActionResult {
                    dispatched: true,
                    verified: None,
                    new_snapshot: None,
                });
            }
            _ => {}
        }

        let a11y = AccessibilityConnection::new()
            .await
            .map_err(a11y_err("AccessibilityConnection::new"))?;
        let conn = a11y.connection();

        match action {
            Action::Click { target, method } => match (target, method) {
                (Target::ByIndex { index, generation }, ClickMethod::Accessibility) => {
                    let r = self.resolve_verified(conn, *index, *generation).await?;
                    if !try_atspi_click(conn, &r).await? {
                        let (x, y) = Self::element_center(conn, &r).await?;
                        x11::click_at(x, y)?;
                    }
                }
                (_, ClickMethod::Global) => {
                    let (x, y) = self.resolve_point(conn, target).await?;
                    x11::click_at(x, y)?;
                }
                (Target::ByCoordinate { .. }, ClickMethod::Accessibility) => {
                    return Err(DesktopError::Unsupported(
                        "accessibility clicks need an element index; use method=global for coordinates"
                            .into(),
                    ));
                }
                (_, ClickMethod::AppPost) => {
                    return Err(DesktopError::Unsupported(
                        "app_post click is not implemented on Linux yet".into(),
                    ));
                }
            },
            Action::SetValue { target, value } => match target {
                Target::ByIndex { index, generation } => {
                    let r = self.resolve_verified(conn, *index, *generation).await?;
                    set_element_value(conn, &r, value).await?;
                }
                Target::ByCoordinate { .. } => {
                    return Err(DesktopError::Unsupported(
                        "set_value needs an element index".into(),
                    ));
                }
            },
            Action::Scroll {
                target,
                delta_x,
                delta_y,
            } => {
                let (x, y) = self.resolve_point(conn, target).await?;
                x11::scroll_at(x, y, *delta_x, *delta_y)?;
            }
            Action::Drag { from, to, method } => {
                if *method != ClickMethod::Global {
                    return Err(DesktopError::Unsupported(
                        "drag requires method=global".into(),
                    ));
                }
                let a = self.resolve_point(conn, from).await?;
                let b = self.resolve_point(conn, to).await?;
                x11::drag(a, b)?;
            }
            Action::TypeText { .. } | Action::KeyPress { .. } => unreachable!(),
        }

        Ok(ActionResult {
            dispatched: true,
            verified: None,
            new_snapshot: None,
        })
    }
}

// ── AT-SPI action & value helpers ─────────────────────────────────────────────

async fn try_atspi_click(conn: &zbus::Connection, r: &Retained) -> Res<bool> {
    let Some(bus_name) = r.object_ref.name() else {
        return Ok(false);
    };
    let path = r.object_ref.path();

    if r.interfaces.contains(Interface::Action)
        && let Ok(action_proxy) = ActionProxy::builder(conn)
            .destination(bus_name.clone())
            .and_then(|b| b.path(path.clone()))
            .map(|b| b.cache_properties(CacheProperties::No))
        && let Ok(proxy) = action_proxy.build().await
    {
        let actions = if r.actions.is_empty() {
            proxy
                .get_actions()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.name)
                .collect::<Vec<_>>()
        } else {
            r.actions.clone()
        };
        if let Some(idx) = pure::preferred_action_index(&actions)
            && let Ok(true) = proxy.do_action(idx).await
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn set_element_value(conn: &zbus::Connection, r: &Retained, value: &str) -> Res<()> {
    if let Some(bus_name) = r.object_ref.name() {
        let path = r.object_ref.path();

        if r.interfaces.contains(Interface::EditableText)
            && let Ok(builder) = EditableTextProxy::builder(conn)
                .destination(bus_name.clone())
                .and_then(|b| b.path(path.clone()))
            && let Ok(editable) = builder.cache_properties(CacheProperties::No).build().await
        {
            if let Ok(true) = editable.set_text_contents(value).await {
                return Ok(());
            }
            let _ = editable.delete_text(0, i32::MAX).await;
            let len = i32::try_from(value.chars().count()).unwrap_or(0);
            if let Ok(true) = editable.insert_text(0, value, len).await {
                return Ok(());
            }
        }

        if r.interfaces.contains(Interface::Value)
            && let Ok(num) = value.trim().parse::<f64>()
            && let Ok(builder) = ValueProxy::builder(conn)
                .destination(bus_name.clone())
                .and_then(|b| b.path(path.clone()))
            && let Ok(val_proxy) = builder.cache_properties(CacheProperties::No).build().await
            && val_proxy.set_current_value(num).await.is_ok()
        {
            return Ok(());
        }

        // Grab focus via AT-SPI Component before XTest typing fallback.
        let mut focused = false;
        if r.interfaces.contains(Interface::Component)
            && let Ok(builder) = ComponentProxy::builder(conn)
                .destination(bus_name.clone())
                .and_then(|b| b.path(path.clone()))
            && let Ok(comp) = builder.cache_properties(CacheProperties::No).build().await
            && let Ok(true) = comp.grab_focus().await
        {
            focused = true;
        }
        if !focused && let Ok((x, y)) = AtspiWorker::element_center(conn, r).await {
            x11::click_at(x, y)?;
            focused = true;
        }
        if !focused {
            return Err(DesktopError::Accessibility(
                "set_value: could not focus the target element; refusing to type into the focused window"
                    .into(),
            ));
        }
    } else {
        return Err(DesktopError::Accessibility(
            "set_value: target has no bus name; refusing to type into the focused window".into(),
        ));
    }

    x11::key_chord(&["ctrl".to_string(), "a".to_string()])?;
    x11::key_chord(&["BackSpace".to_string()])?;
    x11::type_text(value)
}

// ── Target frame discovery & tree walk ────────────────────────────────────────

struct WalkedNode {
    object_ref: ObjectRefOwned,
    interfaces: InterfaceSet,
    raw: RawNode,
}

async fn find_target_frame(
    a11y: &AccessibilityConnection,
    x11_win: Option<&WindowInfo>,
    explicit_scope: bool,
) -> Res<(ObjectRefOwned, String, Option<Rect>)> {
    let conn = a11y.connection();
    let registry = a11y
        .root_accessible_on_registry()
        .await
        .map_err(a11y_err("root_accessible_on_registry"))?;
    let apps = registry
        .get_children()
        .await
        .map_err(a11y_err("registry.get_children"))?;

    let mut best: Option<(i32, ObjectRefOwned, String, Option<Rect>)> = None;

    for app_ref in apps {
        if app_ref.is_null() {
            continue;
        }
        let Ok(app_proxy) = app_ref.as_accessible_proxy(conn).await else {
            continue;
        };
        let frames = app_proxy.get_children().await.unwrap_or_default();
        for frame_ref in frames {
            if frame_ref.is_null() {
                continue;
            }
            let Ok(frame_proxy) = frame_ref.as_accessible_proxy(conn).await else {
                continue;
            };
            let state = frame_proxy.get_state().await.unwrap_or_default();
            if state.contains(State::Defunct) {
                continue;
            }
            let title = frame_proxy.name().await.unwrap_or_default();
            let ifaces = frame_proxy.get_interfaces().await.unwrap_or_default();
            let bounds = if ifaces.contains(Interface::Component) {
                read_extents(conn, &frame_ref).await
            } else {
                None
            };

            let mut score = 0i32;
            if state.contains(State::Active) {
                score += 80;
            }
            if state.contains(State::Focused) {
                score += 30;
            }
            if state.contains(State::Showing) {
                score += 15;
            }
            if state.contains(State::Visible) {
                score += 5;
            }
            if let Some(w) = x11_win {
                if !w.title.is_empty() && !title.is_empty() {
                    if title == w.title {
                        score += 120;
                    } else if title.to_lowercase().contains(&w.title.to_lowercase())
                        || w.title.to_lowercase().contains(&title.to_lowercase())
                    {
                        score += 70;
                    }
                }
                if let Some(b) = bounds
                    && (b.x - w.bounds.x).abs() < 64.0
                    && (b.y - w.bounds.y).abs() < 64.0
                {
                    score += 45;
                }
            }

            if best
                .as_ref()
                .is_none_or(|(best_score, ..)| score > *best_score)
            {
                best = Some((score, frame_ref, title, bounds));
            }
        }
    }

    if let Some((score, frame_ref, title, bounds)) = best {
        if explicit_scope
            && let Some(w) = x11_win
            && !w.title.is_empty()
            && score < 40
        {
            return Err(DesktopError::Accessibility(format!(
                "target window '{}' not found on AT-SPI bus",
                w.title
            )));
        }
        return Ok((frame_ref, title, bounds));
    }

    Err(DesktopError::Accessibility(
        "no accessible application window found on AT-SPI bus (ensure GTK_MODULES=gail:atk-bridge \
         and org.a11y.Status IsEnabled=true)"
            .into(),
    ))
}

async fn walk_subtree(conn: &zbus::Connection, root_ref: ObjectRefOwned) -> Vec<WalkedNode> {
    let mut out = Vec::new();
    let mut visited: HashSet<(String, String)> = HashSet::new();
    let mut stack: Vec<(ObjectRefOwned, usize)> = vec![(root_ref, 0)];
    let max_visited = MAX_NODES * 3;

    while let Some((obj_ref, depth)) = stack.pop() {
        if out.len() >= max_visited || obj_ref.is_null() {
            continue;
        }
        let Some(bus_name) = obj_ref.name_as_str().map(str::to_owned) else {
            continue;
        };
        let path_str = obj_ref.path_as_str().to_owned();
        if !visited.insert((bus_name, path_str)) {
            continue;
        }

        let Ok(ap) = obj_ref.as_accessible_proxy(conn).await else {
            continue;
        };
        let states = ap.get_state().await.unwrap_or_default();
        if depth > 0 && states.contains(State::Defunct) {
            continue;
        }

        let (interfaces, raw) = read_node(conn, &obj_ref, &ap, states, depth == 0).await;
        let manages_descendants = states.contains(State::ManagesDescendants);

        if depth < MAX_DEPTH
            && !manages_descendants
            && let Ok(children) = ap.get_children().await
        {
            for child in children.into_iter().rev() {
                if !child.is_null() {
                    stack.push((child, depth + 1));
                }
            }
        }

        out.push(WalkedNode {
            object_ref: obj_ref,
            interfaces,
            raw,
        });
    }

    out
}

async fn read_node(
    conn: &zbus::Connection,
    obj_ref: &ObjectRefOwned,
    ap: &AccessibleProxy<'_>,
    states: StateSet,
    is_root: bool,
) -> (InterfaceSet, RawNode) {
    let atspi_role = ap.get_role().await.unwrap_or(Role::Unknown);
    let editable = states.contains(State::Editable);
    let role = pure::role_name(atspi_role.name(), editable);
    let name = ap.name().await.unwrap_or_default();
    let interfaces = ap.get_interfaces().await.unwrap_or_default();

    let bounds = if interfaces.contains(Interface::Component) {
        read_extents(conn, obj_ref).await
    } else {
        None
    };

    let actions = if interfaces.contains(Interface::Action) {
        read_actions(conn, obj_ref).await
    } else {
        Vec::new()
    };

    let password = atspi_role == Role::PasswordText;
    let value = if password {
        None
    } else {
        read_value(conn, obj_ref, interfaces, role, editable).await
    };

    let mut pattern_states: Vec<&'static str> = Vec::new();
    if states.contains(State::Checked) {
        pattern_states.push("checked");
    } else if states.contains(State::Indeterminate) {
        pattern_states.push("indeterminate");
    }
    if states.contains(State::Selected) {
        pattern_states.push("selected");
    }
    if states.contains(State::Expanded) {
        pattern_states.push("expanded");
    } else if states.contains(State::Collapsed) || states.contains(State::Expandable) {
        pattern_states.push("collapsed");
    }
    if states.contains(State::Pressed) {
        pattern_states.push("pressed");
    }
    if states.contains(State::ReadOnly) {
        pattern_states.push("readonly");
    }

    let showing_or_visible =
        is_root || states.contains(State::Showing) || states.contains(State::Visible);
    let offscreen = !showing_or_visible;
    let enabled = is_root || states.contains(State::Enabled) || states.contains(State::Sensitive);
    let focused = states.contains(State::Focused);

    (
        interfaces,
        RawNode {
            role,
            name,
            value,
            bounds,
            offscreen,
            enabled,
            focused,
            password,
            pattern_states,
            actions,
        },
    )
}

async fn read_extents(conn: &zbus::Connection, obj_ref: &ObjectRefOwned) -> Option<Rect> {
    let bus_name = obj_ref.name()?.clone();
    let path = obj_ref.path().clone();
    let comp = ComponentProxy::builder(conn)
        .destination(bus_name)
        .ok()?
        .path(path)
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()?;
    let (x, y, w, h) = comp.get_extents(CoordType::Screen).await.ok()?;
    (w > 0 && h > 0).then(|| Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(w),
        height: f64::from(h),
    })
}

async fn read_actions(conn: &zbus::Connection, obj_ref: &ObjectRefOwned) -> Vec<String> {
    let Some(bus_name) = obj_ref.name().cloned() else {
        return Vec::new();
    };
    let path = obj_ref.path().clone();
    let Ok(builder) = ActionProxy::builder(conn)
        .destination(bus_name)
        .and_then(|b| b.path(path))
    else {
        return Vec::new();
    };
    let Ok(proxy) = builder.cache_properties(CacheProperties::No).build().await else {
        return Vec::new();
    };
    proxy
        .get_actions()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.name)
        .filter(|s| !s.is_empty())
        .collect()
}

async fn read_value(
    conn: &zbus::Connection,
    obj_ref: &ObjectRefOwned,
    interfaces: InterfaceSet,
    role: &str,
    editable: bool,
) -> Option<String> {
    let bus_name = obj_ref.name()?.clone();
    let path = obj_ref.path().clone();

    if interfaces.contains(Interface::Text)
        && (editable || matches!(role, "edit" | "combobox"))
        && let Ok(builder) = TextProxy::builder(conn)
            .destination(bus_name.clone())
            .and_then(|b| b.path(path.clone()))
        && let Ok(text_proxy) = builder.cache_properties(CacheProperties::No).build().await
        && let Ok(count) = text_proxy.character_count().await
        && count > 0
        && let Ok(txt) = text_proxy.get_text(0, count.min(500)).await
        && !txt.is_empty()
    {
        return Some(txt);
    }

    if interfaces.contains(Interface::Value)
        && let Ok(builder) = ValueProxy::builder(conn)
            .destination(bus_name)
            .and_then(|b| b.path(path))
        && let Ok(val_proxy) = builder.cache_properties(CacheProperties::No).build().await
    {
        if let Ok(txt) = val_proxy.text().await
            && !txt.is_empty()
        {
            return Some(txt);
        }
        if let Ok(v) = val_proxy.current_value().await {
            return Some(v.to_string());
        }
    }

    None
}
