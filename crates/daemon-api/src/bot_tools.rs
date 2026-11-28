//! The daemon side of the bot management tools: a [`BotHost`] over the bot
//! store.
//!
//! The security rules live here, in one place, and run for every preview and
//! again, from scratch, when the change is applied:
//!
//! * **Ceiling.** A created or updated bot never has broader permissions, tool
//!   surface or model/provider access than the calling bot, never a `full`
//!   preset, and gets no MCP access, skills or computer target the caller does
//!   not have (MCP servers list bots explicitly and a new bot is on no list;
//!   skills are per-bot folders a new bot does not have; `runtime` is copied
//!   from the caller). `bot_update` can only lower permissions and narrow
//!   tools of its target, never raise them.
//! * **Provenance.** A bot manages only bots stamped `bot.created_by = <its
//!   id>` at creation (stamped here, never from input), never itself, and
//!   only while the target is still within its own ceiling (a bot the user
//!   widened later is read-only to it).
//! * **Exactly what was approved.** `preview` freezes the resulting profile;
//!   the apply recomputes it and refuses unless it is identical (a call that
//!   was never previewed, or whose bot changed meanwhile, fails).
//! * Audit first: a failed audit write fails the change.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use builtin_tools::{
    BOT_PRESETS, BOT_SHAPES, BotCaller, BotChange, BotHost, BotPreview, BotRequest, BotView,
    FieldChange,
};
use permission_engine::{Level, Policy, Rule};
use protocol::{BotId, PermissionCategory, RunId, UserId};
use serde_json::{Map, Value, json};
use storage::{Bot, Storage, now_ms};
use uuid::Uuid;

use crate::dto::BotInput;
use crate::schedule_tools::sha256_hex;
use crate::{AppState, WeakAppState};

/// Longest bot name / persona, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// See [`MAX_NAME_CHARS`].
pub const MAX_PERSONA_CHARS: usize = 8_000;
/// Longest avatar data URL, in bytes.
pub const MAX_AVATAR_BYTES: usize = 200_000;
/// Most bots one bot may have created (and still own).
pub const MAX_BOTS_PER_CREATOR: usize = 10;
/// Most entries of a tool list.
const MAX_TOOLS: usize = 64;
/// A preview is valid this long; at most this many are kept.
const FROZEN_TTL_MS: i64 = 6 * 3_600_000;
const FROZEN_MAX: usize = 64;

const CATEGORIES: [PermissionCategory; 10] = [
    PermissionCategory::Fs,
    PermissionCategory::Shell,
    PermissionCategory::Desktop,
    PermissionCategory::Browser,
    PermissionCategory::Accounts,
    PermissionCategory::Messaging,
    PermissionCategory::Money,
    PermissionCategory::System,
    PermissionCategory::Remote,
    PermissionCategory::Script,
];

/// Trim and check free text: not empty, at most `max` characters, no control
/// characters (newline and tab only when `lines`), no invisible or bidi
/// controls (the card shows this text to the user: it must be what it looks like).
fn clean_text(field: &str, raw: &str, max: usize, lines: bool) -> Result<String, String> {
    let normalized = raw.replace("\r\n", "\n");
    let t = normalized.trim();
    if t.is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    let n = t.chars().count();
    if n > max {
        return Err(format!("`{field}` is {n} characters; the limit is {max}"));
    }
    for c in t.chars() {
        let bad_control = c.is_control() && !(lines && (c == '\n' || c == '\t'));
        if bad_control || protocol::approval_text::is_hidden(c) {
            return Err(format!(
                "`{field}` contains a control or invisible character (U+{:04X}); use plain text",
                u32::from(c)
            ));
        }
    }
    Ok(t.to_owned())
}

fn clean_color(raw: &str) -> Result<String, String> {
    let c = raw.trim();
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if ok {
        Ok(c.to_ascii_lowercase())
    } else {
        Err("`color` must look like #rrggbb".into())
    }
}

fn clean_shape(raw: &str) -> Result<String, String> {
    let s = raw.trim().to_ascii_lowercase();
    if BOT_SHAPES.contains(&s.as_str()) {
        Ok(s)
    } else {
        Err(format!("`shape` must be one of {}", BOT_SHAPES.join(", ")))
    }
}

fn clean_avatar(raw: &str) -> Result<String, String> {
    const PREFIXES: [&str; 4] = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/gif;base64,",
        "data:image/webp;base64,",
    ];
    let a = raw.trim();
    let Some(prefix) = PREFIXES.iter().find(|p| a.starts_with(**p)) else {
        return Err(
            "`avatar` must be a data:image/png|jpeg|gif|webp;base64 URL (remote URLs are not accepted)".into(),
        );
    };
    if a.len() > MAX_AVATAR_BYTES {
        return Err(format!(
            "`avatar` is {} bytes; the limit is {MAX_AVATAR_BYTES}",
            a.len()
        ));
    }
    let data = &a[prefix.len()..];
    if data.is_empty()
        || !data
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
    {
        return Err("`avatar` is not valid base64".into());
    }
    Ok(a.to_owned())
}

