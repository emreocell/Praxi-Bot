//! Turn a `DOMSnapshot.captureSnapshot` response into [`DomExtra`] facts
//! (cursor:pointer / onclick / tabindex candidates and input types for value masking).

use std::collections::HashMap;

use serde_json::Value;

use crate::snapshot::DomExtra;

const MAX_LABEL: usize = 80;

fn ints(v: Option<&Value>) -> Vec<i64> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| x.as_i64().unwrap_or(-1)).collect())
        .unwrap_or_default()
}

/// Computed styles requested from `DOMSnapshot.captureSnapshot`.
pub const COMPUTED_STYLES: &[&str] = &["cursor"];

/// Parse every document in a `DOMSnapshot.captureSnapshot` result.
pub fn parse_dom_snapshot(resp: &Value) -> Vec<DomExtra> {
    let strings: Vec<&str> = resp
        .get("strings")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|s| s.as_str().unwrap_or("")).collect())
        .unwrap_or_default();
    let s = |i: i64| -> &str {
        usize::try_from(i)
            .ok()
            .and_then(|i| strings.get(i).copied())
            .unwrap_or("")
    };
    let mut out = Vec::new();
    for doc in resp
        .get("documents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let frame_id = doc
            .get("frameId")
            .and_then(Value::as_i64)
            .map(|i| s(i).to_string())
            .filter(|f| !f.is_empty());
        let nodes = &doc["nodes"];
        let parent = ints(nodes.get("parentIndex"));
        let kind = ints(nodes.get("nodeType"));
        let name = ints(nodes.get("nodeName"));
        let value = ints(nodes.get("nodeValue"));
        let backend = ints(nodes.get("backendNodeId"));
        let attrs: Vec<Vec<i64>> = nodes
            .get("attributes")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|x| ints(Some(x))).collect())
            .unwrap_or_default();
        let n = parent.len();

        // layout: which nodes are rendered, and their cursor.
        let mut cursor: HashMap<usize, &str> = HashMap::new();
        let layout_idx = ints(doc["layout"].get("nodeIndex"));
        let styles = doc["layout"]
            .get("styles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (k, &ni) in layout_idx.iter().enumerate() {
            let c = styles
                .get(k)
                .map(|st| ints(Some(st)))
                .and_then(|st| st.first().copied())
                .map_or("", s);
            if let Ok(ni) = usize::try_from(ni) {
                cursor.insert(ni, c);
            }
        }

        let mut extra_of: HashMap<usize, usize> = HashMap::new();
        let mut local: Vec<DomExtra> = Vec::new();
        for i in 0..n {
            if kind.get(i) != Some(&1) {
                continue;
            }
            let tag = s(name.get(i).copied().unwrap_or(-1)).to_ascii_lowercase();
            let mut attr: HashMap<&str, &str> = HashMap::new();
            for pair in attrs
                .get(i)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .chunks(2)
            {
                if let [k, v] = pair {
                    attr.insert(s(*k), s(*v));
                }
            }
            let rendered = cursor.contains_key(&i);
            let is_pointer = cursor.get(&i).is_some_and(|c| *c == "pointer");
            let parent_pointer = usize::try_from(parent.get(i).copied().unwrap_or(-1))
                .ok()
                .and_then(|p| cursor.get(&p))
                .is_some_and(|c| *c == "pointer");
            let tabindex = attr
                .get("tabindex")
                .and_then(|t| t.trim().parse::<i32>().ok())
                .filter(|t| *t >= 0);
            let candidate = rendered
                && ((is_pointer && !parent_pointer)
                    || attr.contains_key("onclick")
                    || tabindex.is_some());
            let is_input = matches!(tag.as_str(), "input" | "textarea");
            if !candidate && !is_input {
                continue;
            }
            let fallback = ["aria-label", "title", "alt"]
                .iter()
                .find_map(|k| attr.get(k).filter(|v| !v.trim().is_empty()))
                .map_or(String::new(), |v| (*v).to_string());
            extra_of.insert(i, local.len());
            local.push(DomExtra {
                backend_id: backend.get(i).copied().unwrap_or(-1),
                frame_id: frame_id.clone(),
                tag,
                cursor_pointer: is_pointer && !parent_pointer,
                onclick: attr.contains_key("onclick"),
                tabindex,
                input_type: is_input
                    .then(|| attr.get("type").copied().unwrap_or("text").to_string()),
                autocomplete: attr.get("autocomplete").map(|a| (*a).to_string()),
                text: fallback,
            });
        }

        // Gather text of candidates from descendant text nodes (nearest candidate ancestor).
        let mut gathered: HashMap<usize, String> = HashMap::new();
        for i in 0..n {
            if kind.get(i) != Some(&3) {
                continue;
            }
            let text = s(value.get(i).copied().unwrap_or(-1)).trim();
            if text.is_empty() {
                continue;
            }
            let mut p = parent.get(i).copied().unwrap_or(-1);
            let mut hops = 0;
            while let Ok(pi) = usize::try_from(p) {
                if let Some(&ei) = extra_of.get(&pi)
                    && local[ei].tag != "input"
                {
                    let buf = gathered.entry(ei).or_default();
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    buf.push_str(text);
                    break;
                }
                p = parent.get(pi).copied().unwrap_or(-1);
                hops += 1;
                if hops > 32 {
                    break;
                }
            }
        }
        for (ei, text) in gathered {
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            local[ei].text = flat.chars().take(MAX_LABEL).collect();
        }
        out.extend(local);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // strings: 0 "main" 1 "HTML" 2 "BODY" 3 "DIV" 4 "INPUT" 5 "type" 6 "password" 7 "pointer"
    // 8 "auto" 9 "Click me" 10 "SPAN" 11 "tabindex" 12 "0" 13 "#text" 14 "inner" 15 "autocomplete" 16 "cc-number"
    fn fixture() -> Value {
        json!({
            "strings": ["main","HTML","BODY","DIV","INPUT","type","password","pointer","auto",
                        "Click me","SPAN","tabindex","0","#text","inner","autocomplete","cc-number"],
            "documents": [{
                "frameId": 0,
                "nodes": {
                    "parentIndex": [-1,0,1,2,1,1,5],
                    "nodeType":    [1,1,1,3,1,1,3],
                    "nodeName":    [1,2,3,13,4,10,13],
                    "nodeValue":   [-1,-1,-1,9,-1,-1,14],
                    "backendNodeId":[1,2,40,41,42,43,44],
                    "attributes":  [[],[],[],[],[5,6,15,16],[11,12],[]]
                },
                "layout": {
                    "nodeIndex": [0,1,2,4,5],
                    "styles": [[8],[8],[7],[8],[8]]
                }
            }]
        })
    }

    #[test]
    fn finds_candidates_inputs_and_text() {
        let extras = parse_dom_snapshot(&fixture());
        assert_eq!(extras.len(), 3, "{extras:?}");
        let div = extras.iter().find(|e| e.backend_id == 40).unwrap();
        assert!(div.cursor_pointer && div.is_cursor_interactive());
        assert_eq!(div.text, "Click me");
        assert_eq!(div.frame_id.as_deref(), Some("main"));
        let input = extras.iter().find(|e| e.backend_id == 42).unwrap();
        assert!(input.is_sensitive());
        assert_eq!(input.input_type.as_deref(), Some("password"));
        let span = extras.iter().find(|e| e.backend_id == 43).unwrap();
        assert_eq!(span.tabindex, Some(0));
        assert_eq!(span.text, "inner");
    }
}
