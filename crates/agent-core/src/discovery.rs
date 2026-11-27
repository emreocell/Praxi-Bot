//! Per-run deferred tool definitions shared by the HTTP provider adapters.

use agent_router::{
    TOOL_DESCRIBE, discovery_name, discovery_schema, discovery_summary, provider_tool_name,
};
use storage::RunStep;
use task_runtime::StepStatus;
use tool_registry::ToolSpec;

use crate::config::DEFAULT_DIRECT_TOOL_LIMIT;
use crate::history::status_of;
use crate::shared::{RunCtx, STEP_TOOLS, Shared};

const CORE: &[&str] = &[
    "ask_user",
    "todo_write",
    "fs_list",
    "fs_read",
    // Writing a file and running a command are the two most common jobs. They sit in the
    // direct set (shown only when the bot may use them) so a light model never has to
    // find them through `tool_search` first: a search for "write file" does not reliably
    // rank them, and a bot that cannot find its own tool tells the owner it has none.
    "fs_write",
    "fs_edit",
    "shell_run",
    "web_search",
    "web_extract",
    "desktop_list_apps",
    "browser_open",
];

const DIRECT_EXTRAS: &[&str] = &[
    "fs_search",
    "desktop_get_state",
    "browser_snapshot",
    "memory_search",
    "notes_read",
    "knowledge_search",
];

const MAX_ACTIVATIONS: usize = DEFAULT_DIRECT_TOOL_LIMIT - CORE.len() - 2;

pub(crate) fn bound_legacy(specs: &mut Vec<ToolSpec>) {
    while specs.len() > DEFAULT_DIRECT_TOOL_LIMIT {
        let last_direct = specs.iter().rposition(|s| discovery_name(&s.name));
        if let Some(index) = last_direct {
            specs.remove(index);
        } else {
            specs.truncate(DEFAULT_DIRECT_TOOL_LIMIT);
        }
    }
}

pub(crate) fn activations(steps: &[RunStep]) -> Vec<&str> {
    let mut names = Vec::new();
    for step in steps {
        if step.name != STEP_TOOLS || status_of(step) != Some(StepStatus::Succeeded) {
            continue;
        }
        for row in step
            .output
            .as_ref()
            .and_then(|out| out["results"].as_array())
            .into_iter()
            .flatten()
        {
            if row["name"] == TOOL_DESCRIBE
                && row["is_error"] == false
                && let Some(name) = row["activated_tool"].as_str()
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    names
}

impl Shared {
    pub(crate) fn deferred_specs(
        &self,
        ctx: &RunCtx,
        active: &[&str],
        direct_only: bool,
    ) -> Vec<ToolSpec> {
        if self.cfg.safe_mode {
            return Vec::new();
        }
        let mission: &[&str] = if ctx.mission.is_some() {
            &missions::tools::MISSION_TOOL_NAMES
        } else if self.mission_create_offered(ctx) {
            &[crate::mission_start::MISSION_CREATE]
        } else {
            &[]
        };
        let mut specs: Vec<_> = CORE
            .iter()
            .chain(DIRECT_EXTRAS.iter().filter(|_| direct_only))
            .chain(mission)
            .filter_map(|name| self.visible_definition(ctx, name))
            .collect();
        if !direct_only {
            specs.extend(
                self.surface
                    .deferred_tools()
                    .iter()
                    .map(|t| t.spec().clone()),
            );
            specs.extend(
                active
                    .iter()
                    .take(MAX_ACTIVATIONS)
                    .filter_map(|name| self.visible_definition(ctx, name)),
            );
        }
        let mut aliases = std::collections::HashSet::new();
        specs.retain(|s| aliases.insert(provider_tool_name(&s.name)));
        specs
    }

    fn visible_definition(&self, ctx: &RunCtx, name: &str) -> Option<ToolSpec> {
        let tool = self.tools.get(name)?;
        let mut spec = tool.spec().clone();
        if !discovery_name(name) || !self.spec_visible(&ctx.profile, ctx.bot_id, &spec) {
            return None;
        }
        if spec.content.has_external_metadata() {
            spec.description = discovery_summary(&spec.description);
            spec.input_schema = discovery_schema(&spec.input_schema);
        }
        Some(spec)
    }

    pub(crate) fn check_activation(
        &self,
        ctx: &RunCtx,
        name: &str,
        active: &[&str],
    ) -> Result<bool, &'static str> {
        let spec = self
            .visible_definition(ctx, name)
            .ok_or("unknown tool; search with tool_search")?;
        if CORE.contains(&name)
            || active.contains(&name)
            || (ctx.mission.is_some() && missions::tools::is_mission_tool(name))
            || (name == crate::mission_start::MISSION_CREATE && self.mission_create_offered(ctx))
        {
            return Ok(false);
        }
        let alias = provider_tool_name(&spec.name);
        if self
            .deferred_specs(ctx, active, false)
            .iter()
            .any(|s| provider_tool_name(&s.name) == alias)
        {
            return Err("tool name conflicts with an active definition");
        }
        if active.len() >= MAX_ACTIVATIONS {
            return Err("the tool activation limit for this run has been reached");
        }
        Ok(true)
    }
}
