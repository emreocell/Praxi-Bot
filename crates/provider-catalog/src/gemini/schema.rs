//! JSON Schema -> Gemini `Schema` (an `OpenAPI` 3.0 subset) conversion.
//!
//! Kept keywords: `type`, `format` (only values Gemini accepts), `description`,
//! `nullable`, `enum`, `items`, `properties`, `required`, `anyOf`, `minItems`,
//! `maxItems`, `minimum`, `maximum`, `minLength`, `maxLength`, `pattern`,
//! `propertyOrdering`, `minProperties`, `maxProperties`.
//!
//! Rewritten:
//! - `type` is upper-cased (`"string"` -> `"STRING"`); `["x","null"]` becomes
//!   `type: X` + `nullable: true`; several non-null types become `anyOf`.
//! - `$ref` into `#/$defs/..` or `#/definitions/..` is inlined (recursion is cut
//!   off at a fixed depth with a bare `OBJECT`).
//! - `oneOf` -> `anyOf`; `allOf` is flattened by merging `properties`/`required`.
//! - `const` -> single-value `enum`; `enum` values are stringified and a
//!   non-string `type` becomes `STRING` (Gemini only supports string enums).
//! - `format` is dropped unless Gemini supports it for the type
//!   (`date-time`/`enum` for strings, `int32`/`int64`, `float`/`double`).
//! - `required` is filtered to declared properties; empty `properties` and
//!   `required` are removed; array-form `items` keeps its first schema.
//!
//! Dropped (unsupported): `$schema`, `$id`, `$defs`, `definitions`, `$comment`,
//! `additionalProperties`, `patternProperties`, `unevaluatedProperties`,
//! `propertyNames`, `not`, `if`/`then`/`else`, `default`, `examples`, `title`,
//! `readOnly`/`writeOnly`, `deprecated`, `multipleOf`, `exclusiveMinimum`,
//! `exclusiveMaximum`, `uniqueItems`, `contains`, `minContains`, and any unknown
//! keyword.

use serde_json::{Map, Value, json};

const MAX_DEPTH: u32 = 12;

const COPIED: &[&str] = &[
    "description",
    "nullable",
    "minItems",
    "maxItems",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "propertyOrdering",
    "minProperties",
    "maxProperties",
];

/// Convert a tool's JSON-schema `parameters` to a Gemini schema. Returns `None`
/// when the schema declares no properties (Gemini rejects empty objects, so the
/// declaration must omit `parameters` entirely).
pub(super) fn convert_parameters(schema: &Value) -> Option<Value> {
    let out = convert(schema, schema, 0);
    let has_props = out
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty());
    has_props.then_some(out)
}

fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let path = reference.strip_prefix('#')?;
    if path.is_empty() {
        return Some(root);
    }
    root.pointer(path)
}

/// Merge `allOf` members (and `$ref`s) into one object schema.
fn flatten_all_of(obj: &Map<String, Value>, root: &Value, depth: u32) -> Map<String, Value> {
    let mut merged = obj.clone();
    let Some(Value::Array(members)) = merged.remove("allOf") else {
        return merged;
    };
    for m in &members {
        let mut m = m.clone();
        if let Some(t) = m
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| resolve(root, r))
        {
            m = t.clone();
        }
        let Some(mo) = m.as_object() else { continue };
        let mo = if depth < MAX_DEPTH {
            flatten_all_of(mo, root, depth + 1)
        } else {
            mo.clone()
        };
        for (k, v) in mo {
            match (k.as_str(), merged.get_mut(&k)) {
                ("properties", Some(Value::Object(dst))) => {
                    if let Value::Object(src) = v {
                        dst.extend(src);
                    }
                }
                ("required", Some(Value::Array(dst))) => {
                    if let Value::Array(src) = v {
                        dst.extend(src);
                    }
                }
                (_, Some(_)) => {}
                (_, None) => {
                    merged.insert(k, v);
                }
            }
        }
    }
    merged
}

