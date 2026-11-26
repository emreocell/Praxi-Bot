//! Per-site and per-app permission profiles in the decision pipeline (issue #227).
//!
//! The rules and their matching live in [`permission_engine::profiles`]; this module
//! decides what a call acts on and keeps the owner's rules.
//!
//! * The site is the destination of `browser_open`, or the origin the BROWSER reports
//!   for the frame that would receive a click / typing (never a tool argument or page
//!   text); the program is the verified image of a desktop input call.
//! * A browser action whose origin cannot be read, when the bot has a narrowing site
//!   rule that would cover its class, asks (it cannot be shown to be somewhere safe).
//!   A program that cannot be identified fails CLOSED: the strictest `block` / `ask` rule
//!   of the bot that covers the class applies (an `allow` rule never matches it).
//! * Reading a page (`browser_snapshot`, `browser_screenshot`) and `browser_back` are judged
//!   by the origin of the page that is open; after any browser call the page the browser
//!   LANDED on is checked against the `block` rules (a redirect or a click may have moved it),
//!   and a blocked page's content is withheld from the model.
//! * EVERY browser tool is judged (see [`BROWSER_CLASSES`]; one that is not listed is judged
//!   by the strictest class, so a new tool can never slip past a block), by the origin of the
//!   page that is open (`browser_open`: by its destination). The web fetch tools
//!   (`web_extract`, `web_feed`, ...) are judged by the address asked for, and a block also
//!   holds for every redirect hop they follow ([`tool_registry::with_url_guard`]).
//! * The landing check fails CLOSED: when the origin the browser ended on cannot be read
//!   (or is not an `http(s)` page such as `blob:` / `data:` / `about:`), the content of the
//!   call is withheld while the bot has a block / ask rule that covers reading.
//! * Rules apply on every computer the bot uses (the owner's computer and the host): a block
//!   is never weaker on another machine, and the owner sees that scope on the rules card.
//! * `block` and `ask` rules sit above grants, read shortcuts and autonomy modes; an
//!   `allow` rule counts as one more remembered grant and sits below every floor.
//! * Rules are kept as audit-log events (`profile_rule.set` / `.removed`), like the
//!   "always allow" grants: audited by construction, durable, and not part of the bot
//!   profile file, so profile sharing / export can never carry them.

use permission_engine::{
    Capability, ProfileAction, ProfileRule, Subject, SubjectKind, Verdict, evaluate, normalize_app,
    site_of_url,
};
use protocol::BotId;
use serde_json::{Value, json};
use storage::Storage;
use tool_registry::ControlTarget;
use uuid::Uuid;

use crate::shared::{RunCtx, Shared};

/// Audit action: a rule was created.
pub(crate) const AUDIT_PROFILE_SET: &str = "profile_rule.set";
/// Audit action: a rule was removed.
pub(crate) const AUDIT_PROFILE_REMOVED: &str = "profile_rule.removed";
/// Audit action: a rule decided (or narrowed) a call.
pub(crate) const AUDIT_PROFILE_APPLIED: &str = "profile_rule.applied";

/// How many rules one bot may hold.
pub(crate) const MAX_RULES_PER_BOT: usize = 200;

/// Every built-in browser tool and its class. A new browser tool MUST be added here: a
/// test walks the real browser registry and fails for a tool that is missing. A browser
/// tool that is not listed anyway is judged by all classes at once (see [`classes_of`]).
pub(crate) const BROWSER_CLASSES: &[(&str, Capability)] = &[
    ("browser_open", Capability::Read),
    ("browser_snapshot", Capability::Read),
    ("browser_screenshot", Capability::Read),
    ("browser_back", Capability::Read),
    ("browser_scroll", Capability::Read),
    ("browser_hover", Capability::Read),
    ("browser_wait", Capability::Read),
    ("browser_tabs", Capability::Read),
    ("browser_handover", Capability::Read),
    ("browser_click", Capability::Interact),
    ("browser_fill", Capability::Interact),
    ("browser_select", Capability::Interact),
    ("browser_select_text", Capability::Interact),
    ("browser_dismiss_consent", Capability::Interact),
    ("browser_press", Capability::Submit),
    ("browser_act", Capability::Submit),
    // A coordinate click can press any button, so it is judged like a key press / batch.
    ("browser_pointer", Capability::Submit),
    ("browser_attach", Capability::Read),
    ("browser_eval", Capability::Submit),
];

