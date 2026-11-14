//! Fake backends, tools and gates for the unit tests.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use builtin_tools::{BrowserDriver, GateRequest, ToolGate, Wait};
use desktop_api::snapshot::{
    AppSpec, Image, Pid, Quality, Rect, Region, TreeQuality, UiNode, WindowInfo,
};
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Snapshot, Target,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

pub fn window(id: u64, title: &str) -> WindowInfo {
    WindowInfo {
        id,
        title: title.into(),
        pid: 100 + u32::try_from(id).unwrap(),
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        focused: true,
    }
}

pub fn node(index: u32, role: &str, name: &str, x: f64, y: f64) -> UiNode {
    UiNode::new(
        index,
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

/// A scripted desktop: fixed windows and one window's tree.
pub struct FakeDesktop {
    pub windows: Mutex<Vec<WindowInfo>>,
    pub nodes: Mutex<Vec<UiNode>>,
    pub actions: Mutex<Vec<Action>>,
    pub launched: Mutex<Vec<String>>,
    /// Window that appears when something is launched.
    pub on_launch: Mutex<Option<WindowInfo>>,
    generation: Mutex<u64>,
}

impl FakeDesktop {
    pub fn new(windows: Vec<WindowInfo>, nodes: Vec<UiNode>) -> Arc<Self> {
        Arc::new(Self {
            windows: Mutex::new(windows),
            nodes: Mutex::new(nodes),
            actions: Mutex::new(Vec::new()),
            launched: Mutex::new(Vec::new()),
            on_launch: Mutex::new(None),
            generation: Mutex::new(0),
        })
    }
}

#[async_trait]
impl DesktopBackend for FakeDesktop {
    async fn snapshot(&self, scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        let mut g = self.generation.lock();
        *g += 1;
        let wins = self.windows.lock();
        let win = scope
            .and_then(|id| wins.iter().find(|w| w.id == id))
            .or_else(|| wins.first())
            .cloned()
            .ok_or_else(|| DesktopError::Accessibility("no window".into()))?;
        Ok(Snapshot {
            window: win,
            nodes: self.nodes.lock().clone(),
            generation: *g,
            quality: TreeQuality::Good,
        })
    }

    async fn act(&self, action: Action) -> Result<ActionResult, DesktopError> {
        let gen_now = *self.generation.lock();
        let check = |t: &Target| match t {
            Target::ByIndex { index, generation } if *generation != gen_now => {
                Err(DesktopError::ElementNotFound {
                    index: *index,
                    generation: *generation,
                })
            }
            _ => Ok(()),
        };
        match &action {
            Action::Click { target, .. }
            | Action::SetValue { target, .. }
            | Action::Scroll { target, .. } => check(target)?,
            _ => {}
        }
        self.actions.lock().push(action);
        Ok(ActionResult {
            dispatched: true,
            verified: None,
            new_snapshot: None,
        })
    }

    async fn screenshot(&self, _r: Region, _q: Quality) -> Result<Image, DesktopError> {
        Err(DesktopError::Unsupported("fake".into()))
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        Ok(self.windows.lock().clone())
    }

    async fn launch(&self, app: AppSpec) -> Result<Pid, DesktopError> {
        self.launched.lock().push(app.executable);
        if let Some(w) = self.on_launch.lock().take() {
            self.windows.lock().push(w);
        }
        Ok(4242)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: "fake".into(),
            accessibility_tree: true,
            screenshot: false,
            app_launch: true,
            has_display: true,
        }
    }
}

/// A scripted browser: one page text; calls are logged.
pub struct FakeBrowser {
    pub page: Mutex<String>,
    pub calls: Mutex<Vec<String>>,
    pub eval_result: Mutex<Value>,
}

impl FakeBrowser {
    pub fn new(page: &str) -> Arc<Self> {
        Arc::new(Self {
            page: Mutex::new(page.to_owned()),
            calls: Mutex::new(Vec::new()),
            eval_result: Mutex::new(json!(1)),
        })
    }

    fn rec(&self, s: String) -> Result<String, String> {
        self.calls.lock().push(s);
        Ok("ok".into())
    }
}

#[async_trait]
impl BrowserDriver for FakeBrowser {
    async fn open(&self, url: &str, _profile: Option<&str>) -> Result<String, String> {
        self.rec(format!("open:{url}"))?;
        Ok(self.page.lock().clone())
    }
    async fn snapshot(&self, _i: bool) -> Result<String, String> {
        Ok(self.page.lock().clone())
    }
    async fn click(&self, r: &str) -> Result<String, String> {
        self.rec(format!("click:{r}"))
    }
    async fn fill(&self, r: &str, t: &str) -> Result<String, String> {
        self.rec(format!("fill:{r}:{t}"))
    }
    async fn select(&self, r: &str, v: &str) -> Result<String, String> {
        self.rec(format!("select:{r}:{v}"))
    }
    async fn press(&self, k: &str) -> Result<String, String> {
        self.rec(format!("press:{k}"))
    }
    async fn scroll(&self, _r: Option<&str>, _dx: f64, _dy: f64) -> Result<String, String> {
        self.rec("scroll".into())
    }
    async fn wait(&self, c: Wait, _t: Option<Duration>) -> Result<String, String> {
        self.rec(format!("wait:{c:?}"))
    }
    async fn back(&self) -> Result<String, String> {
        self.rec("back".into())
    }
    async fn tabs(&self, _s: Option<usize>) -> Result<String, String> {
        self.rec("tabs".into())
    }
    async fn screenshot(
        &self,
        _full: bool,
        _e: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Err("fake".into())
    }
    async fn eval(&self, js: &str) -> Result<Value, String> {
        self.calls.lock().push(format!("eval:{js}"));
        Ok(self.eval_result.lock().clone())
    }
}

/// Allows everything and remembers the requests.
#[derive(Default)]
pub struct RecordingGate {
    pub requests: Mutex<Vec<GateRequest>>,
    pub deny: Mutex<Option<String>>,
}

impl RecordingGate {
    pub fn denying(tool: &str) -> Arc<Self> {
        let g = Self::default();
        *g.deny.lock() = Some(tool.to_owned());
        Arc::new(g)
    }

    pub fn tools(&self) -> Vec<String> {
        self.requests
            .lock()
            .iter()
            .map(|r| r.tool.clone())
            .collect()
    }
}

#[async_trait]
impl ToolGate for RecordingGate {
    async fn authorize(&self, req: &GateRequest) -> Result<(), String> {
        self.requests.lock().push(req.clone());
        if self.deny.lock().as_deref() == Some(req.tool.as_str()) {
            return Err(format!("permission denied: {} was refused", req.tool));
        }
        Ok(())
    }
}

/// A tool that stores the inputs it was called with.
pub struct CaptureTool {
    spec: ToolSpec,
    pub inputs: Mutex<Vec<Value>>,
}

impl CaptureTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            spec: ToolSpec {
                name: name.into(),
                description: "capture".into(),
                input_schema: json!({"type": "object"}),
                effect: Effect::Write,
                category: PermissionCategory::Fs,
                platforms: vec![Platform::All],

                content: tool_registry::ContentClass::none(),
            },
            inputs: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Tool for CaptureTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        self.inputs.lock().push(input);
        Ok(ToolOutput::text("captured"))
    }
}