fn supported_format(ty: &str, format: &str) -> bool {
    matches!(
        (ty, format),
        ("STRING", "date-time" | "enum")
            | ("INTEGER", "int32" | "int64")
            | ("NUMBER", "float" | "double")
    )
}

fn upper(t: &str) -> String {
    t.to_ascii_uppercase()
}

#[allow(clippy::too_many_lines)]
fn convert(node: &Value, root: &Value, depth: u32) -> Value {
    let Some(obj) = node.as_object() else {
        return json!({});
    };
    if depth > MAX_DEPTH {
        return json!({"type": "OBJECT"});
    }
    if let Some(target) = obj
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| resolve(root, r))
    {
        return convert(target, root, depth + 1);
    }
    let obj = flatten_all_of(obj, root, depth);
    let mut out = Map::new();
    for k in COPIED {
        if let Some(v) = obj.get(*k) {
            out.insert((*k).into(), v.clone());
        }
    }

    // type / nullable / anyOf-from-type-array
    let mut any_of: Vec<Value> = Vec::new();
    let mut ty: Option<String> = None;
    match obj.get("type") {
        Some(Value::String(t)) => ty = Some(upper(t)),
        Some(Value::Array(ts)) => {
            let names: Vec<&str> = ts.iter().filter_map(Value::as_str).collect();
            if names.contains(&"null") {
                out.insert("nullable".into(), Value::Bool(true));
            }
            let mut non_null = names.iter().filter(|t| **t != "null");
            match (non_null.next(), non_null.next()) {
                (Some(only), None) => ty = Some(upper(only)),
                (Some(_), Some(_)) => {
                    for t in names.iter().filter(|t| **t != "null") {
                        any_of.push(json!({"type": upper(t)}));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }

    // enum / const
    let enum_vals: Option<Vec<Value>> = obj
        .get("enum")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| obj.get("const").map(|c| vec![c.clone()]));
    if let Some(vals) = enum_vals {
        let strs: Vec<Value> = vals
            .iter()
            .map(|v| match v {
                Value::String(s) => Value::String(s.clone()),
                other => Value::String(other.to_string()),
            })
            .collect();
        out.insert("enum".into(), Value::Array(strs));
        ty = Some("STRING".into());
    }

    // properties / required
    if let Some(Value::Object(props)) = obj.get("properties") {
        let converted: Map<String, Value> = props
            .iter()
            .map(|(k, v)| (k.clone(), convert(v, root, depth + 1)))
            .collect();
        if !converted.is_empty() {
            if let Some(Value::Array(req)) = obj.get("required") {
                let req: Vec<Value> = req
                    .iter()
                    .filter(|r| r.as_str().is_some_and(|n| converted.contains_key(n)))
                    .cloned()
                    .collect();
                if !req.is_empty() {
                    out.insert("required".into(), Value::Array(req));
                }
            }
            out.insert("properties".into(), Value::Object(converted));
            ty.get_or_insert_with(|| "OBJECT".into());
        }
    }

    // items
    if let Some(items) = obj.get("items") {
        let first = match items {
            Value::Array(a) => a.first(),
            other => Some(other),
        };
        if let Some(i) = first {
            out.insert("items".into(), convert(i, root, depth + 1));
            ty.get_or_insert_with(|| "ARRAY".into());
        }
    }

    // anyOf / oneOf
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(alts)) = obj.get(key) {
            any_of.extend(alts.iter().map(|a| convert(a, root, depth + 1)));
        }
    }
    if !any_of.is_empty() {
        out.insert("anyOf".into(), Value::Array(any_of));
    }

    if let Some(t) = &ty {
        if let Some(f) = obj.get("format").and_then(Value::as_str)
            && supported_format(t, f)
        {
            out.insert("format".into(), f.into());
        }
        out.insert("type".into(), t.clone().into());
    }
    Value::Object(out)
}