/// A web tool that fetches an address the model names and follows its redirects: every
/// `web_*` tool but `web_search` (which only talks to the configured search providers).
/// A prefix rule on purpose: a web tool added later is covered without anyone remembering.
pub(crate) fn is_web_fetch(tool: &str) -> bool {
    tool.starts_with("web_") && tool != "web_search"
}

/// A browser tool that acts on, or reads, the page that is open right now (everything but
/// `browser_open`, which is judged by where it goes). Deliberately a prefix rule, not a
/// list: a browser tool nobody classified is judged by the page too.
pub(crate) fn is_page_judged(tool: &str) -> bool {
    tool.starts_with("browser_") && tool != "browser_open"
}

fn is_unlisted_browser(tool: &str) -> bool {
    tool.starts_with("browser_") && !BROWSER_CLASSES.iter().any(|(n, _)| *n == tool)
}

/// The class of a built-in browser, web fetch or desktop tool call; `None` for every other
/// tool. An unlisted `browser_*` tool is `Submit` here (the most consequential class); its
/// verdict also looks at the other classes, see [`classes_of`].
#[must_use]
pub(crate) fn classify(tool: &str) -> Option<Capability> {
    if let Some((_, c)) = BROWSER_CLASSES.iter().find(|(n, _)| *n == tool) {
        return Some(*c);
    }
    match tool {
        t if is_unlisted_browser(t) => Some(Capability::Submit),
        t if is_web_fetch(t) => Some(Capability::Read),
        "desktop_type" | "desktop_set_value" => Some(Capability::Interact),
        "desktop_key" => Some(Capability::Submit),
        _ => None,
    }
}

/// The classes a call is judged by: its own, or every class for an unlisted browser tool
/// (fail closed: a rule for any class then speaks for it).
fn classes_of(tool: &str, class: Capability) -> Vec<Capability> {
    if is_unlisted_browser(tool) {
        vec![Capability::Read, Capability::Interact, Capability::Submit]
    } else {
        vec![class]
    }
}

/// What the call acts on, and whether the subject is exact enough for an `allow` rule.
/// `None`: not known (the caller decides what that means).
fn subject_of(
    tool: &str,
    input: &Value,
    page_origin: Option<&str>,
    target: Option<&ControlTarget>,
) -> Option<(Subject, bool)> {
    match tool {
        "browser_open" => {
            let host = site_of_url(input.get("url")?.as_str()?)?;
            Some((Subject::Site(host), true))
        }
        t if is_page_judged(t) => {
            let host = site_of_url(page_origin?)?;
            Some((Subject::Site(host), !is_unlisted_browser(t)))
        }
        t if is_web_fetch(t) => {
            let host = site_of_url(input.get("url")?.as_str()?)?;
            Some((Subject::Site(host), true))
        }
        "desktop_type" | "desktop_set_value" | "desktop_key" => {
            let target = target?;
            let path = resolve_app_key(&target.identity.image_path).ok()?;
            Some((Subject::App(path), target.eligible()))
        }
        _ => None,
    }
}

/// The subject of an approved call as `(kind, key, class)`, for "add from a recent
/// approval". Same derivation as the decision, so what is offered is what would match.
#[must_use]
pub(crate) fn approval_subject(
    tool: &str,
    input: &Value,
    page_origin: Option<&str>,
    target: Option<&ControlTarget>,
) -> Option<(SubjectKind, String, Capability)> {
    if is_web_fetch(tool) {
        return None;
    }
    let class = classify(tool)?;
    match subject_of(tool, input, page_origin, target)?.0 {
        Subject::Site(h) => Some((SubjectKind::Site, h, class)),
        Subject::App(p) => Some((SubjectKind::App, p, class)),
    }
}

