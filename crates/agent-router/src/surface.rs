//! The model-facing tool surface: direct tools below a threshold, discovery
//! meta-tools (`find_tool` / `get_tool_details` / `use_tool`) above it.
//!
//! Large tool sets (typically MCP servers) are never dumped into the model's
//! context.  Instead the model sees three small tools and navigates the
//! registry via BM25 search.  Per ADR-0005 the router only *narrows*: the model
//! can always reach any registered tool through `find_tool`.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tool_registry::{
    ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput,
    ToolRegistry, ToolSpec,
};

use crate::discovery::ToolIndex;

/// Name of the search meta-tool.
pub const FIND_TOOL: &str = "find_tool";
/// Name of the schema-lookup meta-tool.
pub const GET_TOOL_DETAILS: &str = "get_tool_details";
/// Name of the dispatch meta-tool.
pub const USE_TOOL: &str = "use_tool";

/// Registry size above which discovery meta-tools replace direct exposure.
pub const DEFAULT_DISCOVERY_THRESHOLD: usize = 25;
/// Default number of results of `find_tool`.
pub const DEFAULT_FIND_K: usize = 5;
/// Upper bound on `k` accepted by `find_tool`.
pub const MAX_FIND_K: usize = 25;

fn is_meta(name: &str) -> bool {
    matches!(name, FIND_TOOL | GET_TOOL_DETAILS | USE_TOOL)
}

/// Per-call visibility of a tool: `(call context, tool spec) -> visible`.
///
/// The discovery meta-tools consult it so `find_tool` / `get_tool_details` /
/// `use_tool` only reveal tools the *calling bot* may see (the context carries
/// the run id the loop resolves the bot from).
pub type ToolVisibility = Arc<dyn Fn(&ToolContext, &ToolSpec) -> bool + Send + Sync>;

/// Shared state of the three meta-tools: the registry and a cached BM25 index
/// that is rebuilt only when [`ToolRegistry::generation`] changes.
struct Discovery {
    registry: ToolRegistry,
    cache: Mutex<Option<(u64, Arc<ToolIndex>)>>,
    visibility: Option<ToolVisibility>,
}

impl Discovery {
    fn visible(&self, ctx: &ToolContext, spec: &ToolSpec) -> bool {
        self.visibility.as_ref().is_none_or(|f| f(ctx, spec))
    }

    fn index(&self) -> Arc<ToolIndex> {
        let generation = self.registry.generation();
        let mut cache = self.cache.lock();
        if let Some((cached_gen, index)) = cache.as_ref()
            && *cached_gen == generation
        {
            return Arc::clone(index);
        }
        let specs: Vec<ToolSpec> = self
            .registry
            .list()
            .into_iter()
            .filter(|s| !is_meta(&s.name))
            .collect();
        let index = Arc::new(ToolIndex::build(&specs));
        *cache = Some((generation, Arc::clone(&index)));
        index
    }
}

fn meta_spec(name: &str, description: &str, effect: Effect, schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: schema,
        effect,
        category: PermissionCategory::Meta,
        platforms: vec![Platform::All],
    }
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidInput(format!("`{key}` must be a string")))
}

struct FindTool {
    spec: ToolSpec,
    discovery: Arc<Discovery>,
}

#[async_trait]
impl Tool for FindTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let query = str_arg(&input, "query")?;
        let k = input
            .get("k")
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| ToolError::InvalidInput("`k` must be a positive integer".into()))
            })
            .transpose()?
            .map_or(DEFAULT_FIND_K, |k| {
                usize::try_from(k)
                    .unwrap_or(MAX_FIND_K)
                    .clamp(1, MAX_FIND_K)
            });
        let index = self.discovery.index();
        // The index is shared by all bots: rank everything, then drop what this
        // caller may not see, then cut to `k`.
        let hits: Vec<Value> = index
            .search(query, index.len())
            .into_iter()
            .filter(|h| {
                self.discovery
                    .registry
                    .get(&h.name)
                    .is_some_and(|t| self.discovery.visible(ctx, t.spec()))
            })
            .take(k)
            .map(|h| json!({ "name": h.name, "description": h.description, "score": h.score }))
            .collect();
        Ok(ToolOutput {
            content: vec![ContentPart::Json {
                data: Value::Array(hits),
            }],
            spill: None,
        })
    }
}

struct GetToolDetails {
    spec: ToolSpec,
    discovery: Arc<Discovery>,
}

#[async_trait]
impl Tool for GetToolDetails {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let name = str_arg(&input, "name")?;
        let target = lookup(&self.discovery, ctx, name)?;
        let spec = target.spec();
        Ok(ToolOutput {
            content: vec![ContentPart::Json {
                data: json!({
                    "name": spec.name,
                    "description": spec.description,
                    "input_schema": spec.input_schema,
                    "effect": spec.effect,
                }),
            }],
            spill: None,
        })
    }
}

