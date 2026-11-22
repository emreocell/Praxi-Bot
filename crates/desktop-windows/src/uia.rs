//! UI Automation worker: lives on the dedicated STA thread and owns every COM
//! object. Nothing in this module is `Send`.
#![allow(unsafe_code)]

use desktop_api::{
    ActGuard, DesktopError, InputTarget, Refusal,
    action::{Action, ActionResult, ClickMethod, CoordinateSpace, Target},
    guarded,
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
            CUIAutomation, ExpandCollapseState_Collapsed, IUIAutomation, IUIAutomation2,
            IUIAutomationCacheRequest, IUIAutomationElement, IUIAutomationExpandCollapsePattern,
            IUIAutomationInvokePattern, IUIAutomationScrollPattern,
            IUIAutomationSelectionItemPattern, IUIAutomationTextPattern,
            IUIAutomationTogglePattern, IUIAutomationValuePattern, IUIAutomationWindowPattern,
            ScrollAmount_NoAmount, ScrollAmount_SmallDecrement, ScrollAmount_SmallIncrement,
            ToggleState_Indeterminate, ToggleState_On, TreeScope_Subtree,
            UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId,
            UIA_ExpandCollapseExpandCollapseStatePropertyId, UIA_ExpandCollapsePatternId,
            UIA_HasKeyboardFocusPropertyId, UIA_InvokePatternId, UIA_IsEnabledPropertyId,
            UIA_IsOffscreenPropertyId, UIA_IsPasswordPropertyId, UIA_NamePropertyId,
            UIA_ScrollPatternId, UIA_SelectionItemIsSelectedPropertyId, UIA_SelectionItemPatternId,
            UIA_TextPatternId, UIA_TogglePatternId, UIA_ToggleToggleStatePropertyId,
            UIA_ValuePatternId, UIA_ValueValuePropertyId, UIA_WindowPatternId,
        },
        UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE},
    },
    core::{BOOL, BSTR, Interface},
};

use crate::{
    guard_glue::{self as glue, ElementRaw},
    identity, input,
    pure::{self, MAX_NODES, RawNode},
    winlist,
};

type Res<T> = Result<T, DesktopError>;