impl Shared {
    /// Whether the bot has any rule (so the program a desktop call reaches is worth resolving).
    pub(crate) fn has_profile_rules(&self, bot: BotId) -> bool {
        self.profile_rules.lock().iter().any(|(b, _)| *b == bot)
    }

    /// The rule of this bot that speaks for this call, if any.
    pub(crate) fn profile_verdict(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
        target: Option<&ControlTarget>,
    ) -> Option<Verdict> {
        let origin = ctx.page_origin.lock().clone();
        self.profile_verdict_at(ctx.bot_id, name, input, origin.as_deref(), target)
    }

    /// [`Self::profile_verdict`] for an explicit page origin (a parked approval is judged by
    /// the origin it was shown for, not by whatever page is open now).
    pub(crate) fn profile_verdict_at(
        &self,
        bot_id: BotId,
        name: &str,
        input: &Value,
        origin: Option<&str>,
        target: Option<&ControlTarget>,
    ) -> Option<Verdict> {
        let class = classify(name)?;
        let now = storage::now_ms();
        let rules: Vec<ProfileRule> = {
            let all = self.profile_rules.lock();
            all.iter()
                .filter(|(b, r)| *b == bot_id && r.active(now))
                .map(|(_, r)| r.clone())
                .collect()
        };
        if rules.is_empty() {
            return None;
        }
        let known = subject_of(name, input, origin, target);
        if let Some((subject, trusted)) = &known {
            let mut best: Option<Verdict> = None;
            for c in classes_of(name, class) {
                let Some(v) = evaluate(&rules, subject, c, now, *trusted) else {
                    continue;
                };
                // A web fetch is never allowed or waved through by a rule: only a block / ask
                // narrows it (an allow rule is about opening the site in the browser).
                if is_web_fetch(name) && v.action == ProfileAction::Allow {
                    continue;
                }
                if best
                    .as_ref()
                    .is_none_or(|b| strictness(v.action) > strictness(b.action))
                {
                    best = Some(v);
                }
            }
            return best;
        }
        let classes = classes_of(name, class);
        let covers = |r: &ProfileRule, kind: SubjectKind| {
            r.kind == kind
                && r.action != ProfileAction::Allow
                && (r.capability == Capability::Any || classes.contains(&r.capability))
        };
        // A browser action that cannot be placed: ask when a narrowing site rule would
        // cover it somewhere (a navigation to an unreadable address keeps the default).
        if is_page_judged(name) {
            return rules
                .iter()
                .find(|r| covers(r, SubjectKind::Site))
                .map(|r| Verdict {
                    action: ProfileAction::Ask,
                    rule_id: r.id.clone(),
                });
        }
        // A desktop input call whose program could not be identified fails closed: the
        // strictest block / ask rule on a program that covers its class speaks for it.
        if matches!(name, "desktop_type" | "desktop_set_value" | "desktop_key") {
            return rules
                .iter()
                .filter(|r| covers(r, SubjectKind::App))
                .max_by_key(|r| u8::from(r.action == ProfileAction::Block))
                .map(|r| Verdict {
                    action: r.action,
                    rule_id: r.id.clone(),
                });
        }
        None
    }

