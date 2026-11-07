//! `json_query`: JSONPath (RFC 9535) queries over a JSON file or inline value.

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Value, json};
use serde_json_path::JsonPath;
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, clip, exec, invalid, parse, read_limited};

#[derive(Deserialize)]
struct In {
    /// JSON file path (use this or `json`).
    path: Option<String>,
    /// Inline JSON value or JSON text.
    json: Option<Value>,
    /// JSONPath query, e.g. `$.items[?@.price > 10].name`.
    query: String,
    /// Maximum matches to return.
    limit: Option<usize>,
}

pub fn spec() -> ToolSpec {
    crate::mk_spec(
        "json_query",
        "Query JSON with JSONPath (RFC 9535: $.a.b[0], $..name, $.items[?@.price > 10], slices). Source is a file path or an inline json value. Example: {\"path\":\"data.json\",\"query\":\"$.items[*].name\",\"limit\":20}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"JSON file path."},
            "json":{"description":"Inline JSON (object/array or JSON text) instead of path."},
            "query":{"type":"string","description":"JSONPath expression."},
            "limit":{"type":"integer","minimum":1,"description":"Max matches (default config max_rows)."}
        },"required":["query"]}),
        Effect::Read,
    )
}

pub fn run(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: In = parse(input)?;
    let (source, doc): (String, Value) = match (&i.path, i.json) {
        (Some(p), None) => {
            let bytes = read_limited(cfg, p)?;
            let v = serde_json::from_slice(&bytes)
                .map_err(|e| exec(format!("invalid JSON in {p}: {e}")))?;
            (p.clone(), v)
        }
        (None, Some(Value::String(s))) => {
            let v = serde_json::from_str(&s)
                .map_err(|e| invalid(format!("inline json text invalid: {e}")))?;
            ("inline".into(), v)
        }
        (None, Some(v)) => ("inline".into(), v),
        _ => return Err(invalid("give exactly one of path or json")),
    };
    let path = JsonPath::parse(&i.query).map_err(|e| invalid(format!("bad JSONPath: {e}")))?;
    let nodes = path.query_located(&doc);
    let total = nodes.len();
    let limit = i
        .limit
        .unwrap_or(cfg.max_rows)
        .clamp(1, cfg.max_rows.max(1));
    let matches: Vec<Value> = nodes.iter().take(limit).map(|n| n.node().clone()).collect();
    let locations: Vec<String> = nodes
        .iter()
        .take(limit)
        .map(|n| n.location().to_string())
        .collect();
    let mut md = format!("`{}` on {source}: {total} match(es)\n\n", i.query);
    for (loc, m) in locations.iter().zip(&matches) {
        let _ = writeln!(md, "- `{loc}`: {}", clip(&m.to_string(), 200));
    }
    if total > limit {
        let _ = write!(
            md,
            "
_…{} more matches_
",
            total - limit
        );
    }
    let data = json!({"source": source, "query": i.query, "total": total, "truncated": total > limit,
        "matches": matches, "locations": locations});
    Ok(Reply::new(cfg, &md, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_inline_and_file() {
        let cfg = DocToolsConfig::default();
        let doc = json!({"items":[{"name":"a","price":5},{"name":"b","price":20},{"name":"c","price":30}]});
        let r = run(
            &cfg,
            json!({"json": doc, "query": "$.items[?@.price > 10].name"}),
        )
        .unwrap();
        assert_eq!(r.data["matches"], json!(["b", "c"]));
        assert_eq!(r.data["total"], 2);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.json");
        std::fs::write(&p, doc.to_string()).unwrap();
        let r = run(
            &cfg,
            json!({"path": p.to_str().unwrap(), "query": "$..name", "limit": 1}),
        )
        .unwrap();
        assert_eq!(r.data["matches"], json!(["a"]));
        assert_eq!(r.data["truncated"], true);
        assert!(r.markdown.contains("more matches"));
    }

    #[test]
    fn errors_are_clear() {
        let cfg = DocToolsConfig::default();
        assert!(run(&cfg, json!({"json": {}, "query": "$[["})).is_err());
        assert!(run(&cfg, json!({"query": "$"})).is_err());
        assert!(run(&cfg, json!({"json": "{bad", "query": "$"})).is_err());
    }

    #[test]
    fn limit_is_clamped_to_max_rows() {
        let cfg = DocToolsConfig {
            max_rows: 3,
            ..DocToolsConfig::default()
        };
        let r = run(
            &cfg,
            json!({"json": [1,2,3,4,5,6], "query": "$[*]", "limit": 100}),
        )
        .unwrap();
        assert_eq!(r.data["matches"].as_array().unwrap().len(), 3);
        assert_eq!(r.data["total"], 6);
    }
}
