//! The `Tool` trait and associated output types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{error::ToolError, spec::ToolSpec};

/// A reference to a spilled (large) tool output written to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpillRef {
    /// Absolute path to the spill file on the node running the tool.
    pub path: String,
    /// MIME type of the spill file contents.
    pub mime_type: String,
    /// Byte size of the spill file.
    pub size_bytes: u64,
}

/// A single content part in a tool's output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text output.
    Text { text: String },
    /// Structured JSON data.
    Json { data: Value },
    /// A reference to binary data (e.g. an image).
    Binary { mime_type: String, base64: String },
}

/// Trust of a tool output, recorded by daemon code.
///
/// This is not read back from the bytes shown to the model. A tool sets it
/// when the output's trust is not the tool's own declared class (discovery
/// returning someone else's description, for example).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputExposure {
    /// The output contains untrusted text.
    pub ingests_untrusted: bool,
    /// The output contains private data.
    pub reads_private: bool,
    /// Short source label for an approval card. Empty means the caller picks one.
    pub label: String,
    /// A validated local repository folder whose read caused this exposure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_folder: Option<String>,
}

/// The result of a successful tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The content parts returned by the tool.
    ///
    /// For most tools this contains a single text or JSON part.
    pub content: Vec<ContentPart>,
    /// If the full output exceeded the in-memory budget, the remainder was
    /// written to a temporary file and a pointer is returned here.
    pub spill: Option<SpillRef>,
    /// Trust of this output when it differs from the tool's declared class.
    /// `None` means "use the spec". Never derived from [`Self::content`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<OutputExposure>,
}

impl ToolOutput {
    /// Create a simple text output with no spill.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text { text: text.into() }],
            spill: None,
            exposure: None,
        }
    }
}

/// One running program as the daemon names it for a control grant: image
/// path, process id and process start time. A reused pid has another start
/// time and is another program.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ControlIdentity {
    /// Full path of the executable image.
    pub image_path: String,
    /// Process id.
    pub pid: u32,
    /// Process start time in OS ticks.
    pub start_time: u64,
}

/// Which kind of input a control call sends. Only these three can ever be
/// covered by a run-scoped control grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    /// `desktop_type`.
    Type,
    /// `desktop_set_value`.
    SetValue,
    /// `desktop_key`.
    Key,
}

/// The program and window a desktop input call would reach, resolved by the
/// tool before the permission decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlTarget {
    /// What the call sends.
    pub kind: ControlKind,
    /// The program that would receive the input.
    pub identity: ControlIdentity,
    /// Window title, for the card.
    pub window_title: String,
    /// Why no grant may cover this call or be offered for it (a stable code
    /// such as `terminal`, `elevated`, `password_field`, `chord`, `dialog`).
    /// `None` = a grant may cover it.
    pub refusal: Option<String>,
}

impl ControlTarget {
    /// File name of the program, for the card.
    #[must_use]
    pub fn program_name(&self) -> String {
        let p = &self.identity.image_path;
        p.rsplit(['\\', '/']).next().unwrap_or(p).to_owned()
    }

    /// A grant may cover this call and may be offered on its card.
    #[must_use]
    pub fn eligible(&self) -> bool {
        self.refusal.is_none()
            && self.identity.pid != 0
            && self.identity.start_time != 0
            && !self.identity.image_path.trim().is_empty()
    }
}

/// Payment guard for an action in the managed browser.
#[derive(Debug, Clone, Default)]
pub enum PaymentApproval {
    /// This call uses the existing approval rules.
    #[default]
    Unchecked,
    /// Withhold input if a checkout or payment surface is detected.
    Check,
    /// The owner approved this call on the identified payment document.
    Approved(u64),
}