    /// After a browser call: the page the browser LANDED on (a redirect from an allowed
    /// site, a click or a back step) is checked against the rules. `Some(text)` means what
    /// the call returned must not reach the model: the page is blocked, OR its origin could
    /// not be verified (unreadable, or not an `http(s)` page such as `blob:` / `data:` /
    /// `about:`) while the bot has a block / ask rule that covers reading a site. That check
    /// fails CLOSED: not knowing where the browser is never lets content through.
    pub(crate) async fn blocked_landing(
        &self,
        ctx: &RunCtx,
        tool: &std::sync::Arc<dyn tool_registry::Tool>,
        tctx: &tool_registry::ToolContext,
    ) -> Option<String> {
        let now = storage::now_ms();
        let rules: Vec<ProfileRule> = {
            let all = self.profile_rules.lock();
            all.iter()
                .filter(|(b, r)| *b == ctx.bot_id && r.active(now))
                .map(|(_, r)| r.clone())
                .collect()
        };
        let narrows_reads = |r: &&ProfileRule| {
            r.kind == SubjectKind::Site
                && r.action != ProfileAction::Allow
                && matches!(r.capability, Capability::Read | Capability::Any)
        };
        if !rules.iter().any(|r| narrows_reads(&r)) {
            return None;
        }
        let landed = tool.landed_origin(tctx).await;
        let Some(host) = landed.as_deref().and_then(site_of_url) else {
            let rule = rules.iter().find(narrows_reads)?;
            self.audit_profile_applied(
                ctx,
                "landing_unverified",
                &Verdict {
                    action: ProfileAction::Block,
                    rule_id: rule.id.clone(),
                },
            );
            return Some(LANDING_UNVERIFIED_TEXT.to_owned());
        };
        let v = evaluate(&rules, &Subject::Site(host), Capability::Read, now, true)?;
        (v.action == ProfileAction::Block).then(|| {
            self.audit_profile_applied(ctx, "landing", &v);
            LANDING_BLOCKED_TEXT.to_owned()
        })
    }

    /// The guard for a web fetch tool's own redirects (see [`tool_registry::with_url_guard`]),
    /// `None` when the call is not a web fetch or the bot has no rule.
    pub(crate) fn url_guard_for(
        self: &std::sync::Arc<Self>,
        ctx: &RunCtx,
        tool: &str,
    ) -> Option<std::sync::Arc<dyn tool_registry::UrlGuard>> {
        if !is_web_fetch(tool) || !self.has_profile_rules(ctx.bot_id) {
            return None;
        }
        Some(std::sync::Arc::new(RuleUrlGuard {
            sh: std::sync::Arc::clone(self),
            bot: ctx.bot_id,
            run: ctx.run_id,
            user: ctx.user_id,
        }))
    }

    /// Why a call was blocked by a rule, for the model and the owner's activity feed.
    pub(crate) fn profile_block_text(
        &self,
        ctx: &RunCtx,
        name: &str,
        input: &Value,
        target: Option<&ControlTarget>,
    ) -> Option<String> {
        let v = self.profile_verdict(ctx, name, input, target)?;
        (v.action == ProfileAction::Block).then(|| {
            "Permission denied: the owner set a rule for this site or program that blocks this. \
             Do not retry it; choose another approach or tell the user."
                .to_owned()
        })
    }

    /// Record that a rule decided or narrowed a call.
    pub(crate) fn audit_profile_applied(&self, ctx: &RunCtx, tool: &str, v: &Verdict) {
        self.audit(
            ctx,
            "system",
            AUDIT_PROFILE_APPLIED,
            &json!({
                "bot_id": ctx.bot_id.to_string(),
                "tool": tool,
                "rule_id": v.rule_id,
                "action": v.action.as_str(),
            }),
        );
    }
}

/// What the model is told when the page the browser ended on is blocked.
const LANDING_BLOCKED_TEXT: &str = "Permission denied: the page this ended up on is blocked by an owner rule for that site, so its content is withheld. Do not go back to it; choose another approach or tell the user.";

/// What the model is told when the browser's page could not be verified.
const LANDING_UNVERIFIED_TEXT: &str = "Blocked: page origin could not be verified. The owner has a rule about sites, and the browser did not report a normal web address for the page this ended on (it may be a blob:, data: or about: page), so its content is withheld. Open a normal https page with browser_open, or tell the user.";

