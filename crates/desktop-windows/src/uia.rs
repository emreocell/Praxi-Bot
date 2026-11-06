//! UI Automation worker: lives on the dedicated STA thread and owns every COM
//! object. Nothing in this module is `Send`.
#![allow(unsafe_code)]

use desktop_api::{
    DesktopError,
    action::{Action, ActionResult, ClickMethod, CoordinateSpace, Target},
    snapshot::{Fingerprint, Rect, Snapshot, TreeQuality, UiNode},
};
use windows::{
    Win32::{
        Foundation::HWND,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Accessibility::{
            CUIAutomation, ExpandCollapseState_Collapsed, IUIAutomation, IUIAutomationCacheRequest,
            IUIAutomationElement, IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
            IUIAutomationScrollPattern, IUIAutomationSelectionItemPattern,
            IUIAutomationTogglePattern, IUIAutomationValuePattern, ScrollAmount_NoAmount,
            ScrollAmount_SmallDecrement, ScrollAmount_SmallIncrement, ToggleState_Indeterminate,
            ToggleState_On, TreeScope_Subtree, UIA_BoundingRectanglePropertyId,
            UIA_ControlTypePropertyId, UIA_ExpandCollapseExpandCollapseStatePropertyId,
            UIA_ExpandCollapsePatternId, UIA_HasKeyboardFocusPropertyId, UIA_InvokePatternId,
            UIA_IsEnabledPropertyId, UIA_IsOffscreenPropertyId, UIA_IsPasswordPropertyId,
            UIA_NamePropertyId, UIA_ScrollPatternId, UIA_SelectionItemIsSelectedPropertyId,
            UIA_SelectionItemPatternId, UIA_TogglePatternId, UIA_ToggleToggleStatePropertyId,
            UIA_ValuePatternId, UIA_ValueValuePropertyId,
        },
    },
    core::{BOOL, BSTR},
};

use crate::{
    input,
    pure::{self, MAX_NODES, RawNode},
    winlist,
};

type Res<T> = Result<T, DesktopError>;

fn acc<E: std::fmt::Display>(ctx: &str) -> impl Fn(E) -> DesktopError + '_ {
    move |e| DesktopError::Accessibility(format!("{ctx}: {e}"))
}

struct Retained {
    index: u32,
    element: IUIAutomationElement,
    fingerprint: Fingerprint,
}

/// All UIA state; constructed and used on the STA thread only.
pub struct Worker {
    uia: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    generation: u64,
    retained: Vec<Retained>,
    hwnd: Option<HWND>,
}