/// Context supplied to every tool call.
// Each flag is a separate fact the gate decides on (taint, untrusted, private, unattended mail).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// The run that invoked the tool.
    pub run_id: String,
    /// Maximum number of bytes of output to retain in memory before spilling.
    pub max_inline_bytes: usize,
    /// Bot that invoked the tool. `None` when the caller is not a bot.
    ///
    /// MCP servers with a `bots` allowlist refuse a call whose id is missing
    /// or not listed. `None` here is not "every bot".
    pub bot_id: Option<String>,
    /// The run has already read untrusted or private content. A skill saved
    /// from this call is stored as untrusted data, not as a trusted instruction.
    pub content_tainted: bool,
    /// The run holds UNTRUSTED content (a page, mail, a download, another bot's reply). Files
    /// written or moved by a tool in such a run get an untrusted origin mark.
    pub exposure_untrusted: bool,
    /// The run holds PRIVATE content of the owner (a file, a mailbox, a memory, a screen). A note
    /// written in such a run is marked private, so a later read of it taints that run as private.
    pub exposure_private: bool,
    /// The origin (`scheme://host:port`) the loop saw the browser page on when it allowed this
    /// page action. The browser tool refuses to act if the page is on another origin by then
    /// (a navigation or redirect in between), so an allowance never follows the page elsewhere.
    pub expected_origin: Option<String>,
    /// The Full level admitted this call without a card. A mail send then refuses
    /// every mail-guard warning: only a card the owner saw can send past one.
    pub unattended_mail: bool,
    /// The loop let this bot-management call through without a card because it is the owner's
    /// own Full-level chat turn (nothing asked but money), so no card froze the preview the host
    /// applies: the bot tools freeze it themselves. Set only there, never from `unattended_mail`
    /// (which every Full-level call carries) and never by a tool.
    pub full_waived_preview: bool,
    /// Payment authorization supplied by the daemon, never tool arguments.
    pub payment: PaymentApproval,
    /// Set by the loop only when a run-scoped control grant covers this call:
    /// the tool must send its input through the backend's guarded path, so the
    /// window that would receive it is checked against this program right
    /// before the OS call. `None` for every other call.
    pub control_guard: Option<ControlIdentity>,
    /// Set for a subagent run (its own run id): a tool with per-run state, such
    /// as the browser, keeps this run apart from the parent and its siblings.
    /// `None` for a top-level run, which uses the shared state.
    pub lane: Option<String>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            max_inline_bytes: 1024 * 1024, // 1 MiB
            bot_id: None,
            content_tainted: false,
            exposure_untrusted: false,
            exposure_private: false,
            expected_origin: None,
            unattended_mail: false,
            full_waived_preview: false,
            payment: PaymentApproval::Unchecked,
            control_guard: None,
            lane: None,
        }
    }
}

/// A narrow, owner-created standing authorisation that stands in for the
/// approval card of one specific call (for example "this secret on this exact
/// origin"). Returned by [`Tool::standing_grant`]; the loop records it in the
/// audit log and does not ask. It is never a policy waiver: nothing but the
/// tool itself can produce one, and only after it re-verified the exact facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingGrant {
    /// The grant's id (safe to audit: no value).
    pub id: String,
    /// One line saying what was authorised (names and origins only).
    pub label: String,
}