/// How long UIA waits to reach a target process (the system default).
const CONNECTION_TIMEOUT_MS: u32 = 2_000;
/// How long UIA waits for one cross-process call to finish (default 20 000).
const TRANSACTION_TIMEOUT_MS: u32 = 8_000;

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
    // NOTE(concurrency): The backend currently retains only ONE global generation and element
    // tree across all runs. If run B takes a snapshot while run A is between get_state and action,
    // run A's generation will become stale and its action will fail closed with ElementNotFound.
    // See tasks/060-desktop-per-run-trees.md for the planned follow-up (per-run retained trees or
    // single-flight desktop actions).
    generation: u64,
    retained: Vec<Retained>,
    hwnd: Option<HWND>,
    last_focused: Option<IUIAutomationElement>,
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
            // A target whose UI thread is blocked (modal dialog, hung app) would
            // otherwise stall this single UIA thread for the 20 s default of
            // every cross-process call, and every queued tool call behind it.
            // Best effort: older systems lack IUIAutomation2.
            if let Ok(uia2) = uia.cast::<IUIAutomation2>() {
                let _ = uia2.SetConnectionTimeout(CONNECTION_TIMEOUT_MS);
                let _ = uia2.SetTransactionTimeout(TRANSACTION_TIMEOUT_MS);
            }
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
                last_focused: None,
            })
        }
    }

    /// Release every COM object, then uninitialise COM (STA thread exit).
    pub fn close(self) {
        let Self {
            uia,
            cache,
            retained,
            last_focused,
            ..
        } = self;
        drop(last_focused);
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
    /// `ExpandCollapse`. Returns `(false, None)` when none is supported, or
    /// `(true, verified)` when dispatched.
    fn pattern_click(el: &IUIAutomationElement) -> Res<(bool, Option<bool>)> {
        // SAFETY: pattern queries and invocations on a live element.
        unsafe {
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            {
                p.Invoke().map_err(acc("Invoke"))?;
                return Ok((true, None));
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            {
                let before = p.CurrentToggleState().ok().map(|s| s.0);
                p.Toggle().map_err(acc("Toggle"))?;
                let after = p.CurrentToggleState().ok().map(|s| s.0);
                return Ok((true, Some(verify_toggle(before, after))));
            }
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                UIA_SelectionItemPatternId,
            ) {
                p.Select().map_err(acc("Select"))?;
                return Ok((true, None));
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
                return Ok((true, None));
            }
        }
        Ok((false, None))
    }

    fn set_value(el: &IUIAutomationElement, value: &str) -> Res<Option<bool>> {
        // SAFETY: pattern queries and calls on a live element.
        unsafe {
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                && !p.CurrentIsReadOnly().map_err(acc("IsReadOnly"))?.as_bool()
            {
                p.SetValue(&BSTR::from(value)).map_err(acc("SetValue"))?;
                let actual = p.CurrentValue().ok().map(|b| b.to_string());
                return Ok(Some(verify_value(value, actual.as_deref())));
            }
            // Fallback: focus, select all, type.
            el.SetFocus().map_err(acc("SetFocus"))?;
        }
        input::key_chord(&["ctrl".to_string(), "a".to_string()])?;
        input::type_text(value)?;
        // SAFETY: optional read-back if ValuePattern is exposed.
        let actual = unsafe {
            el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|p| p.CurrentValue().ok())
                .map(|b| b.to_string())
        };
        Ok(Some(verify_value(value, actual.as_deref())))
    }

    /// `ValuePattern.SetValue` only, no typing fallback: the guarded path sends
    /// nothing through the keyboard for a value it cannot set directly.
    fn set_value_pattern_only(el: &IUIAutomationElement, value: &str) -> Result<bool, String> {
        // SAFETY: pattern queries and calls on a live element.
        unsafe {
            let p = el
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .map_err(|e| format!("this element has no value pattern: {e}"))?;
            if p.CurrentIsReadOnly().map_err(|e| e.to_string())?.as_bool() {
                return Err("this element is read-only".into());
            }
            p.SetValue(&BSTR::from(value)).map_err(|e| e.to_string())?;
            let actual = p.CurrentValue().ok().map(|b| b.to_string());
            Ok(verify_value(value, actual.as_deref()))
        }
    }

    /// Fallback when `SendInput` is blocked (e.g. UIPI / non-interactive desktop):
    /// append `text` to the focused element's `ValuePattern`.
    fn uia_type_fallback(&self, text: &str) -> Res<()> {
        // SAFETY: reads the live focused element on the STA thread.
        let el = unsafe { self.uia.GetFocusedElement() }.map_err(acc("GetFocusedElement"))?;
        Self::refuse_password(&el)?;
        // SAFETY: COM calls on a live element owned by this STA thread.
        unsafe {
            let p = el
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .map_err(acc("ValuePattern"))?;
            let cur = p
                .CurrentValue()
                .ok()
                .map(|b| b.to_string())
                .unwrap_or_default();
            let next = format!("{cur}{text}");
            p.SetValue(&BSTR::from(next.as_str()))
                .map_err(acc("SetValue"))
        }
    }

    /// Fallback for key chords when `SendInput` is blocked by UIPI.
    fn uia_key_fallback(&self, keys: &[String]) -> Res<()> {
        let lower: Vec<String> = keys.iter().map(|k| k.trim().to_ascii_lowercase()).collect();
        match lower.as_slice() {
            [a, b] if (a == "ctrl" || a == "control") && b == "a" => Ok(()),
            [k] if k == "delete" || k == "del" || k == "backspace" => {
                if let Some(el) = self.focused_element() {
                    // SAFETY: COM call on a live element owned by this thread.
                    unsafe {
                        if let Ok(p) =
                            el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                        {
                            let _ = p.SetValue(&BSTR::from(""));
                        }
                    }
                }
                Ok(())
            }
            [a, b] if (a == "alt" || a == "menu") && b == "f4" => {
                let hwnd = self.hwnd.or_else(winlist::foreground_hwnd).ok_or_else(|| {
                    DesktopError::Accessibility("no target window to close".into())
                })?;
                // SAFETY: COM / Win32 close on the target window handle.
                unsafe {
                    if let Ok(root) = self.uia.ElementFromHandle(hwnd)
                        && let Ok(wp) = root
                            .GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
                        && wp.Close().is_ok()
                    {
                        return Ok(());
                    }
                    PostMessageW(
                        Some(hwnd),
                        WM_CLOSE,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    )
                    .map_err(acc("PostMessageW(WM_CLOSE)"))
                }
            }
            _ => Err(DesktopError::Accessibility(format!(
                "SendInput blocked and no UIA fallback for key chord {keys:?}"
            ))),
        }
    }

    fn focused_element(&self) -> Option<IUIAutomationElement> {
        if let Some(el) = &self.last_focused {
            return Some(el.clone());
        }
        // SAFETY: COM query on the STA thread's IUIAutomation instance.
        unsafe { self.uia.GetFocusedElement().ok() }
            .or_else(|| self.retained.first().map(|r| r.element.clone()))
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

    /// Keyboard input goes to the foreground window: bring the window of the
    /// last snapshot forward if another application took the focus.
    fn focus_target(&self) {
        if let Some(h) = self.hwnd {
            winlist::ensure_input_target(h);
        }
    }

    fn click(&mut self, target: &Target, method: ClickMethod) -> Res<Option<bool>> {
        match (target, method) {
            (Target::ByIndex { index, generation }, ClickMethod::Accessibility) => {
                let r = self.resolve(*index, *generation)?;
                let el = r.element.clone();
                let (ok, ver) = Self::pattern_click(&el)?;
                if !ok {
                    return Err(DesktopError::Unsupported(
                        "element has no invoke/toggle/select/expand pattern; \
                         retry with method=global to click its centre"
                            .into(),
                    ));
                }
                self.last_focused = Some(el);
                Ok(ver)
            }
            (_, ClickMethod::Global) => {
                let resolved_el = match *target {
                    Target::ByIndex { index, generation } => {
                        Some(self.resolve(index, generation)?.element.clone())
                    }
                    Target::ByCoordinate { .. } => None,
                };
                let (x, y) = self.point(target)?;
                if let Err(e) = input::click_at(x, y) {
                    if let Some(el) = &resolved_el {
                        // Fallback to UIA focus / pattern invoke when SendInput is blocked.
                        // SAFETY: COM call on a live element owned by this thread.
                        let focused = unsafe { el.SetFocus().is_ok() };
                        let patterned = Self::pattern_click(el).is_ok_and(|(ok, _)| ok);
                        if !focused && !patterned {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                }
                if let Some(el) = resolved_el {
                    self.last_focused = Some(el);
                }
                Ok(None)
            }
            (Target::ByCoordinate { .. }, ClickMethod::Accessibility) => {
                Err(DesktopError::Unsupported(
                    "accessibility clicks need an element index; use method=global for coordinates"
                        .into(),
                ))
            }
            (_, ClickMethod::AppPost) => Err(DesktopError::Unsupported(
                "app_post click is not implemented on Windows yet".into(),
            )),
        }
    }

    pub fn act(&mut self, action: &Action) -> Res<ActionResult> {
        self.act_with(action, None)
    }

    // ── run-scoped control grant: facts about the real receiver ──────────────

    /// The raw live properties of one UIA element (never the snapshot cache).
    fn element_raw(&self, el: &IUIAutomationElement) -> ElementRaw<HWND> {
        // SAFETY: live COM property reads on an element owned by this thread.
        unsafe {
            ElementRaw {
                pid: el.CurrentProcessId().map_err(|_| Refusal::NoIdentity),
                password: el
                    .CurrentIsPassword()
                    .map(BOOL::as_bool)
                    .map_err(|_| Refusal::PasswordField),
                native: self.element_native(el),
            }
        }
    }

    /// UIA-only controls share their containing native window. Walk their live
    /// ancestors without substituting a foreground window or crossing processes.
    fn element_native(&self, el: &IUIAutomationElement) -> Result<Option<HWND>, Refusal> {
        // SAFETY: all COM objects belong to this STA thread.
        unsafe {
            let identity = |_| Refusal::NoIdentity;
            let native = el.CurrentNativeWindowHandle().map_err(identity)?;
            if !native.0.is_null() {
                return Ok(Some(native));
            }
            let process_id = el.CurrentProcessId().map_err(identity)?;
            let pid = u32::try_from(process_id).map_err(|_| Refusal::NoIdentity)?;
            let walker = self.uia.RawViewWalker().map_err(identity)?;
            let mut ancestor = el.clone();
            for _ in 0..pure::MAX_DEPTH {
                ancestor = walker.GetParentElement(&ancestor).map_err(identity)?;
                if ancestor.CurrentProcessId().map_err(identity)? != process_id {
                    return Err(Refusal::NoIdentity);
                }
                let native = ancestor.CurrentNativeWindowHandle().map_err(identity)?;
                if !native.0.is_null() {
                    return (winlist::window_pid(native) == pid)
                        .then_some(Some(native))
                        .ok_or(Refusal::NoIdentity);
                }
            }
            Ok(None)
        }
    }

    /// The live focused element, raw.
    fn focused_raw(&self) -> Result<ElementRaw<HWND>, Refusal> {
        // SAFETY: COM call on the STA thread's IUIAutomation instance.
        let focused = unsafe { self.uia.GetFocusedElement() }.map_err(|_| Refusal::NoIdentity)?;
        Ok(self.element_raw(&focused))
    }

    fn refuse_password(el: &IUIAutomationElement) -> Res<()> {
        // SAFETY: reads the live property on the STA thread.
        if unsafe { el.CurrentIsPassword() }.is_ok_and(|password| !password.as_bool()) {
            return Ok(());
        }
        Err(DesktopError::GuardRefused(
            Refusal::PasswordField.why().into(),
        ))
    }

    fn refuse_focused_password(&self) -> Res<()> {
        // SAFETY: reads the current keyboard focus, independent of cached elements.
        let focused = unsafe { self.uia.GetFocusedElement() }.map_err(acc("GetFocusedElement"))?;
        Self::refuse_password(&focused)
    }

    /// The facts that must all pass the guard before keyboard input: the
    /// foreground window and the live focused element.
    fn keyboard_facts(&self) -> Result<Vec<InputTarget>, Refusal> {
        glue::keyboard_facts(&RealOs, self.focused_raw())
    }

    /// The window and program keyboard input would reach now. Mirrors
    /// [`Self::focus_target`]: another program's foreground is replaced by the
    /// last snapshot's window, the same program's own dialog keeps it. When the
    /// foreground is the target, the live focused element must be the same
    /// program too, otherwise no identity is reported (no grant is offered).
    pub fn input_target(&self) -> Option<InputTarget> {
        let fg = winlist::foreground_hwnd();
        let (hwnd, is_fg) = match (fg, self.hwnd) {
            (Some(f), Some(s)) if f == s || winlist::window_pid(f) == winlist::window_pid(s) => {
                (f, true)
            }
            (Some(_), Some(s)) => (s, false),
            (Some(f), None) => (f, true),
            // No foreground to read: no identity, no grant offered.
            (None, _) => return None,
        };
        let mut t = identity::top_level_facts(hwnd)?;
        if is_fg {
            let facts = self.keyboard_facts().ok()?;
            let element = facts.into_iter().nth(1)?;
            if !t.identity.same_program(&element.identity) || element.owned != t.owned {
                return None;
            }
            t.password_focus = element.password_focus;
        }
        Some(t)
    }

    #[allow(clippy::too_many_lines)] // one arm per action kind
    pub fn act_with(&mut self, action: &Action, guard: Option<&ActGuard>) -> Res<ActionResult> {
        if let Some(g) = guard {
            return self.act_guarded_flow(action, g);
        }
        let mut verified = None;
        match action {
            Action::Click { target, method } => {
                verified = self.click(target, *method)?;
            }
            Action::TypeText { text } => {
                self.focus_target();
                self.refuse_focused_password()?;
                if let Err(e) = input::type_text(text) {
                    self.uia_type_fallback(text).map_err(|_| e)?;
                }
            }
            Action::SetValue { target, value } => match target {
                Target::ByIndex { index, generation } => {
                    let r = self.resolve(*index, *generation)?;
                    let el = r.element.clone();
                    Self::refuse_password(&el)?;
                    verified = Self::set_value(&el, value)?;
                    self.last_focused = Some(el);
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
                    input::scroll_at(self.point(target)?, *delta_x, *delta_y)?;
                }
            },
            Action::KeyPress { keys } => {
                self.focus_target();
                self.refuse_focused_password()?;
                if let Err(e) = input::key_chord(keys) {
                    self.uia_key_fallback(keys).map_err(|_| e)?;
                }
            }
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
            verified,
            new_snapshot: None,
        })
    }

    pub fn act_approved(&mut self, action: &Action, guard: &ActGuard) -> Res<ActionResult> {
        let facts = match action {
            Action::Click {
                target,
                method: ClickMethod::Accessibility,
            }
            | Action::Scroll { target, .. } => match target {
                Target::ByIndex { index, generation } => {
                    let element = &self.resolve(*index, *generation)?.element;
                    glue::element_fact(&RealOs, self.element_raw(element)).map(|fact| vec![fact])
                }
                Target::ByCoordinate { .. } => {
                    let (x, y) = self.point(target)?;
                    glue::point_facts(&RealOs, x, y)
                }
            },
            _ => return self.act_guarded_flow(action, guard),
        };
        let mut result = None;
        guarded::guarded_write(guard, facts, || {
            result = Some(self.act(action).map_err(|e| e.to_string())?);
            Ok(())
        })
        .map_err(|f| glue::to_error(&f))?;
        result
            .ok_or_else(|| DesktopError::Internal("approved action did not return a result".into()))
    }

    /// The run-scoped control grant's path: type, keys, the focusing click and
    /// `SetValue` only, each through the platform-neutral guarded flow with the
    /// real receivers and the real sink. No UIA fallback exists on this path.
    fn act_guarded_flow(&mut self, action: &Action, g: &ActGuard) -> Res<ActionResult> {
        let mut verified = None;
        let plan = glue::plan(action)?;
        match plan {
            glue::Plan::Click(target) => {
                let p = self.point(target)?;
                self.with_probe(|probe| {
                    guarded::guarded_click(g, p, probe, &mut glue::Sink(Win32Input))
                })
                .map_err(|f| glue::to_error(&f))?;
            }
            glue::Plan::Type(text) => {
                self.focus_target();
                self.with_probe(|probe| {
                    guarded::guarded_type(g, text, probe, &mut glue::Sink(Win32Input))
                })
                .map_err(|f| glue::to_error(&f))?;
            }
            glue::Plan::Key(keys) => {
                self.focus_target();
                self.with_probe(|probe| {
                    guarded::guarded_key(g, keys, probe, &mut glue::Sink(Win32Input))
                })
                .map_err(|f| glue::to_error(&f))?;
            }
            glue::Plan::SetValue {
                index,
                generation,
                value,
            } => {
                let el = self.resolve(index, generation)?.element.clone();
                // Live facts of the very element, plus the foreground and focus
                // facts the keyboard path uses, read right before the write.
                let facts = glue::write_facts(&RealOs, self.element_raw(&el), self.focused_raw());
                guarded::guarded_write(g, facts, || {
                    Self::set_value_pattern_only(&el, value).map(|v| verified = Some(v))
                })
                .map_err(|f| glue::to_error(&f))?;
                self.last_focused = Some(el);
            }
        }
        Ok(ActionResult {
            dispatched: true,
            verified,
            new_snapshot: None,
        })
    }

    fn with_probe<R>(&self, f: impl FnOnce(&mut glue::Probe<'_, RealOs>) -> R) -> R {
        let focused = || self.focused_raw();
        f(&mut glue::Probe {
            os: &RealOs,
            focused: &focused,
            modifiers_down: &input::modifiers_down,
        })
    }
}

/// The real window and process queries.
struct RealOs;

impl glue::WinFacts for RealOs {
    type Window = HWND;

    fn foreground(&self) -> Option<HWND> {
        winlist::foreground_hwnd()
    }

    fn root_of(&self, w: HWND) -> Option<HWND> {
        identity::root_of(w)
    }

    fn top_level_facts(&self, w: HWND) -> Option<InputTarget> {
        identity::top_level_facts(w)
    }

    fn child_facts(&self, w: HWND) -> Option<InputTarget> {
        identity::child_facts(w)
    }

    fn process_identity(&self, pid: u32) -> Option<desktop_api::ProcessIdentity> {
        identity::process_identity(pid)
    }

    fn process_elevated(&self, pid: u32) -> Option<bool> {
        identity::process_elevated(pid)
    }

    fn point_hit(&self, x: f64, y: f64) -> Option<(HWND, HWND)> {
        identity::point_hit(x, y)
    }
}

/// The real `SendInput` calls.
struct Win32Input;

impl glue::InputApi for Win32Input {
    fn type_units(&mut self, units: &[u16]) -> Result<(), DesktopError> {
        input::type_units(units)
    }

    fn key_chord(&mut self, keys: &[String]) -> Result<(), DesktopError> {
        input::key_chord(keys)
    }

    fn click_at(&mut self, x: f64, y: f64) -> Result<(), DesktopError> {
        input::click_at(x, y)
    }

    fn release_all(&mut self) -> bool {
        input::release_all()
    }
}

/// Compare the requested `SetValue` string against `ValuePattern.CurrentValue`.
///
/// Normalises `\r\n` to `\n` because Win32 / `RichEdit` controls convert lone
/// newlines to CRLF on write. Returns `false` (never an error) on mismatch or
/// when `actual` could not be read.
#[must_use]
pub fn verify_value(expected: &str, actual: Option<&str>) -> bool {
    let Some(actual) = actual else {
        return false;
    };
    if actual == expected {
        return true;
    }
    actual.replace("\r\n", "\n") == expected.replace("\r\n", "\n")
}

/// Compare `TogglePattern.CurrentToggleState` before and after `Toggle()`.
///
/// Returns `true` when both states were read and the state changed.
#[must_use]
pub fn verify_toggle(before: Option<i32>, after: Option<i32>) -> bool {
    matches!((before, after), (Some(b), Some(a)) if b != a)
}

/// Body text of a document or edit that does not expose `ValuePattern`.
///
/// Windows 11 Notepad's editor is a document whose text is only available
/// through `TextPattern`. Capped so a large buffer cannot dominate a snapshot.
fn text_pattern_excerpt(el: &IUIAutomationElement, role: &str) -> Option<String> {
    if role != "document" && role != "edit" {
        return None;
    }
    // SAFETY: current (not cached) pattern query on an element from this walk.
    // Text ranges are not part of the cache request.
    unsafe {
        let pattern = el
            .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .ok()?;
        let text = pattern
            .DocumentRange()
            .ok()?
            .GetText(2_000)
            .ok()?
            .to_string();
        if text.chars().any(|c| !c.is_whitespace()) {
            Some(text)
        } else {
            None
        }
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
            .filter(|s| !s.is_empty())
            .or_else(|| text_pattern_excerpt(el, role));
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

#[cfg(test)]
mod tests {
    use super::{verify_toggle, verify_value};

    #[test]
    fn verify_value_matches_exact_and_crlf_normalised() {
        assert!(verify_value("hello", Some("hello")));
        assert!(verify_value("", Some("")));
        assert!(verify_value("line1\nline2", Some("line1\r\nline2")));
        assert!(verify_value("line1\r\nline2", Some("line1\nline2")));
        assert!(!verify_value("hello", Some("world")));
        assert!(!verify_value("hello", Some("hello!")));
        assert!(!verify_value("hello", None));
    }

    #[test]
    fn verify_toggle_detects_state_transitions() {
        // Off (0) -> On (1), On (1) -> Off (0), On (1) -> Indeterminate (2)
        assert!(verify_toggle(Some(0), Some(1)));
        assert!(verify_toggle(Some(1), Some(0)));
        assert!(verify_toggle(Some(1), Some(2)));
        assert!(!verify_toggle(Some(0), Some(0)));
        assert!(!verify_toggle(Some(1), Some(1)));
        assert!(!verify_toggle(None, Some(1)));
        assert!(!verify_toggle(Some(0), None));
        assert!(!verify_toggle(None, None));
    }
}