fn strictness(a: ProfileAction) -> u8 {
    match a {
        ProfileAction::Allow => 0,
        ProfileAction::Ask => 1,
        ProfileAction::Block => 2,
    }
}

/// The owner's site rules for the addresses a web fetch tool contacts itself.
struct RuleUrlGuard {
    sh: std::sync::Arc<Shared>,
    bot: BotId,
    run: protocol::RunId,
    user: protocol::UserId,
}

impl tool_registry::UrlGuard for RuleUrlGuard {
    fn blocked(&self, url: &str, redirect_hop: bool) -> bool {
        let now = storage::now_ms();
        let rules: Vec<ProfileRule> = {
            let all = self.sh.profile_rules.lock();
            all.iter()
                .filter(|(b, r)| *b == self.bot && r.active(now))
                .map(|(_, r)| r.clone())
                .collect()
        };
        // An address that has no readable web host cannot be shown to be somewhere safe.
        let Some(host) = site_of_url(url) else {
            return !rules.is_empty();
        };
        let Some(v) = evaluate(&rules, &Subject::Site(host), Capability::Read, now, true) else {
            return false;
        };
        // A block always refuses. An ask was put to the owner for the address that was asked
        // for; a redirect target cannot be, so it is refused (stricter, never weaker).
        let refused = match v.action {
            ProfileAction::Block => true,
            ProfileAction::Ask => redirect_hop,
            ProfileAction::Allow => false,
        };
        if refused {
            let _ = self.sh.storage.append_audit(
                self.user,
                Some(self.run),
                "system",
                AUDIT_PROFILE_APPLIED,
                &json!({
                    "bot_id": self.bot.to_string(),
                    "tool": if redirect_hop { "web_redirect" } else { "web_fetch" },
                    "rule_id": v.rule_id,
                    "action": "block",
                }),
            );
        }
        refused
    }
}

/// Why a program rule's key could not be turned into the form a running program is matched by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppKeyError {
    /// Not an absolute program path.
    NotAPath,
    /// A script (`.bat` / `.cmd`) is never a program image: the program that runs it is.
    Script,
    /// A short `NAME~1` path that this computer cannot expand to the real one.
    ShortName,
}

impl std::fmt::Display for AppKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAPath => "not an absolute program path",
            Self::Script => {
                "a script is not a program: name the program that runs it (for example cmd.exe)"
            }
            Self::ShortName => {
                "this path uses a short 8.3 name (NAME~1) that could not be resolved to the real path: type the full path"
            }
        })
    }
}