/// A callable tool.
///
/// **Delegation rule:** a `Tool` that forwards calls to another tool (a
/// wrapper, bridge or dispatcher) must also forward [`Tool::always_ask`],
/// [`Tool::always_ask_for`], [`Tool::standing_grant`],
/// [`Tool::approval_details`] and [`Tool::approval_closed`] to it, otherwise
/// the wrapped tool's confirmation guarantees are silently lost. Prefer
/// implementing [`ToolWrapper`] (where the hooks are required methods with no
/// defaults); a repository test (`delegating_tools_forward_the_approval_hooks`)
/// also inspects every `impl Tool` AST and fails for a new delegating
/// implementation that does not forward them.
///
/// Implementors describe themselves via [`Tool::spec`] and execute via
/// [`Tool::call`].  All implementations must be `Send + Sync` so they can be
/// shared across async tasks.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Return the static specification for this tool.
    fn spec(&self) -> &ToolSpec;

    /// Host-derived per-bot egress classification, when it differs from a
    /// static tool declaration. `None` keeps the native gate's static/default
    /// classification; this metadata alone does not authorize an operation.
    fn egress_for(&self, _bot_id: Option<&str>) -> Option<bool> {
        None
    }

    /// Whether the bot `bot_id` (`None` = no bot) may see this tool in
    /// discovery and in the model's tool list.  Default: every bot.  This is
    /// a visibility filter only; a tool that restricts callers must still
    /// enforce that in [`Tool::call`].
    fn visible_to(&self, _bot_id: Option<&str>) -> bool {
        true
    }

    /// Execute the tool with the given `input` and return its output.
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError>;

    /// Structured facts an approval card should show for this exact call
    /// (computed before the user is asked; carried on the `approval_needed`
    /// event). `Ok(None)` (the default) means the generic card is enough.
    ///
    /// # Errors
    /// The call is invalid and must not be offered for approval: the error
    /// goes straight back to the model.
    async fn approval_details(
        &self,
        _ctx: &ToolContext,
        _input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        Ok(None)
    }

    /// Called when an approval that [`Tool::approval_details`] prepared ends
    /// without the call running (denied, cancelled, timed out, rejected before
    /// it was shown, or approved but never executed), so the tool can drop what
    /// it froze for it. `token` is the `approval_ref` string the tool put in
    /// its details (the loop removes it before the card is shown): only what
    /// was created for that very approval may be dropped.
    fn approval_closed(&self, _ctx: &ToolContext, _input: &Value, _token: Option<&str>) {}

    /// `true` when the call must always be confirmed by the user: a policy
    /// that would allow it is raised to "ask", and neither "always allow"
    /// grants (tool or category) nor "this chat" grants waive the ask. Only an
    /// outright deny is stricter. For tools that delegate future authority.
    fn always_ask(&self) -> bool {
        false
    }

    /// The program and window this call would send input to, for a run-scoped
    /// control grant. Resolved before the permission decision.
    /// `None` (the default) means the tool has no such target, so no grant is
    /// offered for it and behaviour is unchanged.
    async fn control_target(&self, _ctx: &ToolContext, _input: &Value) -> Option<ControlTarget> {
        None
    }

    /// The origin a page action would act on right now, read from the browser itself (`None`:
    /// not a page action, no session, or unknown: the call then stays once-only).
    async fn current_origin(&self, _ctx: &ToolContext, _input: &Value) -> Option<String> {
        None
    }

    /// The origin the browser page is on right now, read from the browser itself, after a call
    /// ran (a redirect or a click may have moved it). `None`: not a browser tool or unknown.
    async fn landed_origin(&self, _ctx: &ToolContext) -> Option<String> {
        None
    }

    /// Stable id of the browser tab the call just observed, read from the
    /// browser itself after the call ran (the loop keeps one full page
    /// observation per tab when it shortens a long conversation). `None`: not a
    /// browser tool, or the id is unknown.
    async fn landed_tab(&self, _ctx: &ToolContext) -> Option<String> {
        None
    }

    /// Identify a managed payment page just before approval. The token binds
    /// authorization to that document and is never based on model arguments.
    async fn payment_page(&self, _ctx: &ToolContext, _input: &Value) -> Option<u64> {
        None
    }

    /// Host the call actually contacts when the input does not name one.
    ///
    /// `OpenAPI` tools use this so the card footer shows the API host, not the
    /// operation name. `None` leaves the card's own host text.
    fn egress_host(&self, _input: &Value) -> Option<String> {
        None
    }

    /// Lines the tool itself adds to the outgoing text: a default browser
    /// profile, the search backend that will be used, a skill body that is
    /// not in the arguments. Empty when the input already shows everything.
    fn egress_extra(&self, _ctx: &ToolContext, _input: &Value) -> Option<String> {
        None
    }

    /// Like [`Tool::always_ask`] but decided from this call's input: a call
    /// that types an owner-stored secret must be confirmed every time, however
    /// permissive the policy, grants, read shortcuts or presets are. Only an
    /// outright deny is stricter. Default: no.
    fn always_ask_for(&self, _input: &Value) -> bool {
        false
    }

    /// `true` when this very call must still be confirmed on the Full access level, which
    /// otherwise runs everything but money without a card. For a call whose effect the owner
    /// cannot undo from the screen: it opens a lock or a garage door, or disarms an alarm.
    /// Decided from the call's own input, like [`Tool::always_ask_for`] (which such a tool
    /// repeats for the lower levels); nothing but an outright deny is stricter. Default: no.
    fn full_access_asks_for(&self, _input: &Value) -> bool {
        false
    }

    /// An owner-created standing authorisation covering exactly this call, if
    /// there is one, checked against the facts the tool itself verifies at this
    /// moment (never against what the model claims). `Some` lets the loop skip
    /// the approval card for this call and audit the grant's id; it does not
    /// skip [`Tool::call`]'s own checks. Default: none.
    async fn standing_grant(&self, _ctx: &ToolContext, _input: &Value) -> Option<StandingGrant> {
        None
    }
}

/// A wrapper trait for tools that delegate to another [`Tool`].
///
/// Unlike [`Tool`], the approval hooks (`always_ask`, `approval_details`,
/// `approval_closed`) have no default implementations here, so any wrapper
/// implementing [`ToolWrapper`] fails to compile if it forgets to forward them.
#[async_trait]
pub trait ToolWrapper: Send + Sync {
    /// Return the static specification for this tool.
    fn spec(&self) -> &ToolSpec;

    /// Forward the wrapped tool's dynamic egress classification.
    fn egress_for(&self, bot_id: Option<&str>) -> Option<bool>;

    /// Whether the bot `bot_id` (`None` = no bot) may see this tool.
    fn visible_to(&self, _bot_id: Option<&str>) -> bool {
        true
    }

