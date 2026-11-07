//! Per-run metrics and the JSONL result file.
//!
//! One JSON object per line, schema version [`SCHEMA_VERSION`]:
//!
//! ```json
//! {"schema":1,"ts_ms":0,"tier":1,"name":"repeat-detector","model":"fake/m",
//!  "success":true,"failures":[],"state":"failed","error":null,"wall_ms":12,
//!  "llm_calls":5,"input_tokens":50,"output_tokens":25,"cached_tokens":0,
//!  "cost_usd":null,"tool_calls":{"t_read":4},
//!  "perception":{"tree":0,"ocr":0,"grid":0}}
//! ```

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;
use crate::harness::TurnTrace;

/// Version of the JSONL record layout.
pub const SCHEMA_VERSION: u32 = 1;

/// How often each rung of the perception ladder was used in a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Perception {
    /// Accessibility tree / DOM snapshot reads (`desktop_get_state`, `browser_snapshot`).
    pub tree: u32,
    /// OCR set-of-marks sections returned to the model.
    pub ocr: u32,
    /// Grid / zoom perception (screenshots with grid cells).
    pub grid: u32,
}

/// One run's metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Layout version.
    pub schema: u32,
    /// Unix epoch milliseconds when the run started.
    pub ts_ms: i64,
    /// 1 (hermetic) or 2 (live).
    pub tier: u8,
    /// Scenario / task name.
    pub name: String,
    /// `provider/model` used.
    pub model: String,
    /// Whether every assertion / check passed.
    pub success: bool,
    /// Human-readable failed assertions.
    #[serde(default)]
    pub failures: Vec<String>,
    /// Final run state (`succeeded`, `failed`, ...).
    pub state: String,
    /// Run error text, if the run failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Wall time of the turn in milliseconds.
    pub wall_ms: u64,
    /// Number of model calls.
    pub llm_calls: u32,
    /// Prompt tokens, summed.
    pub input_tokens: u64,
    /// Completion tokens, summed.
    pub output_tokens: u64,
    /// Cached prompt tokens, summed.
    pub cached_tokens: u64,
    /// Cost in USD from the configured price table; `None` when the model has no price.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Tool calls requested, by tool name.
    #[serde(default)]
    pub tool_calls: BTreeMap<String, u32>,
    /// Perception ladder usage.
    #[serde(default)]
    pub perception: Perception,
}

impl RunRecord {
    /// Build a record from a finished turn.
    #[must_use]
    pub fn from_trace(
        tier: u8,
        name: &str,
        model: &str,
        trace: &TurnTrace,
        failures: Vec<String>,
    ) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            ts_ms: trace.started_ms,
            tier,
            name: name.to_owned(),
            model: model.to_owned(),
            success: failures.is_empty(),
            failures,
            state: trace.state.as_str().to_owned(),
            error: trace.error.clone(),
            wall_ms: u64::try_from(trace.wall.as_millis()).unwrap_or(u64::MAX),
            llm_calls: trace.llm_calls,
            input_tokens: trace.input_tokens,
            output_tokens: trace.output_tokens,
            cached_tokens: trace.cached_tokens,
            cost_usd: trace.cost_usd,
            tool_calls: trace.tool_counts.clone(),
            perception: trace.perception,
        }
    }

    /// A record for a run that could not even start (setup failure).
    #[must_use]
    pub fn setup_failure(tier: u8, name: &str, model: &str, why: String) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            ts_ms: storage::now_ms(),
            tier,
            name: name.to_owned(),
            model: model.to_owned(),
            success: false,
            failures: vec![why.clone()],
            state: "not_started".into(),
            error: Some(why),
            wall_ms: 0,
            llm_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cost_usd: None,
            tool_calls: BTreeMap::new(),
            perception: Perception::default(),
        }
    }
}

/// Classify tool results into perception-ladder usage.
///
/// `calls` are the requested tool names, `results` the text of tool results.
#[must_use]
pub fn perception_usage<'a>(
    calls: impl IntoIterator<Item = &'a str>,
    results: impl IntoIterator<Item = &'a str>,
) -> Perception {
    let mut p = Perception::default();
    for c in calls {
        match c {
            "desktop_get_state" | "browser_snapshot" => p.tree += 1,
            c if c.contains("grid") || c.contains("zoom") => p.grid += 1,
            _ => {}
        }
    }
    for r in results {
        if r.contains("OCR marks (use index") {
            p.ocr += 1;
        }
        if r.contains("grid cell") {
            p.grid += 1;
        }
    }
    p
}

/// Sum the usage of a run from the loop's `usage` audit entries.
#[must_use]
pub fn sum_usage<'a>(entries: impl IntoIterator<Item = &'a Value>) -> (u64, u64, u64, Option<f64>) {
    let (mut i, mut o, mut c) = (0u64, 0u64, 0u64);
    let mut cost: Option<f64> = None;
    for d in entries {
        i += d["prompt_tokens"].as_u64().unwrap_or(0);
        o += d["completion_tokens"].as_u64().unwrap_or(0);
        c += d["cached_tokens"].as_u64().unwrap_or(0);
        if let Some(x) = d["cost_usd"].as_f64() {
            *cost.get_or_insert(0.0) += x;
        }
    }
    (i, o, c, cost)
}

/// Append records to a JSONL file (created with its parent directories).
///
/// # Errors
/// I/O or serialisation errors.
pub fn append_jsonl(path: &Path, records: &[RunRecord]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for r in records {
        writeln!(f, "{}", serde_json::to_string(r)?)?;
    }
    Ok(())
}

/// Read a JSONL result file (blank lines are skipped).
///
/// # Errors
/// I/O errors or a malformed line (the line number is included).
pub fn read_jsonl(path: &Path) -> Result<Vec<RunRecord>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perception_counts_tree_ocr_grid() {
        let p = perception_usage(
            ["desktop_get_state", "browser_snapshot", "desktop_click"],
            ["fine", "x\nOCR marks (use index \"mark:<n>\"):\nmark:1"],
        );
        assert_eq!(
            p,
            Perception {
                tree: 2,
                ocr: 1,
                grid: 0
            }
        );
    }

    #[test]
    fn usage_sums_and_cost_is_none_without_prices() {
        let a = serde_json::json!({"prompt_tokens": 10, "completion_tokens": 5, "cached_tokens": 2, "cost_usd": null});
        let b = serde_json::json!({"prompt_tokens": 1, "completion_tokens": 1, "cached_tokens": null, "cost_usd": null});
        assert_eq!(sum_usage([&a, &b]), (11, 6, 2, None));
        let c = serde_json::json!({"prompt_tokens": 1, "completion_tokens": 1, "cost_usd": 0.5});
        assert_eq!(sum_usage([&c, &c]).3, Some(1.0));
    }

    #[test]
    fn jsonl_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("r.jsonl");
        let r = RunRecord::setup_failure(1, "x", "fake/m", "boom".into());
        append_jsonl(&path, &[r.clone(), r.clone()]).unwrap();
        assert_eq!(read_jsonl(&path).unwrap(), vec![r.clone(), r]);
    }
}