struct UseTool {
    spec: ToolSpec,
    discovery: Arc<Discovery>,
}

#[async_trait]
impl Tool for UseTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let name = str_arg(&input, "name")?;
        let target = lookup(&self.discovery, ctx, name)?;
        let arguments = input.get("input").cloned().unwrap_or_else(|| json!({}));
        target.call(ctx, arguments).await
    }
}

/// Resolve a non-meta tool by name for the discovery meta-tools.  A tool the
/// caller may not see is reported exactly like a missing one.
fn lookup(
    discovery: &Discovery,
    ctx: &ToolContext,
    name: &str,
) -> Result<Arc<dyn Tool>, ToolError> {
    if is_meta(name) {
        return Err(ToolError::InvalidInput(format!(
            "`{name}` is a discovery tool; use find_tool to locate a real tool"
        )));
    }
    discovery
        .registry
        .get(name)
        .filter(|t| discovery.visible(ctx, t.spec()))
        .ok_or_else(|| {
            ToolError::InvalidInput(format!(
                "unknown tool `{name}`; use find_tool to search for the right name"
            ))
        })
}

/// How tools are currently presented to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceMode {
    /// Every registered tool is passed to the model directly.
    Direct,
    /// Only the three discovery meta-tools are passed.
    Discovery,
}

/// Decides what the model sees and routes its tool calls.
///
/// The agent loop uses it as follows:
/// 1. [`ToolSurface::specs`] → tool definitions for the model request.
/// 2. [`ToolSurface::effective_spec`] → the spec to run permission checks
///    against (for `use_tool` this is the *target* tool's spec, so a read-only
///    tool behind `use_tool` is not treated as destructive).
/// 3. [`ToolSurface::resolve`] → the tool to execute.
pub struct ToolSurface {
    discovery: Arc<Discovery>,
    threshold: usize,
    meta: [Arc<dyn Tool>; 3],
}

impl ToolSurface {
    /// Create a surface over `registry` that switches to discovery when the
    /// registry holds **more than** `threshold` tools.
    #[must_use]
    pub fn new(registry: ToolRegistry, threshold: usize) -> Self {
        Self::build(registry, threshold, None)
    }

    /// Like [`Self::new`], but the discovery meta-tools only reveal / run tools
    /// for which `visibility` returns `true` for the calling context.
    #[must_use]
    pub fn with_visibility(
        registry: ToolRegistry,
        threshold: usize,
        visibility: ToolVisibility,
    ) -> Self {
        Self::build(registry, threshold, Some(visibility))
    }