    /// Execute the tool with the given `input` and return its output.
    ///
    /// # Errors
    /// Returns [`ToolError`] if input is invalid or tool execution fails.
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError>;

    /// Forward [`Tool::always_ask`] to the wrapped tool.
    fn always_ask(&self) -> bool;

    /// Forward [`Tool::always_ask_for`] to the wrapped tool.
    fn always_ask_for(&self, input: &Value) -> bool;

    /// Forward [`Tool::full_access_asks_for`] to the wrapped tool.
    fn full_access_asks_for(&self, input: &Value) -> bool;

    /// Forward [`Tool::standing_grant`] to the wrapped tool.
    async fn standing_grant(&self, ctx: &ToolContext, input: &Value) -> Option<StandingGrant>;

    /// Forward [`Tool::payment_page`] to the wrapped tool: a wrapper that drops it makes the
    /// wrapped browser tool look like it never reaches a checkout.
    async fn payment_page(&self, ctx: &ToolContext, input: &Value) -> Option<u64>;

    /// Forward [`Tool::approval_details`] to the wrapped tool.
    ///
    /// # Errors
    /// Returns [`ToolError`] if the wrapped tool rejects the call.
    async fn approval_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError>;

    /// Forward [`Tool::approval_closed`] to the wrapped tool.
    fn approval_closed(&self, ctx: &ToolContext, input: &Value, token: Option<&str>);
}

#[async_trait]
impl<W: ToolWrapper> Tool for W {
    fn spec(&self) -> &ToolSpec {
        ToolWrapper::spec(self)
    }

    fn egress_for(&self, bot_id: Option<&str>) -> Option<bool> {
        ToolWrapper::egress_for(self, bot_id)
    }

    fn visible_to(&self, bot_id: Option<&str>) -> bool {
        ToolWrapper::visible_to(self, bot_id)
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        ToolWrapper::call(self, ctx, input).await
    }

    async fn approval_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Option<Value>, ToolError> {
        ToolWrapper::approval_details(self, ctx, input).await
    }

    fn approval_closed(&self, ctx: &ToolContext, input: &Value, token: Option<&str>) {
        ToolWrapper::approval_closed(self, ctx, input, token);
    }

    fn always_ask(&self) -> bool {
        ToolWrapper::always_ask(self)
    }

    fn always_ask_for(&self, input: &Value) -> bool {
        ToolWrapper::always_ask_for(self, input)
    }

    fn full_access_asks_for(&self, input: &Value) -> bool {
        ToolWrapper::full_access_asks_for(self, input)
    }

    async fn standing_grant(&self, ctx: &ToolContext, input: &Value) -> Option<StandingGrant> {
        ToolWrapper::standing_grant(self, ctx, input).await
    }

    async fn payment_page(&self, ctx: &ToolContext, input: &Value) -> Option<u64> {
        ToolWrapper::payment_page(self, ctx, input).await
    }
}

#[cfg(test)]
mod delegation_guard {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use crate::ContentClass;

    use syn::visit::{self, Visit};
    use syn::{
        Expr, Fields, Generics, ImplItem, ItemEnum, ItemImpl, ItemStruct, PathSegment, Type,
    };

    /// `file :: type` pairs whose tools dispatch to other tools (or hold a
    /// registry / gate) without forwarding the approval hooks themselves, each
    /// reviewed and why. The list must match the repository EXACTLY: a new
    /// delegating `impl Tool` fails this test until it forwards
    /// `always_ask` / `approval_details` / `approval_closed` (or implements
    /// `ToolWrapper`) or is added here with a reason; an entry that no longer
    /// matches must be removed.
    const REVIEWED: &[(&str, &str, &str)] = &[
        (
            "agent-router/src/surface.rs",
            "FindTool",
            "find_tool only lists tools; it calls none",
        ),
        (
            "agent-router/src/surface.rs",
            "UseTool",
            "the loop resolves `use_tool` to its TARGET by name and runs decide + approval_details against the target, then the target's own hooks apply",
        ),
        (
            "builtin-tools/src/script.rs",
            "ScriptTool",
            "script_run: every inner call goes through the loop's ToolGate (decide + approval_details)",
        ),
        (
            "builtin-tools/src/selfcheck.rs",
            "SelfCheck",
            "tool_self_check: every probe is a fixed read-only sample that the loop's ToolGate must authorize unattended first (never an approval card: a call that would ask is skipped), and its output is discarded",
        ),
        (
            "workflow-replay/src/tool.rs",
            "WorkflowRun",
            "workflow_run: every step goes through the loop's ToolGate (decide + approval_details)",
        ),
        (
            "agent-router/src/surface.rs",
            "GetToolDetails",
            "get_tool_details only describes tools; it calls none",
        ),
    ];