fn clean_model(field: &str, raw: &str) -> Result<String, String> {
    let m = raw.trim();
    if m.is_empty()
        || m.chars().count() > 120
        || m.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("`{field}` must be a `provider/model` name"));
    }
    Ok(m.to_owned())
}

fn provider_of(model: &str) -> &str {
    model.split_once('/').map_or("", |(p, _)| p)
}

fn clean_tools(list: &[String]) -> Result<Vec<String>, String> {
    if list.len() > MAX_TOOLS {
        return Err(format!(
            "`tools` has {} entries; the limit is {MAX_TOOLS}",
            list.len()
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for t in list {
        let t = t.trim();
        let ok = !t.is_empty()
            && t.chars().count() <= 100
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.:/*?[]{}".contains(c));
        if !ok {
            return Err(format!("tool name {t:?} is not valid"));
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_owned());
        }
    }
    Ok(out)
}

fn preset_policy(name: &str) -> Option<Policy> {
    match name {
        "read-only" => Some(Policy::read_only()),
        "read-and-draft" => Some(Policy::read_and_draft()),
        "approve-destructive" => Some(Policy::approve_destructive()),
        "full" => Some(Policy::full()),
        _ => None,
    }
}

/// Every level `rule` can yield (its base level and per-initiator overrides).
fn rule_levels(rule: Option<&Rule>) -> Vec<Level> {
    rule.map_or_else(
        || vec![Level::Ask],
        |r| {
            let mut v = vec![r.level.clone()];
            v.extend(r.by_initiator.values().cloned());
            v
        },
    )
}

/// The rule of `cat`: the policy's own, else the global one.
fn rule_of<'a>(p: &'a Policy, global: &'a Policy, cat: PermissionCategory) -> Option<&'a Rule> {
    p.categories
        .get(&cat)
        .or_else(|| global.categories.get(&cat))
}

/// `Ok` when `cand` never allows more than `cap`, in any category and for any
/// initiator (most permissive level of `cand` against least permissive of `cap`).
fn policy_within(what: &str, cand: &Policy, cap: &Policy, global: &Policy) -> Result<(), String> {
    for cat in CATEGORIES {
        let c_max = rule_levels(rule_of(cand, global, cat))
            .into_iter()
            .fold(None::<Level>, |m, l| match m {
                Some(m) if m.is_at_least_as_permissive_as(&l) => Some(m),
                _ => Some(l),
            })
            .unwrap_or(Level::Ask);
        let k_min = rule_levels(rule_of(cap, global, cat))
            .into_iter()
            .fold(None::<Level>, |m, l| match m {
                Some(m) if l.is_at_least_as_permissive_as(&m) => Some(m),
                _ => Some(l),
            })
            .unwrap_or(Level::Ask);
        if !k_min.is_at_least_as_permissive_as(&c_max) {
            return Err(format!(
                "{what}: permission `{cat:?}` would be broader than allowed ({c_max:?} against {k_min:?})"
            ));
        }
    }
    Ok(())
}

/// How freely a mode lets calls run without a prompt.
fn mode_rank(m: permission_engine::AutonomyMode) -> u8 {
    use permission_engine::AutonomyMode as M;
    match m {
        M::Ask => 0,
        M::Balanced => 1,
        M::Full => 2,
    }
}

/// The mode a policy-holder's preset gives it. A profile with per-category
/// rules counts as `Ask` when used as a ceiling: nobody gets a lane from a
/// hand-made policy.
fn ceiling_mode(p: &agent_core::PermissionsConfig) -> permission_engine::AutonomyMode {
    if p.categories.is_empty() {
        p.mode()
    } else {
        permission_engine::AutonomyMode::Ask
    }
}

/// `Ok` when `cand` prompts at least as often as `cap` allows. `Policy` alone
/// cannot tell `read-and-draft` from `approve-destructive`; the autonomy mode can.
fn mode_within(
    what: &str,
    cand: permission_engine::AutonomyMode,
    cap: permission_engine::AutonomyMode,
) -> Result<(), String> {
    if mode_rank(cand) > mode_rank(cap) {
        return Err(format!(
            "{what}: autonomy `{}` would let calls run without asking more than allowed (`{}`)",
            cand.as_str(),
            cap.as_str()
        ));
    }
    Ok(())
}

/// `Ok` when `cand` reaches no more of the file system than `cap`. An unset
/// mode means "the host's default", so it is only within another unset mode.
fn files_within(
    what: &str,
    cand: &agent_core::FilesConfig,
    cap: &agent_core::FilesConfig,
) -> Result<(), String> {
    use agent_core::FileAccessMode as F;
    let broader = || {
        Err(format!(
            "{what}: file access would be broader than yours; use `folder` mode inside your own folders"
        ))
    };
    match (cap.mode, cand.mode) {
        (Some(F::Computer), _) | (None, None) => Ok(()),
        (None, Some(F::Folder)) if cand.folders.is_empty() => Ok(()),
        (Some(F::Folder), Some(F::Folder)) => {
            let inside = |f: &String| {
                let p = std::path::Path::new(f);
                !p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                    && cap.folders.iter().any(|c| p.starts_with(c))
            };
            if cand.folders.iter().all(inside) {
                Ok(())
            } else {
                broader()
            }
        }
        _ => broader(),
    }
}

/// A bot's own policy, else the global one.
fn policy_of(profile: &agent_core::BotProfile, global: &Policy) -> Policy {
    profile.policy().unwrap_or_else(|| global.clone())
}

fn preset_name(config: &Value) -> String {
    let perms = config.get("permissions");
    if perms
        .and_then(|p| p.get("categories"))
        .and_then(Value::as_object)
        .is_some_and(|c| !c.is_empty())
    {
        return "custom".into();
    }
    perms
        .and_then(|p| p.get("preset"))
        .and_then(Value::as_str)
        .map_or_else(|| "inherited".into(), |p| p.replace('_', "-"))
}

fn created_by(config: &Value) -> Option<&str> {
    config.pointer("/bot/created_by").and_then(Value::as_str)
}

fn tool_list(config: &Value) -> Vec<String> {
    config
        .pointer("/tools/allow")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn models_of(config: &Value) -> Vec<String> {
    let mut v = Vec::new();
    for k in ["executor", "planner", "classifier"] {
        if let Some(s) = config
            .pointer(&format!("/models/{k}"))
            .and_then(Value::as_str)
        {
            v.push(s.to_owned());
        }
    }
    if let Some(a) = config.pointer("/models/fallback").and_then(Value::as_array) {
        v.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    v
}

fn providers_of(config: &Value) -> BTreeSet<String> {
    models_of(config)
        .iter()
        .map(|m| provider_of(m).to_owned())
        .collect()
}

/// A profile with the avatar bytes replaced by a size note (the card shows the
/// image separately).
fn redacted(config: &Value) -> Value {
    let mut c = config.clone();
    if let Some(b) = c.pointer_mut("/bot").and_then(Value::as_object_mut)
        && let Some(a) = b.get("avatar").and_then(Value::as_str)
    {
        let note = format!("<image, {} bytes>", a.len());
        b.insert("avatar".into(), Value::String(note));
    }
    c
}

fn flatten(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&p, x, out);
            }
        }
        Value::String(s) => {
            out.insert(prefix.to_owned(), s.clone());
        }
        other => {
            out.insert(prefix.to_owned(), other.to_string());
        }
    }
}

fn changes(old: &Value, new: &Value) -> Vec<FieldChange> {
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    flatten("", &redacted(old), &mut a);
    flatten("", &redacted(new), &mut b);
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| FieldChange {
            field: k.clone(),
            from: a.get(k).cloned(),
            to: b.get(k).cloned(),
        })
        .collect()
}

fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-');
    if s.is_empty() {
        "bot".into()
    } else {
        s.chars().take(40).collect()
    }
}

/// What the preview resolved; applied as-is later.
struct Resolved {
    action: &'static str,
    target: Option<Bot>,
    name: String,
    /// Profile after the change (for delete: the profile that goes away).
    config: Value,
    /// `sha256` of the target's config before the change.
    base: Option<String>,
}

struct Frozen {
    key: String,
    nonce: String,
    made_ms: i64,
    name: String,
    config: Value,
    base: Option<String>,
}

/// Everything known about the calling bot.
struct Caller {
    bot: Bot,
    profile: agent_core::BotProfile,
    policy: Policy,
    global: Policy,
    providers: BTreeSet<String>,
}

/// [`BotHost`] over the daemon's bot store.
pub(crate) struct DaemonBotHost {
    app: WeakAppState,
    storage: std::sync::Arc<Storage>,
    user_id: UserId,
    clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
    /// Serialises "count, then insert" so the per-creator quota cannot be raced.
    gate: parking_lot::Mutex<()>,
    frozen: parking_lot::Mutex<Vec<Frozen>>,
}

impl DaemonBotHost {
    pub(crate) fn new(app: &AppState) -> Self {
        Self {
            app: app.downgrade(),
            storage: app.storage_arc(),
            user_id: app.user_id(),
            clock: std::sync::Arc::new(now_ms),
            gate: parking_lot::Mutex::new(()),
            frozen: parking_lot::Mutex::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_clock(
        mut self,
        clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        self.clock = clock;
        self
    }

    fn app(&self) -> Result<AppState, String> {
        self.app
            .upgrade()
            .ok_or_else(|| "the daemon is shutting down".to_owned())
    }

    fn bot_by_id(&self, id: &str) -> Result<Option<Bot>, String> {
        let Ok(uuid) = Uuid::parse_str(id.trim()) else {
            return Ok(None);
        };
        Ok(self
            .storage
            .get_bot(BotId(uuid))
            .map_err(|e| e.to_string())?
            .filter(|b| b.user_id == self.user_id))
    }

    /// The "Can create bots" switch of the bot's profile.
    fn manages(config: &Value) -> bool {
        config.pointer("/tools/manage_bots") == Some(&Value::Bool(true))
    }

    /// The bot is at the Full level (the loop offers it the bot tools in the owner's own chat;
    /// the level alone is never enough to use them, see [`Self::caller`]).
    fn full_level(config: &Value) -> bool {
        serde_json::from_value::<agent_core::BotProfile>(config.clone())
            .is_ok_and(|p| p.permissions.mode() == permission_engine::AutonomyMode::Full)
    }

    /// The run is the owner's own chat turn on a Full bot, as the agent loop knows it (the run
    /// id is the loop's, never the model's).
    fn owner_full_chat(&self, who: &BotCaller) -> bool {
        self.app()
            .is_ok_and(|app| app.agent().bot_tools_offered_to_run(&who.run_id))
    }

    /// The calling bot, for ONE call: the one gate of `list`, `preview`, `create`, `update` and
    /// `delete` (each of them resolves its change through here, so an approved preview that is
    /// applied later is judged again). A bot with the switch may use the tools in any run (its
    /// changes are shown on a card); a bot at the Full level only in the owner's own chat turn,
    /// as the loop knows that run right now (never a schedule, a mission, a helper or a run that
    /// read untrusted content).
    fn caller(&self, who: &BotCaller) -> Result<Caller, String> {
        let bot = self
            .bot_by_id(&who.bot_id)?
            .ok_or("the calling bot does not exist")?;
        if !Self::manages(&bot.config) && !self.owner_full_chat(who) {
            return Err("bot management is only for the owner's own chat on a Full bot, or a bot with \"Can create bots\" switched on in its settings".into());
        }
        let profile: agent_core::BotProfile = serde_json::from_value(bot.config.clone())
            .map_err(|e| format!("the calling bot's profile is invalid: {e}"))?;
        let global = self.app()?.agent().global_policy();
        let policy = policy_of(&profile, &global);
        let providers = providers_of(&bot.config);
        Ok(Caller {
            bot,
            profile,
            policy,
            global,
            providers,
        })
    }

    /// A bot the caller manages: found, not itself, made by it, and still
    /// within its own ceiling.
    fn managed_target(&self, c: &Caller, id: &str) -> Result<Bot, String> {
        let target = self
            .bot_by_id(id)?
            .ok_or_else(|| format!("there is no bot with id {id:?}"))?;
        if target.id == c.bot.id {
            return Err("a bot cannot change or delete itself".into());
        }
        if created_by(&target.config) != Some(c.bot.id.to_string().as_str()) {
            return Err(format!(
                "bot {id:?} was not created by you; you can only change or delete bots you created (the user changes the others in the app)"
            ));
        }
        Self::within_ceiling(c, &target, "this bot")?;
        Ok(target)
    }

    /// `target` is not broader than the caller in tools, permissions or models.
    fn within_ceiling(c: &Caller, target: &Bot, what: &str) -> Result<(), String> {
        let tp: agent_core::BotProfile = serde_json::from_value(target.config.clone())
            .map_err(|e| format!("{what} has an invalid profile: {e}"))?;
        c.profile
            .covers_tool_filter(&tp.tools)
            .map_err(|e| format!("{what} has more tools than you, so you can only read it: {e}"))?;
        policy_within(
            &format!("{what} has more permissions than you, so you can only read it"),
            &policy_of(&tp, &c.global),
            &c.policy,
            &c.global,
        )?;
        mode_within(
            &format!("{what} has more autonomy than you, so you can only read it"),
            tp.permissions.mode(),
            ceiling_mode(&c.profile.permissions),
        )?;
        files_within(
            &format!("{what} reaches more files than you, so you can only read it"),
            &tp.files,
            &c.profile.files,
        )?;
        if let Some(p) = providers_of(&target.config)
            .iter()
            .find(|p| !c.providers.contains(*p))
        {
            return Err(format!(
                "{what} uses provider {p:?}, which you do not use yourself, so you can only read it"
            ));
        }
        Ok(())
    }

    fn model_ok(c: &Caller, field: &str, model: &str) -> Result<(), String> {
        if c.providers.contains(provider_of(model)) {
            Ok(())
        } else {
            Err(format!(
                "`{field}` uses provider {:?}, which you do not use yourself; allowed: {}",
                provider_of(model),
                c.providers
                    .iter()
                    .map(|p| format!("{p:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }

    /// The preset to use: the one asked for, or the strongest that fits.
    fn preset_for(
        c: &Caller,
        asked: Option<&str>,
        floor: Option<(&Policy, permission_engine::AutonomyMode)>,
    ) -> Result<String, String> {
        let fits = |name: &str| -> Result<(), String> {
            let p = preset_policy(name).ok_or("unknown preset")?;
            policy_within("the new bot", &p, &c.policy, &c.global)?;
            let mode = permission_engine::AutonomyMode::from_preset(Some(name));
            mode_within("the new bot", mode, ceiling_mode(&c.profile.permissions))?;
            if let Some((f, fmode)) = floor {
                mode_within(
                    "an update can only lower autonomy; the new level",
                    mode,
                    fmode,
                )?;
                policy_within(
                    "an update can only lower permissions; the new level",
                    &p,
                    f,
                    &c.global,
                )?;
            }
            Ok(())
        };
        if let Some(a) = asked {
            let a = a.trim().replace('_', "-");
            if !BOT_PRESETS.contains(&a.as_str()) {
                return Err(format!(
                    "`permission_preset` must be one of {}; `full` and custom policies can only be set by the user in the app",
                    BOT_PRESETS.join(", ")
                ));
            }
            fits(&a).map_err(|e| format!("{e}. You cannot hand out more than you have"))?;
            return Ok(a);
        }
        BOT_PRESETS
            .iter()
            .rev()
            .find(|n| fits(n).is_ok())
            .map(|n| (*n).to_owned())
            .ok_or_else(|| "none of the permission presets fits within your own permissions".into())
    }

    fn created_count(&self, who: &str) -> Result<usize, String> {
        Ok(self
            .storage
            .list_bots(self.user_id)
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|b| created_by(&b.config) == Some(who))
            .count())
    }

    fn resolve(&self, who: &BotCaller, change: &BotChange) -> Result<Resolved, String> {
        let c = self.caller(who)?;
        match change {
            BotChange::Create(req) => self.resolve_create(&c, req),
            BotChange::Update { id, req } => self.resolve_update(&c, id, req),
            BotChange::Delete { id } => {
                let target = self.managed_target_for_delete(&c, id)?;
                Ok(Resolved {
                    action: "delete",
                    name: target.name.clone(),
                    config: target.config.clone(),
                    base: Some(sha256_hex(&target.config.to_string())),
                    target: Some(target),
                })
            }
        }
    }

    /// Delete only needs provenance and "not itself": removing a bot widens
    /// nothing, so a bot the user widened later can still be removed by its maker.
    fn managed_target_for_delete(&self, c: &Caller, id: &str) -> Result<Bot, String> {
        let target = self
            .bot_by_id(id)?
            .ok_or_else(|| format!("there is no bot with id {id:?}"))?;
        if target.id == c.bot.id {
            return Err("a bot cannot change or delete itself".into());
        }
        if created_by(&target.config) != Some(c.bot.id.to_string().as_str()) {
            return Err(format!(
                "bot {id:?} was not created by you; you can only change or delete bots you created"
            ));
        }
        Ok(target)
    }

    fn resolve_create(&self, c: &Caller, req: &BotRequest) -> Result<Resolved, String> {
        let name = clean_text(
            "name",
            req.name.as_deref().ok_or("`name` is required")?,
            MAX_NAME_CHARS,
            false,
        )?;
        let persona = clean_text(
            "persona",
            req.persona.as_deref().ok_or("`persona` is required")?,
            MAX_PERSONA_CHARS,
            true,
        )?;
        if self.created_count(&c.bot.id.to_string())? >= MAX_BOTS_PER_CREATOR {
            return Err(format!(
                "you already created {MAX_BOTS_PER_CREATOR} bots; delete one first"
            ));
        }
        let caller_exec = c.profile.models.executor.clone();
        let executor = clean_model(
            "executor",
            req.executor.as_deref().unwrap_or(caller_exec.as_str()),
        )?;
        Self::model_ok(c, "executor", &executor)?;
        let mut models = Map::new();
        models.insert("executor".into(), json!(executor));
        for (k, v) in [("planner", &req.planner), ("classifier", &req.classifier)] {
            if let Some(m) = v.as_deref().filter(|m| !m.trim().is_empty()) {
                let m = clean_model(k, m)?;
                Self::model_ok(c, k, &m)?;
                models.insert(k.into(), json!(m));
            }
        }
        let preset = Self::preset_for(c, req.permission_preset.as_deref(), None)?;
        let allow = match &req.tools {
            Some(t) => clean_tools(t)?,
            None => c.profile.tools.allow.clone(),
        };
        let exclude = c.profile.tools.exclude.clone();
        let filter = agent_core::ToolFilter {
            allow: allow.clone(),
            exclude: exclude.clone(),
            ..agent_core::ToolFilter::default()
        };
        c.profile.covers_tool_filter(&filter)?;
        let id = format!(
            "{}-{}",
            slug(&name),
            &sha256_hex(&format!("{}|{name}|{persona}", c.bot.id))[..5]
        );
        let mut bot = Map::new();
        bot.insert("id".into(), json!(id));
        bot.insert("name".into(), json!(name));
        bot.insert("persona".into(), json!(persona));
        if let Some(col) = &req.color {
            bot.insert("color".into(), json!(clean_color(col)?));
        }
        if let Some(shape) = &req.shape {
            bot.insert("shape".into(), json!(clean_shape(shape)?));
        }
        if let Some(a) = &req.avatar {
            bot.insert("avatar".into(), json!(clean_avatar(a)?));
        }
        bot.insert("created_by".into(), json!(c.bot.id.to_string()));
        let mut config = Map::new();
        config.insert("bot".into(), Value::Object(bot));
        config.insert("models".into(), Value::Object(models));
        config.insert("permissions".into(), json!({"preset": preset}));
        let mut tools = Map::new();
        if !allow.is_empty() {
            tools.insert("allow".into(), json!(allow));
        }
        if !exclude.is_empty() {
            tools.insert("exclude".into(), json!(exclude));
        }
        if !tools.is_empty() {
            config.insert("tools".into(), Value::Object(tools));
        }
        if let Some(rt) = c.bot.config.get("runtime") {
            config.insert("runtime".into(), rt.clone());
        }
        // File access is inherited, never widened: an unset `[files]` would
        // fall back to "this computer" for a bot of a folder-only caller.
        if let Some(f) = c.bot.config.get("files") {
            config.insert("files".into(), f.clone());
        }
        let config = Value::Object(config);
        let (name, config) = validate(&name, config)?;
        Ok(Resolved {
            action: "create",
            target: None,
            name,
            config,
            base: None,
        })
    }

    #[allow(clippy::too_many_lines)] // one field after the other, each with its own check
    fn resolve_update(&self, c: &Caller, id: &str, req: &BotRequest) -> Result<Resolved, String> {
        let target = self.managed_target(c, id)?;
        let old = target.config.clone();
        let mut config = old.clone();
        let mut name = target.name.clone();
        let tp: agent_core::BotProfile = serde_json::from_value(old.clone())
            .map_err(|e| format!("the bot has an invalid profile: {e}"))?;
        let mut touched = false;
        let root = config
            .as_object_mut()
            .ok_or("the bot's profile is not an object")?;
        if let Some(n) = &req.name {
            name = clean_text("name", n, MAX_NAME_CHARS, false)?;
            section(root, "bot").insert("name".into(), json!(name));
            touched = true;
        }
        if let Some(p) = &req.persona {
            let p = clean_text("persona", p, MAX_PERSONA_CHARS, true)?;
            section(root, "bot").insert("persona".into(), json!(p));
            touched = true;
        }
        if let Some(col) = &req.color {
            section(root, "bot").insert("color".into(), json!(clean_color(col)?));
            touched = true;
        }
        if let Some(shape) = &req.shape {
            section(root, "bot").insert("shape".into(), json!(clean_shape(shape)?));
            touched = true;
        }
        if let Some(a) = &req.avatar {
            section(root, "bot").insert("avatar".into(), json!(clean_avatar(a)?));
            touched = true;
        }
        for (k, v) in [
            ("executor", &req.executor),
            ("planner", &req.planner),
            ("classifier", &req.classifier),
        ] {
            let Some(m) = v else { continue };
            touched = true;
            if m.trim().is_empty() && k != "executor" {
                section(root, "models").remove(k);
                continue;
            }
            let m = clean_model(k, m)?;
            Self::model_ok(c, k, &m)?;
            section(root, "models").insert(k.into(), json!(m));
        }
        if let Some(p) = &req.permission_preset {
            let preset = Self::preset_for(
                c,
                Some(p),
                Some((&policy_of(&tp, &c.global), ceiling_mode(&tp.permissions))),
            )?;
            root.insert("permissions".into(), json!({"preset": preset}));
            touched = true;
        }
        if let Some(list) = &req.tools {
            let allow = clean_tools(list)?;
            let mut exclude = tp.tools.exclude.clone();
            for e in &c.profile.tools.exclude {
                if !exclude.contains(e) {
                    exclude.push(e.clone());
                }
            }
            let filter = agent_core::ToolFilter {
                allow: allow.clone(),
                exclude: exclude.clone(),
                ..agent_core::ToolFilter::default()
            };
            c.profile.covers_tool_filter(&filter)?;
            tp.covers_tool_filter(&filter)
                .map_err(|e| format!("an update can only narrow the tools: {e}"))?;
            let t = section(root, "tools");
            if allow.is_empty() {
                t.remove("allow");
            } else {
                t.insert("allow".into(), json!(allow));
            }
            if exclude.is_empty() {
                t.remove("exclude");
            } else {
                t.insert("exclude".into(), json!(exclude));
            }
            touched = true;
        }
        if !touched {
            return Err("nothing to change: give at least one field besides `id`".into());
        }
        let (name, config) = validate(&name, config)?;
        // Re-check the result against both ceilings (belt and braces).
        let after = Bot {
            config: config.clone(),
            ..target.clone()
        };
        Self::within_ceiling(c, &after, "the updated bot")?;
        let after_p: agent_core::BotProfile =
            serde_json::from_value(config.clone()).map_err(|e| e.to_string())?;
        policy_within(
            "an update can only lower permissions; the result",
            &policy_of(&after_p, &c.global),
            &policy_of(&tp, &c.global),
            &c.global,
        )?;
        tp.covers_tool_filter(&after_p.tools)
            .map_err(|e| format!("an update can only narrow the tools: {e}"))?;
        Ok(Resolved {
            action: "update",
            base: Some(sha256_hex(&old.to_string())),
            target: Some(target),
            name,
            config,
        })
    }

    fn key(who: &BotCaller, change: &BotChange) -> String {
        sha256_hex(&format!(
            "{}|{}",
            who.bot_id,
            serde_json::to_string(change).unwrap_or_default()
        ))
    }

    fn view(bot: &Bot, caller: &str) -> BotView {
        let mine = created_by(&bot.config) == Some(caller) && bot.id.to_string() != caller;
        BotView {
            id: bot.id.to_string(),
            name: bot.name.clone(),
            executor: bot
                .config
                .pointer("/models/executor")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            permission_preset: preset_name(&bot.config),
            tools: tool_list(&bot.config),
            color: bot
                .config
                .pointer("/bot/color")
                .and_then(Value::as_str)
                .map(str::to_owned),
            origin: if created_by(&bot.config).is_some() {
                "bot"
            } else {
                "user"
            }
            .into(),
            editable: mine,
        }
    }

    /// Check the change is still exactly what was previewed; consume the preview.
    fn take_frozen(&self, key: &str, r: &Resolved) -> Result<(), String> {
        let now = (self.clock)();
        let mut f = self.frozen.lock();
        f.retain(|e| now - e.made_ms <= FROZEN_TTL_MS);
        let pos = f
            .iter()
            .position(|e| e.key == key)
            .ok_or("this change was not shown to the user for approval (or the approval expired); call the tool again")?;
        let e = &f[pos];
        if e.name != r.name || e.config != r.config || e.base != r.base {
            f.remove(pos);
            return Err("the bot or your own permissions changed after the user was asked, so this is no longer what was approved; call the tool again".into());
        }
        f.remove(pos);
        Ok(())
    }

    fn audit(&self, who: &BotCaller, action: &str, r: &Resolved) -> Result<(), String> {
        let run = Uuid::parse_str(&who.run_id).ok().map(RunId);
        let persona = r
            .config
            .pointer("/bot/persona")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let detail = json!({
            "caller_bot_id": who.bot_id,
            "run_id": who.run_id,
            "bot_id": r.target.as_ref().map(|b| b.id.to_string()),
            "name": r.name.chars().take(MAX_NAME_CHARS).collect::<String>(),
            "permission_preset": preset_name(&r.config),
            "executor": r.config.pointer("/models/executor"),
            "tools": tool_list(&r.config),
            "persona_chars": persona.chars().count(),
            "persona_sha256": sha256_hex(persona),
            "profile_sha256": sha256_hex(&r.config.to_string()),
        });
        self.storage
            .append_audit(self.user_id, run, "bot", action, &detail)
            .map(|_| ())
            .map_err(|e| format!("the audit log could not be written, so nothing was changed: {e}"))
    }
}

fn section<'a>(root: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let v = root
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    match v {
        Value::Object(m) => m,
        other => {
            *other = Value::Object(Map::new());
            match other {
                Value::Object(m) => m,
                _ => unreachable!("just assigned an object"),
            }
        }
    }
}

/// Run the same validation as `POST /v1/bots`.
fn validate(name: &str, config: Value) -> Result<(String, Value), String> {
    crate::routes::resolve_bot(BotInput {
        expected_version: None,
        name: Some(name.to_owned()),
        config: Some(config),
        profile_toml: None,
        runtime: None,
    })
    .map_err(|e| e.message().to_owned())
}

#[async_trait]
impl BotHost for DaemonBotHost {
    fn may_manage_bots(&self, bot_id: &str) -> bool {
        self.bot_by_id(bot_id)
            .ok()
            .flatten()
            .is_some_and(|b| Self::manages(&b.config))
    }

    /// The switch, or the Full level. This is what the tool list shows; it says nothing about
    /// the run: `caller` decides for every call (list, preview, create, update, delete).
    fn may_see_bot_tools(&self, bot_id: &str) -> bool {
        self.bot_by_id(bot_id)
            .ok()
            .flatten()
            .is_some_and(|b| Self::manages(&b.config) || Self::full_level(&b.config))
    }

    async fn list(&self, who: &BotCaller) -> Result<Vec<BotView>, String> {
        self.caller(who)?;
        Ok(self
            .storage
            .list_bots(self.user_id)
            .map_err(|e| e.to_string())?
            .iter()
            .map(|b| Self::view(b, &who.bot_id))
            .collect())
    }

    async fn preview(&self, who: &BotCaller, change: &BotChange) -> Result<BotPreview, String> {
        let r = self.resolve(who, change)?;
        let nonce = BotId::new().to_string();
        let key = Self::key(who, change);
        {
            let mut f = self.frozen.lock();
            let now = (self.clock)();
            f.retain(|e| e.key != key && now - e.made_ms <= FROZEN_TTL_MS);
            while f.len() >= FROZEN_MAX {
                f.remove(0);
            }
            f.push(Frozen {
                key,
                nonce: nonce.clone(),
                made_ms: now,
                name: r.name.clone(),
                config: r.config.clone(),
                base: r.base.clone(),
            });
        }
        let old = match (&r.target, r.action) {
            (Some(t), "update") => Some(t.config.clone()),
            _ => None,
        };
        Ok(BotPreview {
            action: r.action.into(),
            bot_id: r.target.as_ref().map(|b| b.id.to_string()),
            name: r.name.clone(),
            profile: redacted(&r.config),
            changes: old.map(|o| changes(&o, &r.config)).unwrap_or_default(),
            avatar: r
                .config
                .pointer("/bot/avatar")
                .and_then(Value::as_str)
                .map(str::to_owned),
            approval_ref: nonce,
        })
    }

    fn discard(&self, who: &BotCaller, change: &BotChange, approval_ref: &str) {
        let key = Self::key(who, change);
        self.frozen
            .lock()
            .retain(|e| !(e.key == key && e.nonce == approval_ref));
    }

    async fn create(&self, who: &BotCaller, req: &BotRequest) -> Result<BotView, String> {
        let change = BotChange::Create(req.clone());
        let key = Self::key(who, &change);
        let (bot, app) = {
            let _gate = self.gate.lock();
            let r = self.resolve(who, &change)?;
            self.take_frozen(&key, &r)?;
            self.audit(who, "bot.create", &r)?;
            let bot = Bot {
                id: BotId::new(),
                user_id: self.user_id,
                name: r.name,
                config: r.config,
                created_at_ms: now_ms(),
            };
            self.storage.insert_bot(&bot).map_err(|e| e.to_string())?;
            (bot, self.app()?)
        };
        app.refresh_profile(&bot);
        Ok(Self::view(&bot, &who.bot_id))
    }

    async fn update(&self, who: &BotCaller, id: &str, req: &BotRequest) -> Result<BotView, String> {
        let change = BotChange::Update {
            id: id.to_owned(),
            req: req.clone(),
        };
        let key = Self::key(who, &change);
        let (bot, app) = {
            let _gate = self.gate.lock();
            // The profile revision the change was resolved against: a user edit
            // in between makes the write stale instead of overwriting it.
            let version = Uuid::parse_str(id)
                .ok()
                .and_then(|u| self.storage.get_bot_versioned(BotId(u)).ok().flatten())
                .map(|(_, v)| v)
                .ok_or_else(|| format!("there is no bot with id {id:?}"))?;
            let r = self.resolve(who, &change)?;
            self.take_frozen(&key, &r)?;
            self.audit(who, "bot.update", &r)?;
            let mut bot = r.target.ok_or("no target")?;
            match self
                .storage
                .update_bot(bot.id, &r.name, &r.config, version)
                .map_err(|e| e.to_string())?
            {
                storage::BotPatch::Applied { .. } => {}
                _ => {
                    return Err(
                        "the bot was changed by someone else while this was prepared; ask again"
                            .to_owned(),
                    );
                }
            }
            bot.name = r.name;
            bot.config = r.config;
            (bot, self.app()?)
        };
        app.refresh_profile(&bot);
        Ok(Self::view(&bot, &who.bot_id))
    }

    async fn delete(&self, who: &BotCaller, id: &str) -> Result<BotView, String> {
        let change = BotChange::Delete { id: id.to_owned() };
        let key = Self::key(who, &change);
        let (bot, app) = {
            let _gate = self.gate.lock();
            let r = self.resolve(who, &change)?;
            self.take_frozen(&key, &r)?;
            self.audit(who, "bot.delete", &r)?;
            (r.target.ok_or("no target")?, self.app()?)
        };
        // The same removal as the HTTP route (knowledge, browser slot, MCP and
        // e-mail links); it runs outside the gate because it awaits.
        if crate::routes::remove_bot(&app, &bot).await.is_err() {
            // The audit entry above said "delete"; say it did not happen.
            let _ = self.storage.append_audit(
                self.user_id,
                Uuid::parse_str(&who.run_id).ok().map(RunId),
                "bot",
                "bot.delete_failed",
                &json!({"caller_bot_id": who.bot_id, "bot_id": bot.id.to_string()}),
            );
            return Err("this bot still has chats or runs, so it cannot be deleted (HTTP 409); only the user can clear them in the app".to_owned());
        }
        Ok(Self::view(&bot, &who.bot_id))
    }
}
