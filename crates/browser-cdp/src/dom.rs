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

/// Attributes whose value is a page-chosen name that can reveal a secret field.
pub const NAME_HINT_ATTRS: &[&str] = &["name", "id", "aria-label", "placeholder"];

/// Whether an element that is not an `<input>` / `<textarea>` still takes text:
/// `contenteditable` (anything but `false`) or a text-entry ARIA role.
pub fn is_editable_attrs<'a>(attr: impl Fn(&str) -> Option<&'a str>) -> bool {
    attr("contenteditable").is_some_and(|v| !v.trim().eq_ignore_ascii_case("false"))
        || attr("role").is_some_and(|r| {
            r.split_whitespace().any(|w| {
                matches!(
                    w.to_ascii_lowercase().as_str(),
                    "textbox" | "searchbox" | "combobox"
                )
            })
        })
}

/// Computed styles requested from `DOMSnapshot.captureSnapshot`.
pub const COMPUTED_STYLES: &[&str] = &["cursor"];

fn attr_str<'a>(pairs: &[i64], strings: &[&'a str], key: &str) -> Option<&'a str> {
    let s = |i: i64| -> &'a str {
        usize::try_from(i)
            .ok()
            .and_then(|i| strings.get(i).copied())
            .unwrap_or("")
    };
    pairs
        .as_chunks::<2>()
        .0
        .iter()
        .find_map(|&[k, v]| (s(k) == key).then(|| s(v)))
}

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
        let attrs_raw = nodes.get("attributes").and_then(Value::as_array);
        let n = parent.len();

        // layout: 0 = not rendered, 1 = rendered (non-pointer), 2 = rendered with cursor:pointer.
        let mut layout_state = vec![0u8; n];
        let layout_idx = ints(doc["layout"].get("nodeIndex"));
        let styles = doc["layout"]
            .get("styles")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for (k, &ni) in layout_idx.iter().enumerate() {
            let Ok(ni) = usize::try_from(ni) else {
                continue;
            };
            if ni >= n {
                continue;
            }
            let c = styles
                .get(k)
                .and_then(Value::as_array)
                .and_then(|st| st.first())
                .and_then(Value::as_i64)
                .map_or("", s);
            layout_state[ni] = if c == "pointer" { 2 } else { 1 };
        }

        let mut extra_of = vec![usize::MAX; n];
        let mut local: Vec<DomExtra> = Vec::new();
        let mut attr_buf: Vec<i64> = Vec::new();
        for i in 0..n {
            if kind.get(i) != Some(&1) {
                continue;
            }
            let tag_raw = s(name.get(i).copied().unwrap_or(-1));
            let is_input =
                tag_raw.eq_ignore_ascii_case("input") || tag_raw.eq_ignore_ascii_case("textarea");
            let rendered = layout_state[i] != 0;

            attr_buf.clear();
            if let Some(arr) = attrs_raw.and_then(|a| a.get(i)).and_then(Value::as_array) {
                attr_buf.extend(arr.iter().map(|x| x.as_i64().unwrap_or(-1)));
            }

            let has_onclick = rendered && attr_str(&attr_buf, &strings, "onclick").is_some();
            let tabindex = if rendered {
                attr_str(&attr_buf, &strings, "tabindex")
                    .and_then(|t| t.trim().parse::<i32>().ok())
                    .filter(|t| *t >= 0)
            } else {
                None
            };
            let is_pointer = layout_state[i] == 2;
            let parent_pointer = usize::try_from(parent.get(i).copied().unwrap_or(-1))
                .is_ok_and(|p| p < n && layout_state[p] == 2);
            let cursor_pointer = rendered && is_pointer && !parent_pointer;
            let candidate = rendered && (cursor_pointer || has_onclick || tabindex.is_some());
            let editable = !is_input && is_editable_attrs(|k| attr_str(&attr_buf, &strings, k));
            if !candidate && !is_input && !editable {
                continue;
            }

            let fallback = ["aria-label", "title", "alt"]
                .iter()
                .find_map(|k| attr_str(&attr_buf, &strings, k).filter(|v| !v.trim().is_empty()))
                .map_or(String::new(), str::to_string);
            extra_of[i] = local.len();
            local.push(DomExtra {
                backend_id: backend.get(i).copied().unwrap_or(-1),
                frame_id: frame_id.clone(),
                tag: tag_raw.to_ascii_lowercase(),
                cursor_pointer,
                onclick: has_onclick,
                tabindex,
                input_type: is_input.then(|| {
                    attr_str(&attr_buf, &strings, "type")
                        .unwrap_or("text")
                        .to_string()
                }),
                autocomplete: attr_str(&attr_buf, &strings, "autocomplete").map(str::to_string),
                text: fallback,
                name_hints: NAME_HINT_ATTRS
                    .iter()
                    .filter_map(|k| attr_str(&attr_buf, &strings, k).map(str::to_string))
                    .collect(),
                editable,
            });
        }

        // Gather text of candidates from descendant text nodes (nearest candidate ancestor).
        if local.iter().any(|e| e.tag != "input") {
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
                    if pi >= n {
                        break;
                    }
                    let ei = extra_of[pi];
                    if ei != usize::MAX && local[ei].tag != "input" {
                        let buf = gathered.entry(ei).or_default();
                        if !buf.is_empty() {
                            buf.push(' ');
                        }
                        buf.push_str(text);
                        break;
                    }
                    p = parent[pi];
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
    fn secret_looking_names_are_sensitive() {
        let mk = |hint: &str| DomExtra {
            input_type: Some("text".into()),
            name_hints: vec![hint.into()],
            ..Default::default()
        };
        assert!(mk("user_PassWd").is_sensitive() && mk("Card number").is_sensitive());
        assert!(!mk("username").is_sensitive());
        // `otp` only counts as a word of its own.
        for yes in ["otp", "user_otp", "totp", "mfaOtp", "otp-code"] {
            assert!(mk(yes).is_sensitive(), "{yes}");
        }
        for no in ["footpath", "hotpot", "spotpicker", "protocol"] {
            assert!(!mk(no).is_sensitive(), "{no}");
        }
    }

    #[test]
    fn editable_non_input_with_secret_name_is_sensitive() {
        // strings: 0 "main" 1 "HTML" 2 "BODY" 3 "DIV" 4 "contenteditable" 5 "true" 6 "aria-label"
        // 7 "Password" 8 "placeholder" 9 "Message" 10 "Verification code"
        let resp = |attrs: Value| {
            json!({
                "strings": ["main","HTML","BODY","DIV","contenteditable","true","aria-label",
                            "Password","placeholder","Message","Verification code"],
                "documents": [{
                    "frameId": 0,
                    "nodes": {
                        "parentIndex": [-1,0,1],
                        "nodeType": [1,1,1],
                        "nodeName": [1,2,3],
                        "nodeValue": [-1,-1,-1],
                        "backendNodeId": [1,2,50],
                        "attributes": [[],[],attrs]
                    },
                    "layout": {"nodeIndex": [0,1,2], "styles": [[0],[0],[0]]}
                }]
            })
        };
        let extras = parse_dom_snapshot(&resp(json!([4, 5, 6, 7])));
        let div = extras
            .iter()
            .find(|e| e.backend_id == 50)
            .expect("editable div kept");
        assert!(div.editable && div.is_sensitive(), "{div:?}");
        let extras = parse_dom_snapshot(&resp(json!([4, 5, 8, 10])));
        assert!(
            extras
                .iter()
                .find(|e| e.backend_id == 50)
                .unwrap()
                .is_sensitive()
        );
        let extras = parse_dom_snapshot(&resp(json!([4, 5, 6, 9])));
        assert!(
            !extras
                .iter()
                .find(|e| e.backend_id == 50)
                .unwrap()
                .is_sensitive()
        );
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