    const DELEGATING_TYPES: &[&str] = &[
        "Tool",
        "ToolRegistry",
        "ToolGate",
        "ToolSurface",
        "Discovery",
    ];

    const REQUIRED_HOOKS: &[&str] = &[
        "always_ask",
        "always_ask_for",
        "full_access_asks_for",
        "standing_grant",
        "approval_details",
        "approval_closed",
    ];

    #[derive(Default)]
    struct TypeInfo {
        field_types: Vec<Type>,
        bounded_params: BTreeSet<String>,
    }

    struct IdentFinder<'a> {
        targets: &'a BTreeSet<String>,
        found: bool,
    }

    impl<'ast> Visit<'ast> for IdentFinder<'_> {
        fn visit_path_segment(&mut self, node: &'ast PathSegment) {
            if self.targets.contains(&node.ident.to_string()) {
                self.found = true;
            }
            visit::visit_path_segment(self, node);
        }
    }

    fn type_mentions_any(ty: &Type, targets: &BTreeSet<String>) -> bool {
        let mut finder = IdentFinder {
            targets,
            found: false,
        };
        finder.visit_type(ty);
        finder.found
    }

    fn delegating_set() -> BTreeSet<String> {
        DELEGATING_TYPES.iter().map(|s| (*s).to_owned()).collect()
    }

    fn bounded_type_params(generics: &Generics) -> BTreeSet<String> {
        let targets = delegating_set();
        let mut out = BTreeSet::new();
        for param in generics.type_params() {
            let has_bound = param.bounds.iter().any(|b| {
                let mut finder = IdentFinder {
                    targets: &targets,
                    found: false,
                };
                finder.visit_type_param_bound(b);
                finder.found
            });
            if has_bound {
                out.insert(param.ident.to_string());
            }
        }
        if let Some(wc) = &generics.where_clause {
            for pred in &wc.predicates {
                if let syn::WherePredicate::Type(pt) = pred {
                    let has_bound = pt.bounds.iter().any(|b| {
                        let mut finder = IdentFinder {
                            targets: &targets,
                            found: false,
                        };
                        finder.visit_type_param_bound(b);
                        finder.found
                    });
                    if has_bound
                        && let Type::Path(tp) = &pt.bounded_ty
                        && let Some(ident) = tp.path.get_ident()
                    {
                        out.insert(ident.to_string());
                    }
                }
            }
        }
        out
    }

    fn field_types_of(fields: &Fields) -> Vec<Type> {
        match fields {
            Fields::Named(f) => f.named.iter().map(|x| x.ty.clone()).collect(),
            Fields::Unnamed(f) => f.unnamed.iter().map(|x| x.ty.clone()).collect(),
            Fields::Unit => Vec::new(),
        }
    }

    struct Collector {
        types: BTreeMap<String, Vec<TypeInfo>>,
        tool_impls: Vec<ItemImpl>,
    }

    impl<'ast> Visit<'ast> for Collector {
        fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
            self.types
                .entry(node.ident.to_string())
                .or_default()
                .push(TypeInfo {
                    field_types: field_types_of(&node.fields),
                    bounded_params: bounded_type_params(&node.generics),
                });
            visit::visit_item_struct(self, node);
        }

        fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
            let field_types = node
                .variants
                .iter()
                .flat_map(|v| field_types_of(&v.fields))
                .collect();
            self.types
                .entry(node.ident.to_string())
                .or_default()
                .push(TypeInfo {
                    field_types,
                    bounded_params: bounded_type_params(&node.generics),
                });
            visit::visit_item_enum(self, node);
        }

        fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
            if let Some((_, path, _)) = &node.trait_
                && path.segments.last().is_some_and(|s| s.ident == "Tool")
            {
                self.tool_impls.push(node.clone());
            }
            visit::visit_item_impl(self, node);
        }
    }

    fn self_type_name(ty: &Type) -> Option<String> {
        match ty {
            Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            Type::Group(g) => self_type_name(&g.elem),
            Type::Paren(p) => self_type_name(&p.elem),
            _ => None,
        }
    }

    fn is_bare_self(expr: &Expr) -> bool {
        match expr {
            Expr::Path(p) => p.path.is_ident("self"),
            Expr::Reference(r) => is_bare_self(&r.expr),
            Expr::Paren(p) => is_bare_self(&p.expr),
            Expr::Group(g) => is_bare_self(&g.expr),
            _ => false,
        }
    }

    struct HookCallFinder<'a> {
        hook: &'a str,
        found: bool,
    }

    impl<'ast> Visit<'ast> for HookCallFinder<'_> {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            if node.method == self.hook && !is_bare_self(&node.receiver) {
                self.found = true;
            }
            visit::visit_expr_method_call(self, node);
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if let Expr::Path(p) = &*node.func
                && p.path.segments.len() >= 2
                && p.path.segments.last().is_some_and(|s| s.ident == self.hook)
            {
                self.found = true;
            }
            visit::visit_expr_call(self, node);
        }
    }

    fn forwards_hook(impl_item: &ItemImpl, hook: &str) -> bool {
        impl_item.items.iter().any(|item| {
            let ImplItem::Fn(f) = item else {
                return false;
            };
            if f.sig.ident != hook {
                return false;
            }
            let mut finder = HookCallFinder { hook, found: false };
            finder.visit_block(&f.block);
            finder.found
        })
    }

    fn is_delegating_impl(
        impl_item: &ItemImpl,
        self_name: &str,
        types: &BTreeMap<String, Vec<TypeInfo>>,
    ) -> bool {
        let impl_bounded = bounded_type_params(&impl_item.generics);
        if !impl_bounded.is_empty() {
            return true;
        }
        let Some(defs) = types.get(self_name) else {
            return false;
        };
        let targets = delegating_set();
        for def in defs {
            let mut all_targets = targets.clone();
            all_targets.extend(def.bounded_params.iter().cloned());
            if def
                .field_types
                .iter()
                .any(|ty| type_mentions_any(ty, &all_targets))
            {
                return true;
            }
        }
        false
    }

    /// Return the type names of delegating `impl Tool` blocks in `src` that do
    /// not genuinely forward all three approval hooks.
    fn missing_hooks_in_source(src: &str) -> Vec<String> {
        let file = syn::parse_file(src).expect("valid Rust source");
        let mut collector = Collector {
            types: BTreeMap::new(),
            tool_impls: Vec::new(),
        };
        collector.visit_file(&file);
        let mut out = Vec::new();
        for imp in &collector.tool_impls {
            let Some(name) = self_type_name(&imp.self_ty) else {
                continue;
            };
            if !is_delegating_impl(imp, &name, &collector.types) {
                continue;
            }
            let forwards_all = REQUIRED_HOOKS.iter().all(|hook| forwards_hook(imp, hook));
            if !forwards_all {
                out.push(name);
            }
        }
        out
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            if p.is_dir() {
                if name != "target" && name != "node_modules" && name != ".git" {
                    rust_files(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn delegating_tools_forward_the_approval_hooks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        rust_files(&root.join("crates"), &mut files);
        rust_files(&root.join("apps"), &mut files);
        let mut found: BTreeSet<(String, String)> = BTreeSet::new();
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let rel = f
                .strip_prefix(root.join("crates"))
                .or_else(|_| f.strip_prefix(root.join("apps")))
                .unwrap_or(&f)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            for ident in missing_hooks_in_source(&src) {
                found.insert((rel.clone(), ident));
            }
        }
        let reviewed: BTreeSet<(String, String)> = REVIEWED
            .iter()
            .map(|(f, t, _)| ((*f).to_owned(), (*t).to_owned()))
            .collect();
        let new: Vec<_> = found.difference(&reviewed).collect();
        let stale: Vec<_> = reviewed.difference(&found).collect();
        assert!(
            new.is_empty() && stale.is_empty(),
            "delegating `impl Tool` without always_ask/approval_details/approval_closed (see the delegation rule on `Tool`): new {new:?}; stale reviewed entries {stale:?}"
        );
    }

    #[test]
    fn the_detector_is_per_impl_block_and_sees_delegating_fields() {
        let src = r"
struct A {
    inner: Arc<dyn Tool>,
}
impl Tool for A {
    fn call(&self) {}
}
struct B {
    n: u8,
}
impl Tool for B {
    fn always_ask(&self) -> bool { self.inner.always_ask() }
    async fn approval_details(&self, c: &ToolContext, i: &Value) -> Result<Option<Value>, ToolError> {
        self.inner.approval_details(c, i).await
    }
    fn approval_closed(&self, c: &ToolContext, i: &Value, t: Option<&str>) {
        self.inner.approval_closed(c, i, t);
    }
}
";
        assert_eq!(missing_hooks_in_source(src), vec!["A"]);
    }

    #[allow(clippy::too_many_lines)] // one long table of cases / hooks, flat on purpose
    #[test]
    fn bypass_vectors_are_caught() {
        // 1. Box<dyn Tool>, Vec<Arc<dyn Tool>>, dyn Tool + Send + Sync, tuple struct,
        //    generic wrapper, qualified impl tool_registry::Tool.
        for (label, src) in [
            (
                "Box<dyn Tool>",
                "struct W { inner: Box<dyn Tool> } impl Tool for W { fn call(&self) {} }",
            ),
            (
                "Vec<Arc<dyn Tool>>",
                "struct W { inner: Vec<Arc<dyn Tool>> } impl Tool for W { fn call(&self) {} }",
            ),
            (
                "Arc<dyn Tool + Send + Sync>",
                "struct W { inner: Arc<dyn Tool + Send + Sync> } impl Tool for W { fn call(&self) {} }",
            ),
            (
                "tuple struct",
                "struct W(Arc<dyn Tool>); impl Tool for W { fn call(&self) {} }",
            ),
            (
                "generic impl<T: Tool>",
                "struct W<T>(T); impl<T: Tool> Tool for W<T> { fn call(&self) {} }",
            ),
            (
                "qualified trait path",
                "struct W { inner: Arc<dyn Tool> } impl tool_registry::Tool for W { fn call(&self) {} }",
            ),
            (
                "comment instead of fn",
                "struct W { inner: Arc<dyn Tool> } impl Tool for W { /* fn always_ask fn approval_details fn approval_closed */ }",
            ),
            (
                "empty stub fn approval_closed",
                r"
struct W { inner: Arc<dyn Tool> }
impl Tool for W {
    fn always_ask(&self) -> bool { self.inner.always_ask() }
    async fn approval_details(&self, c: &ToolContext, i: &Value) -> Result<Option<Value>, ToolError> {
        self.inner.approval_details(c, i).await
    }
    fn approval_closed(&self, _: &ToolContext, _: &Value, _: Option<&str>) {}
}
",
            ),
            (
                "forgets approval_closed",
                r"
struct W { inner: Arc<dyn Tool> }
impl Tool for W {
    fn always_ask(&self) -> bool { self.inner.always_ask() }
    async fn approval_details(&self, c: &ToolContext, i: &Value) -> Result<Option<Value>, ToolError> {
        self.inner.approval_details(c, i).await
    }
}
",
            ),
        ] {
            assert_eq!(
                missing_hooks_in_source(src),
                vec!["W"],
                "failed to flag {label}"
            );
        }

        // 2. Column-0 closing brace inside a string literal must not truncate the impl,
        //    and genuine forwarding passes.
        let genuine = r#"
struct Good {
    inner: Box<dyn Tool + Send + Sync>,
}
impl tool_registry::Tool for Good {
    fn spec(&self) -> &ToolSpec {
        let _s = "
}
";
        self.inner.spec()
    }
    fn always_ask(&self) -> bool {
        self.inner.always_ask()
    }
    fn always_ask_for(&self, i: &Value) -> bool {
        self.inner.always_ask_for(i)
    }
    fn full_access_asks_for(&self, i: &Value) -> bool {
        self.inner.full_access_asks_for(i)
    }
    async fn standing_grant(&self, c: &ToolContext, i: &Value) -> Option<StandingGrant> {
        self.inner.standing_grant(c, i).await
    }
    async fn approval_details(&self, c: &ToolContext, i: &Value) -> Result<Option<Value>, ToolError> {
        self.inner.approval_details(c, i).await
    }
    fn approval_closed(&self, c: &ToolContext, i: &Value, t: Option<&str>) {
        self.inner.approval_closed(c, i, t);
    }
}
"#;
        assert!(missing_hooks_in_source(genuine).is_empty(), "{genuine}");

        // A wrapper that forwards the three older hooks but forgets the
        // per-call ones silently loses "this call must always ask".
        let forgets_per_call = r"
struct W { inner: Arc<dyn Tool> }
impl Tool for W {
    fn always_ask(&self) -> bool { self.inner.always_ask() }
    async fn approval_details(&self, c: &ToolContext, i: &Value) -> Result<Option<Value>, ToolError> {
        self.inner.approval_details(c, i).await
    }
    fn approval_closed(&self, c: &ToolContext, i: &Value, t: Option<&str>) {
        self.inner.approval_closed(c, i, t);
    }
}
";
        assert_eq!(missing_hooks_in_source(forgets_per_call), vec!["W"]);
    }

    #[allow(clippy::too_many_lines)] // one long table of cases / hooks, flat on purpose
    #[tokio::test]
    async fn tool_wrapper_trait_forwards_all_approval_hooks() {
        use crate::{Effect, ToolError, ToolSpec};
        use protocol::PermissionCategory;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Inner {
            spec: ToolSpec,
            closed: Arc<AtomicBool>,
        }

        #[async_trait::async_trait]
        impl crate::Tool for Inner {
            fn spec(&self) -> &ToolSpec {
                &self.spec
            }
            async fn call(
                &self,
                _ctx: &crate::ToolContext,
                _input: serde_json::Value,
            ) -> Result<crate::ToolOutput, ToolError> {
                Ok(crate::ToolOutput::text("ok"))
            }
            fn always_ask(&self) -> bool {
                true
            }
            fn always_ask_for(&self, input: &serde_json::Value) -> bool {
                input.get("secret").is_some()
            }
            async fn standing_grant(
                &self,
                _ctx: &crate::ToolContext,
                _input: &serde_json::Value,
            ) -> Option<crate::StandingGrant> {
                Some(crate::StandingGrant {
                    id: "g1".into(),
                    label: "inner".into(),
                })
            }
            async fn approval_details(
                &self,
                _ctx: &crate::ToolContext,
                _input: &serde_json::Value,
            ) -> Result<Option<serde_json::Value>, ToolError> {
                Ok(Some(serde_json::json!({"kind": "schedule"})))
            }
            fn approval_closed(
                &self,
                _ctx: &crate::ToolContext,
                _input: &serde_json::Value,
                _token: Option<&str>,
            ) {
                self.closed.store(true, Ordering::SeqCst);
            }
        }

        struct Wrap(Arc<dyn crate::Tool>);

        #[async_trait::async_trait]
        impl crate::ToolWrapper for Wrap {
            fn spec(&self) -> &ToolSpec {
                self.0.spec()
            }
            fn egress_for(&self, bot_id: Option<&str>) -> Option<bool> {
                self.0.egress_for(bot_id)
            }
            async fn call(
                &self,
                ctx: &crate::ToolContext,
                input: serde_json::Value,
            ) -> Result<crate::ToolOutput, ToolError> {
                self.0.call(ctx, input).await
            }
            fn always_ask(&self) -> bool {
                self.0.always_ask()
            }
            fn always_ask_for(&self, input: &serde_json::Value) -> bool {
                self.0.always_ask_for(input)
            }
            fn full_access_asks_for(&self, input: &serde_json::Value) -> bool {
                self.0.full_access_asks_for(input)
            }
            async fn standing_grant(
                &self,
                ctx: &crate::ToolContext,
                input: &serde_json::Value,
            ) -> Option<crate::StandingGrant> {
                self.0.standing_grant(ctx, input).await
            }
            async fn payment_page(
                &self,
                ctx: &crate::ToolContext,
                input: &serde_json::Value,
            ) -> Option<u64> {
                self.0.payment_page(ctx, input).await
            }
            async fn approval_details(
                &self,
                ctx: &crate::ToolContext,
                input: &serde_json::Value,
            ) -> Result<Option<serde_json::Value>, ToolError> {
                self.0.approval_details(ctx, input).await
            }
            fn approval_closed(
                &self,
                ctx: &crate::ToolContext,
                input: &serde_json::Value,
                token: Option<&str>,
            ) {
                self.0.approval_closed(ctx, input, token);
            }
        }

        let closed = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(Inner {
            spec: ToolSpec {
                name: "w".into(),
                description: "w".into(),
                input_schema: serde_json::json!({"type": "object"}),
                effect: Effect::Write,
                category: PermissionCategory::System,
                platforms: vec![protocol::Platform::All],

                content: ContentClass::none(),
            },
            closed: Arc::clone(&closed),
        });
        let wrapped: Arc<dyn crate::Tool> = Arc::new(Wrap(inner));
        assert!(wrapped.always_ask());
        // The per-call hooks must come back with the INNER tool's distinctive
        // answers, not the trait defaults (false / None).
        assert!(wrapped.always_ask_for(&serde_json::json!({"secret": "x"})));
        assert!(!wrapped.always_ask_for(&serde_json::json!({"text": "x"})));
        let grant = wrapped
            .standing_grant(&crate::ToolContext::default(), &serde_json::json!({}))
            .await
            .expect("the inner standing grant is forwarded");
        assert_eq!(grant.id, "g1");
        wrapped.approval_closed(
            &crate::ToolContext::default(),
            &serde_json::json!({}),
            Some("tok"),
        );
        assert!(closed.load(Ordering::SeqCst));
    }
}
