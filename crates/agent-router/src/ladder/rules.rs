//! K0: slash commands and user-defined regex rules.

use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::{Command, Route, RouterError};

/// What a user rule routes to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum RuleAction {
    /// Run a command.
    Command {
        /// Command name.
        name: String,
        /// Fixed arguments.
        #[serde(default)]
        args: Vec<String>,
    },
    /// Replay a workflow. Named regex captures are merged into `params`.
    Workflow {
        /// Workflow name.
        name: String,
        /// Fixed parameters.
        #[serde(default)]
        params: BTreeMap<String, String>,
    },
    /// Narrow K4's tool surface.
    ToolHint {
        /// Tool names.
        tools: Vec<String>,
    },
    /// Suggest skills.
    SkillHint {
        /// Skill names.
        skills: Vec<String>,
    },
}

/// One user rule as it appears in the bot profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserRule {
    /// Regex, matched against the trimmed raw text and then against the
    /// normalised (folded, lowercase) text.
    pub pattern: String,
    /// The route to take on a match.
    #[serde(flatten)]
    pub action: RuleAction,
}

/// Compiled user rules, tried in order.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    rules: Vec<(Regex, RuleAction)>,
}

impl RuleSet {
    /// Compile `rules`; an invalid regex is an error.
    ///
    /// # Errors
    /// [`RouterError::InvalidRule`] if a pattern does not compile.
    pub fn new(rules: &[UserRule]) -> Result<Self, RouterError> {
        let rules = rules
            .iter()
            .map(|r| {
                Regex::new(&r.pattern)
                    .map(|re| (re, r.action.clone()))
                    .map_err(|e| RouterError::InvalidRule(format!("{}: {e}", r.pattern)))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    /// First matching rule, with a human-readable reason.
    #[must_use]
    pub fn matching(&self, raw: &str, normalized: &str) -> Option<(Route, String)> {
        let raw = raw.trim();
        for (re, action) in &self.rules {
            let caps = re.captures(raw).or_else(|| re.captures(normalized));
            let Some(caps) = caps else { continue };
            let route = match action {
                RuleAction::Command { name, args } => Route::Command(Command {
                    name: name.clone(),
                    args: args.clone(),
                }),
                RuleAction::Workflow { name, params } => {
                    let mut params = params.clone();
                    for cap_name in re.capture_names().flatten() {
                        if let Some(m) = caps.name(cap_name) {
                            params.insert(cap_name.to_owned(), m.as_str().to_owned());
                        }
                    }
                    Route::Workflow {
                        name: name.clone(),
                        params,
                        confidence: 1.0,
                    }
                }
                RuleAction::ToolHint { tools } => Route::ToolHint {
                    tools: tools.clone(),
                },
                RuleAction::SkillHint { skills } => Route::SkillHint {
                    skills: skills.clone(),
                },
            };
            return Some((route, format!("user rule /{}/", re.as_str())));
        }
        None
    }
}

/// Split on whitespace, keeping `"double quoted"` runs together.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Built-in slash commands: `/stop`, `/new`, `/workflow <name> k=v ...`.
/// Anything else (including malformed `/workflow`) is not a command.
#[must_use]
pub fn slash_command(raw: &str) -> Option<Route> {
    let rest = raw.trim().strip_prefix('/')?;
    let mut parts = split_args(rest).into_iter();
    let head = parts.next()?.to_ascii_lowercase();
    let args: Vec<String> = parts.collect();
    match head.as_str() {
        "stop" | "new" => Some(Route::Command(Command { name: head, args })),
        "workflow" => {
            let (name, kvs) = args.split_first()?;
            let mut params = BTreeMap::new();
            for kv in kvs {
                let (k, v) = kv.split_once('=')?;
                if k.is_empty() {
                    return None;
                }
                params.insert(k.to_owned(), v.to_owned());
            }
            Some(Route::Workflow {
                name: name.clone(),
                params,
                confidence: 1.0,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands() {
        assert_eq!(
            slash_command(" /STOP "),
            Some(Route::Command(Command {
                name: "stop".into(),
                args: vec![]
            }))
        );
        let Some(Route::Workflow { name, params, .. }) =
            slash_command(r#"/workflow daily_report to=a@b.c title="Hello World""#)
        else {
            panic!("not a workflow");
        };
        assert_eq!(name, "daily_report");
        assert_eq!(params["title"], "Hello World");
        assert_eq!(params["to"], "a@b.c");
        assert!(slash_command("/workflow").is_none());
        assert!(slash_command("/workflow x bad").is_none());
        assert!(slash_command("/usr/bin/ls").is_none());
        assert!(slash_command("stop").is_none());
    }
}
