//! BM25 tool discovery index over tool name + description + parameter names.

use serde_json::Value;
use tool_registry::ToolSpec;

use crate::bm25::{Bm25Index, tokenize};

/// How many times the tool *name* is repeated in the indexed document, i.e. a
/// cheap field boost: a hit in the name outweighs a hit in the description.
const NAME_BOOST: usize = 3;

/// One discovery result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolHit {
    /// Full tool name (`mcp:<server>/<tool>` for MCP tools).
    pub name: String,
    /// The tool's description.
    pub description: String,
    /// BM25 score (only meaningful relative to other hits of the same query).
    pub score: f64,
}

/// An immutable BM25 index over a snapshot of tool specs.
#[derive(Debug, Clone, Default)]
pub struct ToolIndex {
    entries: Vec<(String, String)>,
    index: Bm25Index,
}

/// Names of the top-level properties declared by a JSON-Schema input object.
fn param_names(schema: &Value) -> Vec<&str> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

impl ToolIndex {
    /// Index `specs`.
    #[must_use]
    pub fn build(specs: &[ToolSpec]) -> Self {
        let docs = specs.iter().map(|spec| {
            let mut terms = Vec::new();
            let name_terms = tokenize(&spec.name);
            for _ in 0..NAME_BOOST {
                terms.extend(name_terms.iter().cloned());
            }
            terms.extend(tokenize(&spec.description));
            for param in param_names(&spec.input_schema) {
                terms.extend(tokenize(param));
            }
            terms
        });
        Self {
            index: Bm25Index::new(docs),
            entries: specs
                .iter()
                .map(|s| (s.name.clone(), s.description.clone()))
                .collect(),
        }
    }

    /// Number of indexed tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The best `k` tools for a natural-language `query`, best first.
    #[must_use]
    pub fn search(&self, query: &str, k: usize) -> Vec<ToolHit> {
        self.index
            .search(&tokenize(query), k)
            .into_iter()
            .map(|(doc, score)| ToolHit {
                name: self.entries[doc].0.clone(),
                description: self.entries[doc].1.clone(),
                score,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_registry::{Effect, PermissionCategory, Platform};

    use super::*;

    fn spec(name: &str, description: &str, params: &[&str]) -> ToolSpec {
        let props: serde_json::Map<String, Value> = params
            .iter()
            .map(|p| ((*p).to_owned(), json!({ "type": "string" })))
            .collect();
        ToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema: json!({ "type": "object", "properties": props }),
            effect: Effect::Read,
            category: PermissionCategory::Accounts,
            platforms: vec![Platform::All],
        }
    }

    /// ~50 fake tools across a handful of fake servers.
    fn corpus() -> Vec<ToolSpec> {
        let mut specs = vec![
            spec(
                "mcp:github/create_issue",
                "Open a new issue in a repository",
                &["repo", "title", "body"],
            ),
            spec(
                "mcp:github/list_pull_requests",
                "List pull requests of a repository",
                &["repo", "state"],
            ),
            spec(
                "mcp:slack/post_message",
                "Post a message to a Slack channel",
                &["channel", "text"],
            ),
            spec(
                "mcp:gmail/send_email",
                "Send an e-mail to a recipient",
                &["to", "subject", "body"],
            ),
            spec(
                "mcp:fs/read_file",
                "Read the contents of a file from disk",
                &["path"],
            ),
            spec(
                "mcp:calendar/create_event",
                "Schedule a meeting on the calendar",
                &["title", "start", "attendees"],
            ),
            spec(
                "mcp:weather/get_forecast",
                "Weather forecast temperature for a city",
                &["city", "days"],
            ),
            spec(
                "mcp:db/run_query",
                "Execute a SQL query against the database",
                &["sql"],
            ),
        ];
        // Filler tools that share generic vocabulary but none of the above topics.
        for i in 0..42 {
            specs.push(spec(
                &format!("mcp:filler{i}/do_thing_{i}"),
                &format!("Generic helper number {i} that processes an item"),
                &["item", "option"],
            ));
        }
        assert_eq!(specs.len(), 50);
        specs
    }

    fn top(index: &ToolIndex, query: &str) -> String {
        index.search(query, 3)[0].name.clone()
    }

    #[test]
    fn ranks_the_right_tool_first_among_fifty() {
        let index = ToolIndex::build(&corpus());
        assert_eq!(index.len(), 50);
        assert_eq!(top(&index, "open an issue on github"), "mcp:github/create_issue");
        assert_eq!(top(&index, "send email"), "mcp:gmail/send_email");
        assert_eq!(top(&index, "read a file"), "mcp:fs/read_file");
        assert_eq!(top(&index, "weather forecast for a city"), "mcp:weather/get_forecast");
        assert_eq!(top(&index, "sql query"), "mcp:db/run_query");
        assert_eq!(top(&index, "schedule meeting attendees"), "mcp:calendar/create_event");
    }

    #[test]
    fn parameter_names_are_searchable() {
        let index = ToolIndex::build(&corpus());
        // "channel" only appears as a parameter (and in the description) of slack.
        assert_eq!(top(&index, "channel"), "mcp:slack/post_message");
        // "attendees" is a parameter name only.
        assert_eq!(top(&index, "attendees"), "mcp:calendar/create_event");
    }

    #[test]
    fn name_hits_outweigh_description_hits() {
        let specs = vec![
            spec("alpha", "reports on widget status", &[]),
            spec("widget", "does something else entirely", &[]),
        ];
        let index = ToolIndex::build(&specs);
        assert_eq!(top(&index, "widget"), "widget");
    }

    #[test]
    fn respects_k_and_returns_nothing_for_unknown_terms() {
        let index = ToolIndex::build(&corpus());
        assert_eq!(index.search("helper", 5).len(), 5);
        assert!(index.search("zzzzqqq", 5).is_empty());
        assert!(index.search("", 5).is_empty());
    }
}