/// Whether a path component looks like an 8.3 short name (`PROGRA~1`, `APP~2.EXE`).
fn is_short_name(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or(part);
    base.len() <= 8
        && base.rsplit_once('~').is_some_and(|(head, n)| {
            !head.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The path the OS knows the program by: short names expanded and junctions / symlinks
/// resolved when the file exists on THIS computer. A path of another operating system, or
/// one that is not on this computer, keeps its lexical form (`None`).
fn os_resolved(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    let native = if cfg!(windows) {
        drive
    } else {
        path.starts_with('/')
    };
    if !native {
        return None;
    }
    let real = std::fs::canonicalize(path).ok()?;
    if real.is_dir() {
        return None;
    }
    let text = real.to_string_lossy().into_owned();
    // Windows answers `\\?\C:\...`; a UNC share (`\\?\UNC\...`) stays as it is (it then fails
    // `normalize_app`, which fails closed).
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => Some(rest.to_owned()),
        _ => Some(text),
    }
}

/// The one form of a program path that rule keys and the identity of a running program are
/// both brought to before they are compared: quotes and spaces removed, `/` separators,
/// `.exe` added when a Windows path has no program extension, lower case on Windows, and
/// (when the file is on this computer) short names expanded and junctions resolved.
///
/// # Errors
/// [`AppKeyError`] for a path that cannot name one program.
pub(crate) fn resolve_app_key(raw: &str) -> Result<String, AppKeyError> {
    let lexical = normalize_app(raw).ok_or(AppKeyError::NotAPath)?;
    let resolved = os_resolved(&lexical)
        .and_then(|p| normalize_app(&p))
        .unwrap_or_else(|| lexical.clone());
    let is_drive = resolved.as_bytes().get(1) == Some(&b':');
    if is_drive {
        let last = resolved.rsplit('/').next().unwrap_or_default();
        if matches!(last.rsplit_once('.'), Some((_, "bat" | "cmd"))) {
            return Err(AppKeyError::Script);
        }
        if resolved == lexical && resolved.split('/').any(is_short_name) {
            // The OS did not change it and it is still short: it could not be expanded.
            return Err(AppKeyError::ShortName);
        }
    }
    Ok(resolved)
}

/// Rebuild every bot's rules from the audit log, oldest event first.
pub(crate) fn load_profile_rules(storage: &Storage) -> Vec<(BotId, ProfileRule)> {
    let mut rules: Vec<(BotId, ProfileRule)> = Vec::new();
    for user in storage.list_users().unwrap_or_default() {
        for e in storage.list_audit(user.id).unwrap_or_default() {
            let bot = e.detail["bot_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(BotId);
            let Some(bot) = bot else { continue };
            match e.action.as_str() {
                AUDIT_PROFILE_SET => {
                    if let Ok(mut rule) =
                        serde_json::from_value::<ProfileRule>(e.detail["rule"].clone())
                    {
                        // A program key stored before keys were normalised (no `.exe`, a short
                        // or junction path) is brought to the matching form; one that cannot
                        // be keeps its text (and stays in force, as stored).
                        if rule.kind == SubjectKind::App
                            && let Ok(key) = resolve_app_key(&rule.key)
                        {
                            rule.key = key;
                        }
                        rules.retain(|(_, r)| r.id != rule.id);
                        rules.push((bot, rule));
                    }
                }
                AUDIT_PROFILE_REMOVED => {
                    if let Some(id) = e.detail["rule_id"].as_str() {
                        rules.retain(|(b, r)| !(*b == bot && r.id == id));
                    }
                }
                _ => {}
            }
        }
    }
    rules
}

/// One site or program the owner recently approved something on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalSubject {
    /// Site or program.
    pub kind: SubjectKind,
    /// Normalised host or image path.
    pub key: String,
    /// The class of the approved call.
    pub capability: Capability,
    /// When it was approved (ms since the Unix epoch).
    pub approved_at_ms: i64,
}

/// Recent approvals of one bot that named a site or program, newest first, one per
/// subject and class.
pub(crate) fn recent_subjects(
    storage: &Storage,
    user: protocol::UserId,
    bot: BotId,
    limit: usize,
) -> Vec<ApprovalSubject> {
    let since = storage::now_ms() - 30 * 24 * 3600 * 1000;
    let mut entries = storage
        .list_audit_action_since(user, "approval.approved", since)
        .unwrap_or_default();
    entries.reverse();
    let mut seen: std::collections::HashSet<(SubjectKind, String, Capability)> =
        std::collections::HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        if e.detail["bot_id"].as_str() != Some(&bot.to_string()) {
            continue;
        }
        let s = &e.detail["subject"];
        let kind = match s["kind"].as_str() {
            Some("site") => SubjectKind::Site,
            Some("app") => SubjectKind::App,
            _ => continue,
        };
        let capability = match s["capability"].as_str() {
            Some("read") => Capability::Read,
            Some("interact") => Capability::Interact,
            Some("submit") => Capability::Submit,
            _ => continue,
        };
        let Some(key) = s["key"].as_str() else {
            continue;
        };
        if seen.insert((kind, key.to_owned(), capability)) {
            out.push(ApprovalSubject {
                kind,
                key: key.to_owned(),
                capability,
                approved_at_ms: e.ts_ms,
            });
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}