    fn build(registry: ToolRegistry, threshold: usize, visibility: Option<ToolVisibility>) -> Self {
        let discovery = Arc::new(Discovery {
            registry,
            cache: Mutex::new(None),
            visibility,
        });
        let find: Arc<dyn Tool> = Arc::new(FindTool {
            spec: meta_spec(
                FIND_TOOL,
                "Search the available tools by keyword. Returns the best matching tool names \
                 with descriptions. Call this first, then get_tool_details, then use_tool.",
                Effect::Read,
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What you want to do" },
                        "k": { "type": "integer", "description": "Max results (default 5)" }
                    },
                    "required": ["query"]
                }),
            ),
            discovery: Arc::clone(&discovery),
        });
        let details: Arc<dyn Tool> = Arc::new(GetToolDetails {
            spec: meta_spec(
                GET_TOOL_DETAILS,
                "Get the full description and JSON input schema of one tool found via find_tool.",
                Effect::Read,
                json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }),
            ),
            discovery: Arc::clone(&discovery),
        });
        let use_tool: Arc<dyn Tool> = Arc::new(UseTool {
            spec: meta_spec(
                USE_TOOL,
                "Execute a tool found via find_tool with the given input object.",
                // Worst case; permission checks use `effective_spec` instead.
                Effect::Destructive,
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "input": { "type": "object", "description": "Arguments for the tool" }
                    },
                    "required": ["name"]
                }),
            ),
            discovery: Arc::clone(&discovery),
        });
        Self {
            discovery,
            threshold,
            meta: [find, details, use_tool],
        }
    }

    /// A surface with [`DEFAULT_DISCOVERY_THRESHOLD`].
    #[must_use]
    pub fn with_default_threshold(registry: ToolRegistry) -> Self {
        Self::new(registry, DEFAULT_DISCOVERY_THRESHOLD)
    }

    /// The registry behind this surface.
    #[must_use]
    pub fn registry(&self) -> &ToolRegistry {
        &self.discovery.registry
    }

    /// Current presentation mode.
    #[must_use]
    pub fn mode(&self) -> SurfaceMode {
        if self.discovery.registry.list().len() > self.threshold {
            SurfaceMode::Discovery
        } else {
            SurfaceMode::Direct
        }
    }

    /// Tool definitions to hand to the model, sorted by name.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        match self.mode() {
            SurfaceMode::Discovery => self.meta.iter().map(|t| t.spec().clone()).collect(),
            SurfaceMode::Direct => {
                let mut specs = self.discovery.registry.list();
                specs.sort_by(|a, b| a.name.cmp(&b.name));
                specs
            }
        }
    }

    /// Resolve a tool the model asked for.  In discovery mode only the three
    /// meta-tools are directly addressable (everything else goes via
    /// `use_tool`).
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        match self.mode() {
            SurfaceMode::Discovery => self.meta.iter().find(|t| t.spec().name == name).cloned(),
            SurfaceMode::Direct => self.discovery.registry.get(name),
        }
    }

    /// The spec that permission checks should evaluate for a call of `name`
    /// with `input`: the target tool's spec when `name` is `use_tool`,
    /// otherwise the called tool's own spec.
    #[must_use]
    pub fn effective_spec(&self, name: &str, input: &Value) -> Option<ToolSpec> {
        if name == USE_TOOL {
            let target = input.get("name")?.as_str()?;
            if is_meta(target) {
                return None;
            }
            return self
                .discovery
                .registry
                .get(target)
                .map(|t| t.spec().clone());
        }
        self.resolve(name).map(|t| t.spec().clone())
    }
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;

    use super::*;

    struct Fake {
        spec: ToolSpec,
    }

    #[async_trait]
    impl Tool for Fake {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }

        async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(format!("{}:{input}", self.spec.name)))
        }
    }

    fn fake(name: &str, description: &str, effect: Effect) -> Arc<dyn Tool> {
        Arc::new(Fake {
            spec: ToolSpec {
                name: name.into(),
                description: description.into(),
                input_schema: json!({ "type": "object", "properties": { "x": {} } }),
                effect,
                category: PermissionCategory::Accounts,
                platforms: vec![Platform::All],
            },
        })
    }

    fn registry_with(n: usize) -> ToolRegistry {
        let reg = ToolRegistry::new();
        for i in 0..n {
            reg.register(fake(
                &format!("tool{i}"),
                &format!("does job {i}"),
                Effect::Read,
            ));
        }
        reg
    }

    fn text(out: &ToolOutput) -> String {
        match &out.content[0] {
            ContentPart::Text { text } => text.clone(),
            other => panic!("unexpected content {other:?}"),
        }
    }

    fn json_of(out: &ToolOutput) -> Value {
        match &out.content[0] {
            ContentPart::Json { data } => data.clone(),
            other => panic!("unexpected content {other:?}"),
        }
    }

    #[test]
    fn at_or_below_threshold_tools_are_passed_directly() {
        let surface = ToolSurface::new(registry_with(5), 5);
        assert_eq!(surface.mode(), SurfaceMode::Direct);
        let names: Vec<_> = surface.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["tool0", "tool1", "tool2", "tool3", "tool4"]);
        assert!(surface.resolve("tool3").is_some());
        assert!(surface.resolve(FIND_TOOL).is_none());
    }

    #[test]
    fn above_threshold_only_meta_tools_are_exposed() {
        let surface = ToolSurface::new(registry_with(6), 5);
        assert_eq!(surface.mode(), SurfaceMode::Discovery);
        let names: Vec<_> = surface.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, [FIND_TOOL, GET_TOOL_DETAILS, USE_TOOL]);
        assert!(surface.resolve(USE_TOOL).is_some());
        // Real tools are not directly addressable in discovery mode.
        assert!(surface.resolve("tool3").is_none());
    }

    #[test]
    fn mode_switches_as_registry_grows_and_shrinks() {
        let reg = registry_with(3);
        let surface = ToolSurface::new(reg.clone(), 3);
        assert_eq!(surface.mode(), SurfaceMode::Direct);
        reg.register(fake("extra", "one too many", Effect::Read));
        assert_eq!(surface.mode(), SurfaceMode::Discovery);
        reg.unregister("extra");
        assert_eq!(surface.mode(), SurfaceMode::Direct);
    }

    #[test]
    fn find_get_use_roundtrip() {
        let reg = registry_with(10);
        reg.register(fake(
            "mcp:mail/send_email",
            "Send an e-mail message",
            Effect::Destructive,
        ));
        let surface = ToolSurface::new(reg, 5);
        let ctx = ToolContext::default();

        let find = surface.resolve(FIND_TOOL).unwrap();
        let out = block_on(find.call(&ctx, json!({ "query": "send email", "k": 2 }))).unwrap();
        let hits = json_of(&out);
        assert_eq!(hits[0]["name"], "mcp:mail/send_email");
        assert!(hits.as_array().unwrap().len() <= 2);

        let details = surface.resolve(GET_TOOL_DETAILS).unwrap();
        let out = block_on(details.call(&ctx, json!({ "name": "mcp:mail/send_email" }))).unwrap();
        let d = json_of(&out);
        assert_eq!(d["effect"], "destructive");
        assert_eq!(d["input_schema"]["type"], "object");

        let use_tool = surface.resolve(USE_TOOL).unwrap();
        let out = block_on(use_tool.call(
            &ctx,
            json!({ "name": "mcp:mail/send_email", "input": { "x": 1 } }),
        ))
        .unwrap();
        assert_eq!(text(&out), r#"mcp:mail/send_email:{"x":1}"#);
    }

    #[test]
    fn use_tool_rejects_unknown_and_meta_targets() {
        let surface = ToolSurface::new(registry_with(10), 5);
        let ctx = ToolContext::default();
        let use_tool = surface.resolve(USE_TOOL).unwrap();
        let err = block_on(use_tool.call(&ctx, json!({ "name": "nope" }))).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
        let err = block_on(use_tool.call(&ctx, json!({ "name": USE_TOOL }))).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
        let err = block_on(use_tool.call(&ctx, json!({}))).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[test]
    fn discovery_only_reveals_tools_visible_to_the_caller() {
        let reg = registry_with(10);
        reg.register(fake("secret_send", "send a secret message", Effect::Write));
        reg.register(fake("public_send", "send a public message", Effect::Write));
        // Visibility depends on the caller (run id), as in the agent loop.
        let vis: ToolVisibility = Arc::new(|ctx, spec| {
            ctx.run_id == "all" || (ctx.run_id == "narrow" && spec.name != "secret_send")
        });
        let surface = ToolSurface::with_visibility(reg, 5, vis);
        let ctx = |id: &str| ToolContext {
            run_id: id.to_owned(),
            ..ToolContext::default()
        };
        let find = surface.resolve(FIND_TOOL).unwrap();
        let names = |id: &str| -> Vec<String> {
            let out =
                block_on(find.call(&ctx(id), json!({ "query": "send message", "k": 25 }))).unwrap();
            json_of(&out)
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["name"].as_str().unwrap().to_owned())
                .collect()
        };
        assert!(names("all").contains(&"secret_send".to_owned()));
        let narrow = names("narrow");
        assert!(narrow.contains(&"public_send".to_owned()));
        assert!(!narrow.contains(&"secret_send".to_owned()));
        assert!(names("unknown").is_empty(), "unknown callers see nothing");

        let details = surface.resolve(GET_TOOL_DETAILS).unwrap();
        let hidden = block_on(details.call(&ctx("narrow"), json!({ "name": "secret_send" })));
        assert!(matches!(hidden, Err(ToolError::InvalidInput(_))));
        assert!(block_on(details.call(&ctx("all"), json!({ "name": "secret_send" }))).is_ok());
        let use_tool = surface.resolve(USE_TOOL).unwrap();
        let hidden = block_on(use_tool.call(&ctx("narrow"), json!({ "name": "secret_send" })));
        assert!(matches!(hidden, Err(ToolError::InvalidInput(_))));
    }

    #[test]
    fn find_tool_sees_registry_changes() {
        let reg = registry_with(10);
        let surface = ToolSurface::new(reg.clone(), 5);
        let ctx = ToolContext::default();
        let find = surface.resolve(FIND_TOOL).unwrap();
        let q = json!({ "query": "zebra" });
        assert_eq!(
            json_of(&block_on(find.call(&ctx, q.clone())).unwrap())
                .as_array()
                .unwrap()
                .len(),
            0
        );
        reg.register(fake("zebra_tool", "stripes", Effect::Read));
        let hits = json_of(&block_on(find.call(&ctx, q.clone())).unwrap());
        assert_eq!(hits[0]["name"], "zebra_tool");
        reg.unregister("zebra_tool");
        assert_eq!(
            json_of(&block_on(find.call(&ctx, q)).unwrap())
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn effective_spec_of_use_tool_is_the_target() {
        let reg = registry_with(10);
        reg.register(fake("danger", "deletes things", Effect::Destructive));
        let surface = ToolSurface::new(reg, 5);
        let direct = surface
            .effective_spec(USE_TOOL, &json!({ "name": "tool1" }))
            .unwrap();
        assert_eq!(direct.effect, Effect::Read);
        let danger = surface
            .effective_spec(USE_TOOL, &json!({ "name": "danger" }))
            .unwrap();
        assert_eq!(danger.effect, Effect::Destructive);
        assert!(
            surface
                .effective_spec(USE_TOOL, &json!({ "name": "missing" }))
                .is_none()
        );
        // use_tool itself is the worst case; find_tool is read-only.
        assert_eq!(
            surface
                .effective_spec(FIND_TOOL, &json!({}))
                .unwrap()
                .effect,
            Effect::Read
        );
    }
}