impl Worker {
    pub fn new() -> Res<Self> {
        winlist::ensure_dpi_aware();
        // SAFETY: initialises COM for this thread as an STA; paired with
        // CoUninitialize in Drop. Creation and cache setup are plain COM calls.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(acc("CoInitializeEx"))?;
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(acc("CoCreateInstance(CUIAutomation)"))?;
            let cache = uia
                .CreateCacheRequest()
                .map_err(acc("CreateCacheRequest"))?;
            for p in [
                UIA_NamePropertyId,
                UIA_ControlTypePropertyId,
                UIA_BoundingRectanglePropertyId,
                UIA_IsEnabledPropertyId,
                UIA_IsOffscreenPropertyId,
                UIA_HasKeyboardFocusPropertyId,
                UIA_IsPasswordPropertyId,
                UIA_ValueValuePropertyId,
                UIA_ToggleToggleStatePropertyId,
                UIA_SelectionItemIsSelectedPropertyId,
                UIA_ExpandCollapseExpandCollapseStatePropertyId,
            ] {
                cache.AddProperty(p).map_err(acc("AddProperty"))?;
            }
            for p in [
                UIA_ValuePatternId,
                UIA_TogglePatternId,
                UIA_SelectionItemPatternId,
                UIA_ExpandCollapsePatternId,
            ] {
                cache.AddPattern(p).map_err(acc("AddPattern"))?;
            }
            Ok(Self {
                uia,
                cache,
                generation: 0,
                retained: Vec::new(),
                hwnd: None,
            })
        }
    }

    /// Release every COM object, then uninitialise COM (STA thread exit).
    pub fn close(self) {
        let Self {
            uia,
            cache,
            retained,
            ..
        } = self;
        drop(retained);
        drop(cache);
        drop(uia);
        // SAFETY: balances the CoInitializeEx in `Worker::new` on the same
        // thread, after all COM objects have been released above.
        unsafe { CoUninitialize() };
    }

    // ── snapshot ──────────────────────────────────────────────────────────────

    pub fn snapshot(&mut self, scope: Option<u64>) -> Res<Snapshot> {
        let hwnd = match scope {
            Some(id) => winlist::hwnd_from_id(id),
            None => winlist::foreground_hwnd()
                .ok_or_else(|| DesktopError::Accessibility("no foreground window".into()))?,
        };
        let window = winlist::window_info(hwnd)
            .ok_or_else(|| DesktopError::Accessibility("window not found".into()))?;

        // SAFETY: COM calls on live objects owned by this thread.
        let elements = unsafe {
            let root = self
                .uia
                .ElementFromHandleBuildCache(hwnd, &self.cache)
                .map_err(acc("ElementFromHandle"))?;
            let cond = self
                .uia
                .ControlViewCondition()
                .map_err(acc("ControlViewCondition"))?;
            let arr = root
                .FindAllBuildCache(TreeScope_Subtree, &cond, &self.cache)
                .map_err(acc("FindAll"))?;
            let len = arr.Length().map_err(acc("Length"))?;
            let mut v = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            for i in 0..len {
                v.push(arr.GetElement(i).map_err(acc("GetElement"))?);
            }
            v
        };

        let raws: Vec<RawNode> = elements.iter().map(read_raw).collect();
        let selected = pure::select(&raws, MAX_NODES);

        let mut nodes: Vec<UiNode> = Vec::with_capacity(selected.len());
        let mut retained = Vec::with_capacity(selected.len());
        for (n, &src) in selected.iter().enumerate() {
            let index = u32::try_from(n).unwrap_or(u32::MAX);
            let node = pure::to_ui_node(index, &raws[src]);
            retained.push(Retained {
                index,
                element: elements[src].clone(),
                fingerprint: node.fingerprint,
            });
            nodes.push(node);
        }

        // Releases the previous generation's element pointers.
        self.retained = retained;
        self.generation += 1;
        self.hwnd = Some(hwnd);
        let quality = TreeQuality::assess(&nodes);
        Ok(Snapshot {
            window,
            nodes,
            generation: self.generation,
            quality,
        })
    }

    // ── act ───────────────────────────────────────────────────────────────────

    fn resolve(&self, index: u32, generation: u64) -> Res<&Retained> {
        let not_found = DesktopError::ElementNotFound { index, generation };
        if generation != self.generation {
            return Err(not_found);
        }
        let r = self
            .retained
            .iter()
            .find(|r| r.index == index)
            .ok_or(not_found)?;
        // Verify the live element still matches the snapshot fingerprint.
        // SAFETY: COM property reads on a retained live element.
        let live = unsafe {
            let name = r.element.CurrentName().map_err(acc("CurrentName"))?;
            let ct = r
                .element
                .CurrentControlType()
                .map_err(acc("CurrentControlType"))?;
            Fingerprint::compute(pure::role_name(ct.0), &name.to_string())
        };
        if !Fingerprint::verify(r.fingerprint, live) {
            return Err(DesktopError::Accessibility(format!(
                "fingerprint mismatch for index {index}: element changed since snapshot; \
                 re-snapshot before acting"
            )));
        }
        Ok(r)
    }

    fn window_rect(&self) -> Res<Rect> {
        let hwnd = self
            .hwnd
            .or_else(winlist::foreground_hwnd)
            .ok_or_else(|| DesktopError::Accessibility("no target window".into()))?;
        winlist::window_bounds(hwnd)
            .ok_or_else(|| DesktopError::Accessibility("target window not found".into()))
    }

    fn element_center(r: &Retained) -> Res<(f64, f64)> {
        // SAFETY: COM property read on a retained live element.
        let rc = unsafe { r.element.CurrentBoundingRectangle() }
            .map_err(acc("CurrentBoundingRectangle"))?;
        if rc.right <= rc.left || rc.bottom <= rc.top {
            return Err(DesktopError::Accessibility(
                "element has no on-screen bounds".into(),
            ));
        }
        Ok((
            f64::midpoint(f64::from(rc.left), f64::from(rc.right)),
            f64::midpoint(f64::from(rc.top), f64::from(rc.bottom)),
        ))
    }

    fn point(&self, target: &Target) -> Res<(f64, f64)> {
        match *target {
            Target::ByIndex { index, generation } => {
                Self::element_center(self.resolve(index, generation)?)
            }
            Target::ByCoordinate { x, y, space } => match space {
                CoordinateSpace::Absolute => Ok((x, y)),
                CoordinateSpace::Normalized999 => Ok(pure::denormalize(x, y, &self.window_rect()?)),
            },
        }
    }

    /// Try UIA patterns in order: Invoke, Toggle, `SelectionItem`,
    /// `ExpandCollapse`. Returns `false` when none is supported.
    fn pattern_click(el: &IUIAutomationElement) -> Res<bool> {
        // SAFETY: pattern queries and invocations on a live element.
        unsafe {
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            {
                p.Invoke().map_err(acc("Invoke"))?;
                return Ok(true);
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            {
                p.Toggle().map_err(acc("Toggle"))?;
                return Ok(true);
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                UIA_SelectionItemPatternId,
            ) {
                p.Select().map_err(acc("Select"))?;
                return Ok(true);
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                UIA_ExpandCollapsePatternId,
            ) {
                let state = p
                    .CurrentExpandCollapseState()
                    .map_err(acc("ExpandCollapse"))?;
                if state == ExpandCollapseState_Collapsed {
                    p.Expand().map_err(acc("Expand"))?;
                } else {
                    p.Collapse().map_err(acc("Collapse"))?;
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn set_value(el: &IUIAutomationElement, value: &str) -> Res<()> {
        // SAFETY: pattern queries and calls on a live element.
        unsafe {
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                && !p.CurrentIsReadOnly().map_err(acc("IsReadOnly"))?.as_bool()
            {
                return p.SetValue(&BSTR::from(value)).map_err(acc("SetValue"));
            }
            // Fallback: focus, select all, type.
            el.SetFocus().map_err(acc("SetFocus"))?;
        }
        input::key_chord(&["ctrl".to_string(), "a".to_string()])?;
        input::type_text(value)
    }

    fn scroll(el: &IUIAutomationElement, dx: f64, dy: f64) -> Res<()> {
        // SAFETY: pattern query and Scroll calls on a live element.
        unsafe {
            let p = el
                .GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
                .map_err(|_| DesktopError::Unsupported("element is not scrollable".into()))?;
            // Positive dy scrolls down / positive dx scrolls right.
            let amount = |d: f64| {
                if d > 0.0 {
                    ScrollAmount_SmallIncrement
                } else if d < 0.0 {
                    ScrollAmount_SmallDecrement
                } else {
                    ScrollAmount_NoAmount
                }
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let steps = dx.abs().max(dy.abs()).ceil().clamp(1.0, 50.0) as u32;
            for _ in 0..steps {
                p.Scroll(amount(dx), amount(dy)).map_err(acc("Scroll"))?;
            }
        }
        Ok(())
    }

    pub fn act(&mut self, action: &Action) -> Res<ActionResult> {
        match action {
            Action::Click { target, method } => match (target, method) {
                (Target::ByIndex { index, generation }, ClickMethod::Accessibility) => {
                    let r = self.resolve(*index, *generation)?;
                    if !Self::pattern_click(&r.element)? {
                        return Err(DesktopError::Unsupported(
                            "element has no invoke/toggle/select/expand pattern; \
                             retry with method=global to click its centre"
                                .into(),
                        ));
                    }
                }
                (_, ClickMethod::Global) => {
                    let (x, y) = self.point(target)?;
                    input::click_at(x, y)?;
                }
                (Target::ByCoordinate { .. }, ClickMethod::Accessibility) => {
                    return Err(DesktopError::Unsupported(
                        "accessibility clicks need an element index; use method=global for coordinates"
                            .into(),
                    ));
                }
                (_, ClickMethod::AppPost) => {
                    return Err(DesktopError::Unsupported(
                        "app_post click is not implemented on Windows yet".into(),
                    ));
                }
            },
            Action::TypeText { text } => input::type_text(text)?,
            Action::SetValue { target, value } => match target {
                Target::ByIndex { index, generation } => {
                    let r = self.resolve(*index, *generation)?;
                    Self::set_value(&r.element, value)?;
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
            } => match target {
                Target::ByIndex { index, generation } => {
                    let r = self.resolve(*index, *generation)?;
                    Self::scroll(&r.element, *delta_x, *delta_y)?;
                }
                Target::ByCoordinate { .. } => {
                    return Err(DesktopError::Unsupported(
                        "scroll needs an element index".into(),
                    ));
                }
            },
            Action::KeyPress { keys } => input::key_chord(keys)?,
            Action::Drag { from, to, method } => {
                if *method != ClickMethod::Global {
                    return Err(DesktopError::Unsupported(
                        "drag requires method=global".into(),
                    ));
                }
                let a = self.point(from)?;
                let b = self.point(to)?;
                input::drag(a, b)?;
            }
        }
        Ok(ActionResult {
            dispatched: true,
            verified: None,
            new_snapshot: None,
        })
    }
}

/// Read one cached element into a [`RawNode`]. Every property read is
/// best-effort: a failing read yields a neutral default rather than an error,
/// because elements can vanish mid-walk.
fn read_raw(el: &IUIAutomationElement) -> RawNode {
    // SAFETY: cached property/pattern reads on elements returned by
    // FindAllBuildCache with the cache request configured in `Worker::new`.
    unsafe {
        let role = pure::role_name(el.CachedControlType().map_or(0, |c| c.0));
        let name = el.CachedName().map(|b| b.to_string()).unwrap_or_default();
        let bounds = el.CachedBoundingRectangle().ok().and_then(|r| {
            (r.right > r.left && r.bottom > r.top).then(|| Rect {
                x: f64::from(r.left),
                y: f64::from(r.top),
                width: f64::from(r.right - r.left),
                height: f64::from(r.bottom - r.top),
            })
        });
        let mut pattern_states: Vec<&'static str> = Vec::new();
        if let Ok(p) = el.GetCachedPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
            match p.CachedToggleState() {
                Ok(s) if s == ToggleState_On => pattern_states.push("checked"),
                Ok(s) if s == ToggleState_Indeterminate => pattern_states.push("indeterminate"),
                _ => {}
            }
        }
        if let Ok(p) =
            el.GetCachedPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            && p.CachedIsSelected().is_ok_and(BOOL::as_bool)
        {
            pattern_states.push("selected");
        }
        if let Ok(p) =
            el.GetCachedPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
            && let Ok(s) = p.CachedExpandCollapseState()
        {
            pattern_states.push(if s == ExpandCollapseState_Collapsed {
                "collapsed"
            } else {
                "expanded"
            });
        }
        let value = el
            .GetCachedPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()
            .and_then(|p| p.CachedValue().ok())
            .map(|b| b.to_string())
            .filter(|s| !s.is_empty());
        RawNode {
            role,
            name,
            value,
            bounds,
            offscreen: el.CachedIsOffscreen().is_ok_and(BOOL::as_bool),
            enabled: el.CachedIsEnabled().map_or(true, BOOL::as_bool),
            focused: el.CachedHasKeyboardFocus().is_ok_and(BOOL::as_bool),
            password: el.CachedIsPassword().is_ok_and(BOOL::as_bool),
            pattern_states,
        }
    }
}
