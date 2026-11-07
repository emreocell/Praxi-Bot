//! K3: fast classifier model. One compact prompt, strict JSON out.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::semantic::{Candidate, CandidateKind};
use super::{BackendError, Route};

/// A small, fast text model (the bot profile's `classifier` role).
#[async_trait]
pub trait Classifier: Send + Sync {
    /// Complete `prompt` with at most `max_tokens` output tokens.
    async fn classify(&self, prompt: &str, max_tokens: u32) -> Result<String, BackendError>;
}

/// Longest slice of the user text sent to the classifier.
const MAX_TEXT_CHARS: usize = 400;

/// Build the compact prompt listing `candidates`.
#[must_use]
pub fn build_prompt(text: &str, candidates: &[&Candidate]) -> String {
    let mut p = String::from(
        "Route the user request to one candidate, or to \"agent\" if none clearly fits \
         or it needs planning.\nCandidates (kind name - description):\n",
    );
    for c in candidates {
        let kind = match c.kind {
            CandidateKind::Workflow => "workflow",
            CandidateKind::Skill => "skill",
            CandidateKind::Tool => "tool",
        };
        let _ = write!(p, "{kind} {} - {}", c.name, c.description);
        if !c.params.is_empty() {
            let params: Vec<String> = c
                .params
                .iter()
                .map(|x| {
                    if x.required {
                        format!("{}*", x.name)
                    } else {
                        x.name.clone()
                    }
                })
                .collect();
            let _ = write!(p, " [params: {}]", params.join(", "));
        }
        p.push('\n');
    }
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT_CHARS)
        .collect();
    let _ = write!(
        p,
        "Answer with JSON only: {{\"route\":\"workflow|skill|tool|agent\",\"name\":\"<candidate name or null>\",\
         \"params\":{{\"<param>\":\"<value>\"}},\"confidence\":<0..1>}}\n\
         Params (only for workflows, * = required) must be extracted from the request.\n\
         Request: {flat}\nJSON:"
    );
    p
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnswer {
    route: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    params: Option<BTreeMap<String, Value>>,
    confidence: f64,
}

/// A validated classifier verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// A concrete route (workflow / tool hint / skill hint).
    Routed(Route, f64),
    /// The classifier chose `agent` (or confidence was too low to commit).
    Agent(f64, String),
}

/// Validation thresholds.
#[derive(Debug, Clone, Copy)]
pub struct VerdictLimits {
    /// Minimum confidence to route a workflow.
    pub workflow_threshold: f64,
    /// Minimum confidence for tool / skill hints.
    pub hint_threshold: f64,
}

fn strip_fences(raw: &str) -> &str {
    let t = raw.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim()
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Strictly parse and validate a classifier answer against `candidates`.
///
/// # Errors
/// A human-readable reason when the answer is malformed or names things that
/// were not offered; the caller falls back to `Agent`.
pub fn interpret(
    raw: &str,
    candidates: &[&Candidate],
    limits: VerdictLimits,
) -> Result<Verdict, String> {
    let ans: RawAnswer =
        serde_json::from_str(strip_fences(raw)).map_err(|e| format!("malformed JSON: {e}"))?;
    if !ans.confidence.is_finite() || !(0.0..=1.0).contains(&ans.confidence) {
        return Err("confidence outside 0..1".to_owned());
    }
    let kind = match ans.route.as_str() {
        "agent" => {
            return Ok(Verdict::Agent(
                ans.confidence,
                "classifier chose agent".into(),
            ));
        }
        "workflow" => CandidateKind::Workflow,
        "skill" => CandidateKind::Skill,
        "tool" => CandidateKind::Tool,
        other => return Err(format!("unknown route {other:?}")),
    };
    let name = ans
        .name
        .filter(|n| !n.is_empty())
        .ok_or_else(|| "missing name".to_owned())?;
    let cand = candidates
        .iter()
        .find(|c| c.kind == kind && c.name == name)
        .ok_or_else(|| format!("{name:?} was not offered"))?;
    match kind {
        CandidateKind::Workflow => {
            if ans.confidence < limits.workflow_threshold {
                return Ok(Verdict::Agent(
                    ans.confidence,
                    format!(
                        "workflow confidence {:.2} below {:.2}",
                        ans.confidence, limits.workflow_threshold
                    ),
                ));
            }
            let mut params = BTreeMap::new();
            for (k, v) in ans.params.unwrap_or_default() {
                if !cand.params.iter().any(|p| p.name == k) {
                    return Err(format!("undeclared param {k:?}"));
                }
                let v = scalar(&v).ok_or_else(|| format!("param {k:?} is not a scalar"))?;
                params.insert(k, v);
            }
            for req in cand.required_params() {
                if params.get(req).is_none_or(String::is_empty) {
                    return Ok(Verdict::Agent(
                        ans.confidence,
                        format!("missing required param {req:?}"),
                    ));
                }
            }
            Ok(Verdict::Routed(
                Route::Workflow {
                    name,
                    params,
                    confidence: ans.confidence,
                },
                ans.confidence,
            ))
        }
        CandidateKind::Tool | CandidateKind::Skill => {
            if ans.confidence < limits.hint_threshold {
                return Ok(Verdict::Agent(
                    ans.confidence,
                    format!("hint confidence {:.2} too low", ans.confidence),
                ));
            }
            let route = if kind == CandidateKind::Tool {
                Route::ToolHint { tools: vec![name] }
            } else {
                Route::SkillHint { skills: vec![name] }
            };
            Ok(Verdict::Routed(route, ans.confidence))
        }
    }
}
