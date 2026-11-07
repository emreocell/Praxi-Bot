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

/// Number of profile-visible, non-namespaced tools above which discovery
/// meta-tools replace direct exposure.
///
/// This is the single default for both the surface and the agent loop
/// (`agent_core::DEFAULT_DIRECT_TOOL_LIMIT` is this value): the built-in set
/// (with `spawn_subagent`, skill and workflow tools) is just over 25, and the
/// model should see it directly.
pub const DEFAULT_DISCOVERY_THRESHOLD: usize = 40;
/// Default number of results of `find_tool`.
pub const DEFAULT_FIND_K: usize = 5;
/// Upper bound on `k` accepted by `find_tool`.
pub const MAX_FIND_K: usize = 25;

fn is_meta(name: &str) -> bool {
    matches!(name, FIND_TOOL | GET_TOOL_DETAILS | USE_TOOL)
}

/// Namespaced tools (`mcp:server/tool`, future `api:...`) are discovery-only:
/// they never count against the threshold and are reached via `use_tool`.
#[must_use]
pub fn is_namespaced(name: &str) -> bool {
    name.contains(':')
}

/// Short family labels (`fs_*`, `mcp:server/*`) of `names`, sorted and deduped,
/// so an empty `find_tool` result still tells the model where to look.
fn tool_families<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut fams: Vec<String> = names
        .filter(|n| !is_meta(n))
        .map(|n| {
            if let Some((ns, rest)) = n.split_once(':') {
                match rest.split_once('/') {
                    Some((server, _)) => format!("{ns}:{server}/*"),
                    None => format!("{ns}:*"),
                }
            } else if let Some((prefix, _)) = n.split_once('_') {
                format!("{prefix}_*")
            } else {
                n.to_owned()
            }
        })
        .collect();
    fams.sort();
    fams.dedup();
    fams
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
            .filter_map(|h| {
                let tool = self.discovery.registry.get(&h.name)?;
                let spec = tool.spec();
                self.discovery.visible(ctx, spec).then(|| {
                    json!({
                        "name": h.name,
                        "description": h.description,
                        "score": h.score,
                        "effect": spec.effect,
                        "category": spec.category,
                    })
                })
            })
            .take(k)
            .collect();
        let data = if hits.is_empty() {
            let specs = self.discovery.registry.list();
            let fams = tool_families(
                specs
                    .iter()
                    .filter(|s| self.discovery.visible(ctx, s))
                    .map(|s| s.name.as_str()),
            );
            json!({
                "results": [],
                "hint": format!(
                    "No tool matched `{query}`. Try a broader or different keyword. \
                     Available tool families: {}.",
                    fams.join(", ")
                ),
            })
        } else {
            Value::Array(hits)
        };
        Ok(ToolOutput {
            content: vec![ContentPart::Json { data }],
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

    /// Current presentation mode (every registered tool counts).
    #[must_use]
    pub fn mode(&self) -> SurfaceMode {
        self.mode_for(&|_| true)
    }

    /// Presentation mode counting only the non-namespaced tools `visible`
    /// admits (e.g. those a bot profile may see).
    #[must_use]
    pub fn mode_for(&self, visible: &dyn Fn(&ToolSpec) -> bool) -> SurfaceMode {
        let counted = self
            .discovery
            .registry
            .list()
            .iter()
            .filter(|s| !is_namespaced(&s.name) && visible(s))
            .count();
        if counted > self.threshold {
            SurfaceMode::Discovery
        } else {
            SurfaceMode::Direct
        }
    }

    /// Whether a namespaced tool `visible` admits exists; if so the meta-tools
    /// are exposed even in direct mode.
    fn has_namespaced(&self, visible: &dyn Fn(&ToolSpec) -> bool) -> bool {
        self.discovery
            .registry
            .list()
            .iter()
            .any(|s| is_namespaced(&s.name) && visible(s))
    }

    /// Tool definitions to hand to the model, sorted by name.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.specs_for(&|_| true)
    }

    /// Like [`Self::specs`] for the tools `visible` admits.  Direct mode lists
    /// the admitted non-namespaced tools (plus the meta-tools when namespaced
    /// tools exist, so they stay reachable); discovery mode lists only the
    /// meta-tools.
    #[must_use]
    pub fn specs_for(&self, visible: &dyn Fn(&ToolSpec) -> bool) -> Vec<ToolSpec> {
        let meta = || self.meta.iter().map(|t| t.spec().clone());
        match self.mode_for(visible) {
            SurfaceMode::Discovery => meta().collect(),
            SurfaceMode::Direct => {
                let mut specs: Vec<ToolSpec> = self
                    .discovery
                    .registry
                    .list()
                    .into_iter()
                    .filter(|s| !is_namespaced(&s.name) && visible(s))
                    .collect();
                specs.sort_by(|a, b| a.name.cmp(&b.name));
                if self.has_namespaced(visible) {
                    specs.extend(meta());
                }
                specs
            }
        }
    }

    /// Tool definitions for a turn the router narrowed to `hinted` (ADR-0005:
    /// the router only narrows): the hinted tools that exist and pass
    /// `visible`, plus the three discovery meta-tools so the model can still
    /// reach every other tool through `find_tool`.  Sorted by name.
    #[must_use]
    pub fn hinted_specs(
        &self,
        hinted: &[String],
        visible: &dyn Fn(&ToolSpec) -> bool,
    ) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = hinted
            .iter()
            .filter_map(|n| self.discovery.registry.get(n))
            .map(|t| t.spec().clone())
            .filter(|s| !is_meta(&s.name) && visible(s))
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs.dedup_by(|a, b| a.name == b.name);
        specs.extend(self.meta.iter().map(|t| t.spec().clone()));
        specs
    }

    /// Like [`Self::resolve`] for a turn narrowed by [`Self::hinted_specs`]:
    /// the meta-tools and the hinted tools are addressable in either mode.
    #[must_use]
    pub fn resolve_hinted(
        &self,
        name: &str,
        hinted: &[String],
        visible: &dyn Fn(&ToolSpec) -> bool,
    ) -> Option<Arc<dyn Tool>> {
        if let Some(t) = self.meta.iter().find(|t| t.spec().name == name) {
            return Some(Arc::clone(t));
        }
        if hinted.iter().any(|h| h == name) {
            return self.discovery.registry.get(name);
        }
        self.resolve_with(name, visible)
    }

    /// Resolve a tool the model asked for.  In discovery mode only the three
    /// meta-tools are directly addressable (everything else goes via
    /// `use_tool`); namespaced tools are never directly addressable.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.resolve_with(name, &|_| true)
    }

    /// [`Self::resolve`] against the same `visible` filter as
    /// [`Self::specs_for`], so what the model was shown is what resolves.
    #[must_use]
    pub fn resolve_with(
        &self,
        name: &str,
        visible: &dyn Fn(&ToolSpec) -> bool,
    ) -> Option<Arc<dyn Tool>> {
        let meta = || self.meta.iter().find(|t| t.spec().name == name).cloned();
        match self.mode_for(visible) {
            SurfaceMode::Discovery => meta(),
            SurfaceMode::Direct => {
                if is_namespaced(name) {
                    return None;
                }
                self.discovery
                    .registry
                    .get(name)
                    .or_else(|| self.has_namespaced(visible).then(meta).flatten())
            }
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

    /// Hits of a `find_tool` result: a bare array, or `results` of the
    /// empty-result object that carries a hint.
    fn hits_of(v: &Value) -> Vec<Value> {
        v.as_array()
            .or_else(|| v["results"].as_array())
            .unwrap()
            .clone()
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
            hits_of(&json_of(&out))
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
            hits_of(&json_of(&block_on(find.call(&ctx, q.clone())).unwrap())).len(),
            0
        );
        reg.register(fake("zebra_tool", "stripes", Effect::Read));
        let hits = json_of(&block_on(find.call(&ctx, q.clone())).unwrap());
        assert_eq!(hits[0]["name"], "zebra_tool");
        reg.unregister("zebra_tool");
        assert_eq!(
            hits_of(&json_of(&block_on(find.call(&ctx, q)).unwrap())).len(),
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

    #[test]
    fn namespaced_tools_are_discovery_only_and_do_not_count() {
        let reg = registry_with(5);
        for i in 0..30 {
            reg.register(fake(&format!("mcp:x/t{i}"), "remote", Effect::Read));
        }
        let surface = ToolSurface::new(reg, 5);
        // 5 native tools are at the threshold: direct, plus the meta-tools.
        assert_eq!(surface.mode(), SurfaceMode::Direct);
        let names: Vec<_> = surface.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names.len(), 8);
        assert!(names.iter().all(|n| !n.starts_with("mcp:")));
        assert!(surface.resolve("tool2").is_some());
        assert!(surface.resolve("mcp:x/t3").is_none());
        assert!(surface.resolve(USE_TOOL).is_some());
        // The permission check still sees the use_tool target.
        let spec = surface
            .effective_spec(USE_TOOL, &json!({ "name": "mcp:x/t3" }))
            .unwrap();
        assert_eq!(spec.name, "mcp:x/t3");
        // A visibility filter lowers the counted size.
        let reg2 = registry_with(6);
        reg2.register(fake("mcp:x/a", "remote", Effect::Read));
        let s2 = ToolSurface::new(reg2, 5);
        assert_eq!(s2.mode(), SurfaceMode::Discovery);
        assert_eq!(s2.mode_for(&|s| s.name != "tool0"), SurfaceMode::Direct);
    }

    #[test]
    fn find_tool_hits_carry_effect_and_category() {
        let reg = registry_with(10);
        reg.register(fake("mcp:mail/send", "Send an e-mail", Effect::Destructive));
        let surface = ToolSurface::new(reg, 5);
        let find = surface.resolve(FIND_TOOL).unwrap();
        let out =
            block_on(find.call(&ToolContext::default(), json!({ "query": "e-mail" }))).unwrap();
        let hits = json_of(&out);
        assert_eq!(hits[0]["name"], "mcp:mail/send");
        assert_eq!(hits[0]["effect"], "destructive");
        assert!(hits[0]["category"].is_string());
    }

    #[test]
    fn empty_find_tool_result_lists_only_visible_families() {
        let reg = ToolRegistry::new();
        reg.register(fake("fs_read", "read a file", Effect::Read));
        reg.register(fake("fs_write", "write a file", Effect::Write));
        reg.register(fake("desktop_click", "click", Effect::Write));
        reg.register(fake("mcp:mail/send", "send", Effect::Write));
        let vis: ToolVisibility = Arc::new(|_, s| !s.name.starts_with("desktop_"));
        let surface = ToolSurface::with_visibility(reg, 1, vis);
        let find = surface.resolve(FIND_TOOL).unwrap();
        let out =
            block_on(find.call(&ToolContext::default(), json!({ "query": "zzzzqqq" }))).unwrap();
        let data = json_of(&out);
        assert_eq!(data["results"], json!([]));
        let hint = data["hint"].as_str().unwrap();
        assert!(hint.contains("fs_*"), "{hint}");
        assert!(hint.contains("mcp:mail/*"), "{hint}");
        assert!(!hint.contains("desktop_"), "{hint}");
    }
}
