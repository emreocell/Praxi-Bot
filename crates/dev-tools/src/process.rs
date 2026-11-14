//! `process_list`: read-only listing of running processes (via `sysinfo`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, ProcessRefreshKind, ProcessesToUpdate, System};
use tool_registry::{
    ContentClass, ContentPart, Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec,
};

/// Tool name.
pub const PROCESS_LIST_TOOL_NAME: &str = "process_list";

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SortBy {
    Cpu,
    Memory,
    Pid,
    Name,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    name_contains: Option<String>,
    sort_by: Option<SortBy>,
    limit: Option<usize>,
}

struct ProcessList {
    spec: ToolSpec,
}

/// Build the `process_list` tool.
pub(crate) fn tool() -> Arc<dyn Tool> {
    Arc::new(ProcessList {
        spec: ToolSpec {
            name: PROCESS_LIST_TOOL_NAME.to_string(),
            description: "List running processes (pid, name, cpu_percent, memory_bytes, parent_pid, status), read-only. Example: {\"name_contains\":\"chrome\",\"sort_by\":\"memory\",\"limit\":10}".to_string(),
            input_schema: json!({"type":"object","additionalProperties":false,"properties":{
                "name_contains":{"type":"string","description":"Case-insensitive substring filter on the process name."},
                "sort_by":{"type":"string","enum":["cpu","memory","pid","name"],"description":"Default: memory (descending); cpu descending; pid and name ascending."},
                "limit":{"type":"integer","minimum":1,"maximum":MAX_LIMIT,"description":"Default 50."}
            }}),
            effect: Effect::Read,
            category: PermissionCategory::System,
            platforms: vec![Platform::All],
            // Process names and command lines are private machine state.
            content: ContentClass::private_read(),
        },
    })
}

fn snapshot(i: &Input) -> Value {
    let mut sys = System::new();
    let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    // CPU usage is a delta between two refreshes.
    std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);

    let needle = i.name_contains.as_ref().map(|n| n.to_lowercase());
    let mut rows: Vec<(u32, String, f32, u64, Option<u32>, String)> = sys
        .processes()
        .values()
        .filter_map(|p| {
            let name = p.name().to_string_lossy().into_owned();
            if let Some(n) = &needle
                && !name.to_lowercase().contains(n)
            {
                return None;
            }
            Some((
                p.pid().as_u32(),
                name,
                p.cpu_usage(),
                p.memory(),
                p.parent().map(sysinfo::Pid::as_u32),
                p.status().to_string(),
            ))
        })
        .collect();
    match i.sort_by.unwrap_or(SortBy::Memory) {
        SortBy::Cpu => rows.sort_by(|a, b| b.2.total_cmp(&a.2)),
        SortBy::Memory => rows.sort_by_key(|r| std::cmp::Reverse(r.3)),
        SortBy::Pid => rows.sort_by_key(|r| r.0),
        SortBy::Name => rows.sort_by_key(|r| r.1.to_lowercase()),
    }
    let total = rows.len();
    let limit = i.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let processes: Vec<Value> = rows
        .into_iter()
        .take(limit)
        .map(|(pid, name, cpu, mem, parent, status)| {
            json!({"pid":pid,"name":name,"cpu_percent":cpu,"memory_bytes":mem,
                   "parent_pid":parent,"status":status})
        })
        .collect();
    json!({"total_matching": total, "returned": processes.len(), "processes": processes})
}

#[async_trait]
impl Tool for ProcessList {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let i: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let data = tokio::task::spawn_blocking(move || snapshot(&i))
            .await
            .map_err(|e| ToolError::Execution(format!("process listing failed: {e}")))?;
        Ok(ToolOutput {
            content: vec![ContentPart::Json { data }],
            spill: None,
        })
    }
}
