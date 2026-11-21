//! Runless schedule reads through the registered `ToolGate`. No interactive Ask,
//! grants or `read_tools_skip_ask` shortcuts exist on this boundary.
use std::sync::{Arc, Weak};

use crate::{AgentLoop, shared::Shared};
use async_trait::async_trait;
use builtin_tools::{GateHost, GateRequest, ToolGate};
use permission_engine::{ActionRequest, Decision};
use protocol::{BotId, InitiatorKind, TaskId};
use script_runtime::ScriptHost;
use serde_json::{Value, json};
use tool_registry::Effect;

fn check(sh: &Shared, bot_id: BotId, name: &str, input: &Value) -> Result<(), String> {
    if sh.cfg.safe_mode {
        return Err("schedule read tool unavailable".into());
    }
    let tool = sh.tools.get(name).ok_or("schedule read tool unavailable")?;
    let spec = tool.spec();
    let profile = sh
        .profile_for(bot_id)
        .map_err(|_| "schedule profile unavailable")?;
    if !tool.visible_to(Some(&bot_id.to_string()))
        || spec.effect != Effect::Read
        || tool.always_ask()
        || !profile.tool_visible(name)
        || !profile.category_visible(spec.category)
        || sh.locked_deny(spec.category)
    {
        return Err("schedule reads require a visible, noninteractive Read tool".into());
    }
    let req = ActionRequest {
        cwd: None,
        category: spec.category,
        raw_command: builtin_tools::raw_command_for(name, input),
        initiator: InitiatorKind::Schedule,
    };
    if sh.resolver.resolve(
        &sh.global_policy.read(),
        profile.policy().as_ref(),
        None,
        &req,
    ) != Decision::Allow
    {
        return Err("schedule reads require explicit Allow; Ask and Deny are unavailable".into());
    }
    Ok(())
}

struct ScheduleReadGate {
    sh: Weak<Shared>,
    bot_id: BotId,
    schedule_id: TaskId,
}
#[async_trait]
impl ToolGate for ScheduleReadGate {
    async fn authorize(&self, req: &GateRequest) -> Result<(), String> {
        let sh = self.sh.upgrade().ok_or("schedule host unavailable")?;
        if req.effect != Effect::Read
            || !sh
                .tools
                .get(&req.tool)
                .is_some_and(|t| t.spec().category == req.category && t.spec().effect == req.effect)
        {
            return Err("schedule tool changed or is not Read".into());
        }
        check(&sh, self.bot_id, &req.tool, &req.input)?;
        let bot = sh
            .storage
            .get_bot(self.bot_id)
            .map_err(|_| "schedule owner unavailable")?
            .ok_or("schedule owner unavailable")?;
        // The audit contains only locally generated identifiers, never tool
        // input, script source, account/filter/path or upstream error strings.
        sh.storage.append_audit(bot.user_id, None, "schedule", "schedule.read_intent", &json!({"schedule_id": self.schedule_id.to_string(), "bot_id": self.bot_id.to_string()}))
            .map_err(|_| "schedule read audit failed")?;
        Ok(())
    }
}

// Redact leaves, preserving the JSON structure needed by header consumers.
fn clean(sh: &Shared, value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(sh.redact(s)),
        Value::Array(a) => Value::Array(a.into_iter().map(|v| clean(sh, v)).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (sh.redact(k), clean(sh, v)))
                .collect(),
        ),
        other => other,
    }
}

struct RedactedHost {
    sh: Weak<Shared>,
    host: GateHost,
}
#[async_trait]
impl ScriptHost for RedactedHost {
    async fn call_tool(&self, name: &str, input: Value) -> Result<Value, String> {
        let sh = self.sh.upgrade().ok_or("schedule host unavailable")?;
        let result = self
            .host
            .call_tool(name, input)
            .await
            .map_err(|_| "schedule read refused or failed")?;
        sh.refresh_redactor();
        Ok(clean(&sh, result))
    }
}

impl AgentLoop {
    /// Validate a schedule's unattended Read permission without executing it.
    ///
    /// # Errors
    /// Hidden, non-Read, always-ask, Ask, Deny or unavailable tool/profile.
    pub fn validate_schedule_read(
        &self,
        bot_id: BotId,
        name: &str,
        input: &Value,
    ) -> Result<(), String> {
        check(&self.sh, bot_id, name, input)
    }

    /// Noninteractive gate, also used by the metadata watcher before any read.
    #[must_use]
    pub fn schedule_read_gate(&self, bot_id: BotId, schedule_id: TaskId) -> Arc<dyn ToolGate> {
        Arc::new(ScheduleReadGate {
            sh: Arc::downgrade(&self.sh),
            bot_id,
            schedule_id,
        })
    }

    /// Script host backed by the real registry and the runless Read gate.
    /// No Run row or model call is created. Production masking precedes results.
    #[must_use]
    pub fn schedule_read_host(&self, bot_id: BotId, schedule_id: TaskId) -> Arc<dyn ScriptHost> {
        Arc::new(RedactedHost {
            sh: Arc::downgrade(&self.sh),
            host: GateHost::new(
                self.sh.tools.clone(),
                self.schedule_read_gate(bot_id, schedule_id),
                format!("precheck-{schedule_id}"),
                65_536,
                Some(bot_id.to_string()),
            ),
        })
    }
}
