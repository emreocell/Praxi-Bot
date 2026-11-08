//! Spec parsing: `OpenAPI` document -> [`ApiSpec`] of [`Operation`]s.

use std::collections::{BTreeMap, HashSet};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::OpenApiError;

/// HTTP methods that become tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET
    Get,
    /// HEAD
    Head,
    /// OPTIONS
    Options,
    /// POST
    Post,
    /// PUT
    Put,
    /// PATCH
    Patch,
    /// DELETE
    Delete,
}

impl HttpMethod {
    const ALL: [(&'static str, Self); 7] = [
        ("get", Self::Get),
        ("head", Self::Head),
        ("options", Self::Options),
        ("post", Self::Post),
        ("put", Self::Put),
        ("patch", Self::Patch),
        ("delete", Self::Delete),
    ];

    /// Upper-case method name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    /// Whether the method cannot change server state.
    #[must_use]
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Get | Self::Head | Self::Options)
    }
}

/// Where a parameter is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamLocation {
    /// URL path segment.
    Path,
    /// Query string.
    Query,
    /// Request header.
    Header,
}

impl ParamLocation {
    fn label(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Header => "header",
        }
    }
}

/// One path / query / header parameter and the tool-input key it is bound to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamBinding {
    /// Property name in the tool input object.
    pub key: String,
    /// Wire name (query key, header name, path placeholder).
    pub name: String,
    /// Where it goes.
    pub location: ParamLocation,
    /// Whether the caller must supply it.
    pub required: bool,
    /// Whether arrays / objects are exploded (`a=1&a=2`).
    pub explode: bool,
    /// Resolved JSON Schema of the value.
    pub schema: Value,
}

/// Supported request body encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyKind {
    /// `application/json` (or `+json`).
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
}

/// The request body of an operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodySpec {
    /// Encoding.
    pub kind: BodyKind,
    /// Whether the body is mandatory.
    pub required: bool,
    /// Resolved JSON Schema.
    pub schema: Value,
}

/// One importable API operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    /// Unique tool name (prefix applied, `[A-Za-z0-9_]`, at most 64 bytes).
    pub name: String,
    /// The spec's `operationId`, if any.
    pub operation_id: Option<String>,
    /// HTTP method.
    pub method: HttpMethod,
    /// Path template, e.g. `/pets/{petId}`.
    pub path: String,
    /// Tags.
    pub tags: Vec<String>,
    /// Human description (summary + description).
    pub description: String,
    /// Marked deprecated in the spec.
    pub deprecated: bool,
    /// Path / query / header parameters.
    pub params: Vec<ParamBinding>,
    /// Request body.
    pub body: Option<BodySpec>,
}

/// An operation that was left out and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedOperation {
    /// Method (upper-case).
    pub method: String,
    /// Path template.
    pub path: String,
    /// Reason.
    pub reason: String,
}

/// A declared security scheme (informational: helps choose an
/// [`crate::Auth`] configuration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecuritySchemeInfo {
    /// Scheme name in `components.securitySchemes`.
    pub name: String,
    /// Human summary, e.g. `apiKey in header X-Api-Key`, `http bearer`.
    pub kind: String,
}

/// An imported API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiSpec {
    /// `info.title`.
    pub title: String,
    /// `info.version`.
    pub version: String,
    /// Base URL requests are sent to (no trailing slash).
    pub base_url: String,
    /// Selected operations.
    pub operations: Vec<Operation>,
    /// Operations dropped because they are unsupported.
    pub skipped: Vec<SkippedOperation>,
    /// Non-fatal notes (unresolvable refs, ...).
    pub warnings: Vec<String>,
    /// Declared security schemes.
    pub security_schemes: Vec<SecuritySchemeInfo>,
}

/// Import knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportOptions {
    /// Prefix for tool names (`<prefix>_<operation>`); empty for none.
    pub name_prefix: String,
    /// Override for the base URL (required when `servers` is relative/missing).
    pub base_url: Option<String>,
    /// Keep only operations having one of these tags (empty = all).
    pub include_tags: Vec<String>,
    /// Drop operations having one of these tags.
    pub exclude_tags: Vec<String>,
    /// Allow-list of `operationId`s (or generated names); empty = all.
    pub include_operation_ids: Vec<String>,
    /// Deny-list of `operationId`s (or generated names).
    pub exclude_operation_ids: Vec<String>,
    /// Import only GET / HEAD / OPTIONS operations.
    pub read_only: bool,
    /// Import operations marked `deprecated`.
    pub include_deprecated: bool,
    /// Fail when more operations than this are selected.
    pub max_operations: usize,
    /// Reject specs larger than this many bytes.
    pub max_spec_bytes: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            name_prefix: String::new(),
            base_url: None,
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            include_operation_ids: Vec::new(),
            exclude_operation_ids: Vec::new(),
            read_only: false,
            include_deprecated: false,
            max_operations: 100,
            max_spec_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Maximum tool-name length accepted by common providers.
const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION: usize = 1500;
const MAX_REF_DEPTH: usize = 24;
const NODE_BUDGET: usize = 200_000;

impl ApiSpec {
    /// Parse a JSON or YAML `OpenAPI` 3.0 / 3.1 document.
    ///
    /// Only local (`#/...`) `$ref`s are followed; external references are
    /// replaced by an unconstrained schema and reported in `warnings`.
    ///
    /// # Errors
    /// See [`OpenApiError`].
    pub fn parse(text: &str, opts: &ImportOptions) -> Result<Self, OpenApiError> {
        if text.len() > opts.max_spec_bytes {
            return Err(OpenApiError::SpecTooLarge {
                size: text.len(),
                limit: opts.max_spec_bytes,
            });
        }
        let root = parse_document(text)?;
        let version = root
            .get("openapi")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !(version.starts_with("3.0") || version.starts_with("3.1")) {
            return Err(OpenApiError::UnsupportedVersion(version.to_string()));
        }
        let mut res = Resolver::new(&root);
        let base_url = base_url(&root, opts)?;

        let mut operations = Vec::new();
        let mut skipped = Vec::new();
        let empty = Map::new();
        let paths = root
            .get("paths")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        for (path, item) in paths {
            let Some(item) = res.resolve_top(item) else {
                continue;
            };
            for (m, method) in HttpMethod::ALL {
                let Some(op_val) = item.get(m) else { continue };
                let Some(op_val) = res.resolve_top(op_val) else {
                    continue;
                };
                match build_operation(&mut res, path, &item, &op_val, method, opts) {
                    Ok(op) => {
                        if !is_filtered_out(&op, opts) {
                            operations.push(op);
                        }
                    }
                    Err(reason) => skipped.push(SkippedOperation {
                        method: method.as_str().to_string(),
                        path: path.clone(),
                        reason,
                    }),
                }
            }
        }
        if operations.len() > opts.max_operations {
            return Err(OpenApiError::TooManyOperations {
                count: operations.len(),
                limit: opts.max_operations,
            });
        }
        let mut used_names: HashSet<String> = HashSet::new();
        for op in &mut operations {
            op.name = unique_name(&op.name, &mut used_names);
        }
        let info = root.get("info");
        let text_of = |k: &str| {
            info.and_then(|i| i.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Ok(Self {
            title: text_of("title"),
            version: text_of("version"),
            base_url,
            operations,
            skipped,
            warnings: res.warnings,
            security_schemes: security_schemes(&root),
        })
    }

    /// Operation counts per tag (untagged operations are counted under `""`),
    /// useful for choosing import filters.
    #[must_use]
    pub fn tag_counts(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for op in &self.operations {
            if op.tags.is_empty() {
                *m.entry(String::new()).or_insert(0) += 1;
            }
            for t in &op.tags {
                *m.entry(t.clone()).or_insert(0) += 1;
            }
        }
        m
    }
}

fn parse_document(text: &str) -> Result<Value, OpenApiError> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') {
        serde_json::from_str(trimmed).map_err(|e| OpenApiError::Parse(e.to_string()))
    } else {
        serde_yaml_ng::from_str::<Value>(text).map_err(|e| OpenApiError::Parse(e.to_string()))
    }
}

fn is_filtered_out(op: &Operation, o: &ImportOptions) -> bool {
    if o.read_only && !op.method.is_read_only() {
        return true;
    }
    if op.deprecated && !o.include_deprecated {
        return true;
    }
    let has_tag = |list: &[String]| op.tags.iter().any(|t| list.contains(t));
    if !o.include_tags.is_empty() && !has_tag(&o.include_tags) {
        return true;
    }
    if has_tag(&o.exclude_tags) {
        return true;
    }
    let id_in = |list: &[String]| {
        list.iter()
            .any(|i| Some(i) == op.operation_id.as_ref() || *i == op.name)
    };
    (!o.include_operation_ids.is_empty() && !id_in(&o.include_operation_ids))
        || id_in(&o.exclude_operation_ids)
}

fn base_url(root: &Value, opts: &ImportOptions) -> Result<String, OpenApiError> {
    let raw = if let Some(b) = &opts.base_url {
        b.clone()
    } else {
        let server = root
            .get("servers")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| OpenApiError::NoServer("spec has no `servers`".into()))?;
        let mut url = server
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(vars) = server.get("variables").and_then(Value::as_object) {
            for (k, v) in vars {
                if let Some(d) = v.get("default").and_then(Value::as_str) {
                    url = url.replace(&format!("{{{k}}}"), d);
                }
            }
        }
        url
    };
    let parsed = Url::parse(&raw).map_err(|e| {
        OpenApiError::NoServer(format!(
            "`{raw}` is not an absolute URL ({e}); pass ImportOptions::base_url"
        ))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(OpenApiError::NoServer(format!(
            "`{raw}` must be an http(s) URL with a host"
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(OpenApiError::NoServer(
            "server URL must not embed credentials".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(OpenApiError::NoServer(
            "server URL must not contain a query or fragment".into(),
        ));
    }
    Ok(raw.trim_end_matches('/').to_string())
}

fn security_schemes(root: &Value) -> Vec<SecuritySchemeInfo> {
    let Some(map) = root
        .pointer("/components/securitySchemes")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    map.iter()
        .map(|(name, v)| {
            let kind = match v.get("type").and_then(Value::as_str) {
                Some("apiKey") => format!("apiKey in {} {}", s(v, "in"), s(v, "name")),
                Some("http") => format!("http {}", s(v, "scheme")),
                Some(other) => other.to_string(),
                None => "unknown".to_string(),
            };
            SecuritySchemeInfo {
                name: name.clone(),
                kind,
            }
        })
        .collect()
}

/// `[A-Za-z0-9_]` only, runs of other characters collapsed to `_`.
fn sanitize_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn cut(s: &str, max: usize) -> &str {
    let mut i = max.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

fn unique_name(name: &str, used: &mut HashSet<String>) -> String {
    let base = cut(name, MAX_NAME_LEN).to_string();
    let mut candidate = base.clone();
    let mut n = 2;
    while !used.insert(candidate.clone()) {
        let suffix = format!("_{n}");
        candidate = format!("{}{suffix}", cut(&base, MAX_NAME_LEN - suffix.len()));
        n += 1;
    }
    candidate
}

fn build_operation(
    res: &mut Resolver<'_>,
    path: &str,
    item: &Value,
    op: &Value,
    method: HttpMethod,
    opts: &ImportOptions,
) -> Result<Operation, String> {
    let operation_id = op
        .get("operationId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let base_name = operation_id
        .as_deref()
        .map(sanitize_name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            sanitize_name(&format!(
                "{}_{}",
                method.as_str().to_ascii_lowercase(),
                path
            ))
        });
    let prefix = sanitize_name(&opts.name_prefix);
    let name = if prefix.is_empty() {
        base_name
    } else {
        format!("{prefix}_{base_name}")
    };

    // Merge path-level and operation-level parameters; op-level wins.
    let mut raw: Vec<(String, String, Value)> = Vec::new(); // (in, name, param)
    for src in [item, op] {
        for p in src
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(p) = res.resolve_top(p) else {
                return Err("unresolvable parameter $ref".into());
            };
            let loc = p
                .get("in")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let pname = p
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if pname.is_empty() {
                continue;
            }
            raw.retain(|(l, n, _)| !(*l == loc && *n == pname));
            raw.push((loc, pname, p));
        }
    }
    raw.retain(|(l, n, _)| {
        matches!(l.as_str(), "path" | "query" | "header") && !is_reserved_header(l, n)
    });

    // Every `{placeholder}` must be a declared path parameter.
    for placeholder in placeholders(path) {
        if !raw.iter().any(|(l, n, _)| l == "path" && *n == placeholder) {
            return Err(format!("undeclared path parameter `{placeholder}`"));
        }
    }

    let body = build_body(res, op)?;
    let params = bind_params(res, &raw, body.is_some());

    let summary = op.get("summary").and_then(Value::as_str).unwrap_or("");
    let desc = op.get("description").and_then(Value::as_str).unwrap_or("");
    let mut description = [summary, desc]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if description.len() > MAX_DESCRIPTION {
        description = format!("{}…", cut(&description, MAX_DESCRIPTION));
    }
    let line = format!("[{} {path}]", method.as_str());
    description = if description.is_empty() {
        line
    } else {
        format!("{description}\n{line}")
    };

    Ok(Operation {
        name,
        operation_id,
        method,
        path: path.to_string(),
        tags: op
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect(),
        description,
        deprecated: op
            .get("deprecated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        params,
        body,
    })
}

/// Headers owned by the transport / auth layer; never model-controlled.
fn is_reserved_header(location: &str, name: &str) -> bool {
    location == "header"
        && matches!(
            name.to_ascii_lowercase().as_str(),
            "authorization"
                | "proxy-authorization"
                | "cookie"
                | "host"
                | "content-length"
                | "content-type"
                | "accept"
                | "transfer-encoding"
        )
}

pub(crate) fn placeholders(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.push(rest[start + 1..start + end].to_string());
        rest = &rest[start + end + 1..];
    }
    out
}

fn build_body(res: &mut Resolver<'_>, op: &Value) -> Result<Option<BodySpec>, String> {
    let Some(rb) = op.get("requestBody") else {
        return Ok(None);
    };
    let Some(rb) = res.resolve_top(rb) else {
        return Err("unresolvable requestBody $ref".into());
    };
    let required = rb.get("required").and_then(Value::as_bool).unwrap_or(false);
    let content = rb
        .get("content")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let pick = |pred: &dyn Fn(&str) -> bool| {
        content.iter().find(|(ct, _)| {
            pred(
                &ct.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase(),
            )
        })
    };
    let (kind, media) =
        if let Some((_, m)) = pick(&|ct| ct == "application/json" || ct.ends_with("+json")) {
            (BodyKind::Json, m)
        } else if let Some((_, m)) = pick(&|ct| ct == "application/x-www-form-urlencoded") {
            (BodyKind::Form, m)
        } else if let Some((_, m)) = pick(&|ct| ct == "*/*") {
            (BodyKind::Json, m)
        } else {
            let types: Vec<&String> = content.keys().collect();
            return Err(format!("unsupported request body content types {types:?}"));
        };
    let mut schema = media
        .get("schema")
        .map_or_else(|| json!({}), |s| res.resolve_schema(s));
    if let (Some(d), Some(o)) = (
        rb.get("description").and_then(Value::as_str),
        schema.as_object_mut(),
    ) {
        o.entry("description").or_insert_with(|| json!(d));
    }
    Ok(Some(BodySpec {
        kind,
        required,
        schema,
    }))
}

/// Maximum length of a tool input property key (provider schema limit).
const MAX_KEY_LEN: usize = 64;

fn tool_key(name: &str) -> String {
    let k: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_KEY_LEN)
        .collect();
    if k.is_empty() { "param".into() } else { k }
}

/// `base` shortened (ASCII only) so that `base + suffix` fits [`MAX_KEY_LEN`].
fn fit_key(base: &str, suffix: &str) -> String {
    let room = MAX_KEY_LEN.saturating_sub(suffix.len());
    format!("{}{suffix}", &base[..base.len().min(room)])
}

fn bind_params(
    res: &mut Resolver<'_>,
    raw: &[(String, String, Value)],
    has_body: bool,
) -> Vec<ParamBinding> {
    let keys: Vec<String> = raw.iter().map(|(_, n, _)| tool_key(n)).collect();
    let location_of = |loc: &str| match loc {
        "path" => ParamLocation::Path,
        "query" => ParamLocation::Query,
        _ => ParamLocation::Header,
    };
    let needs_prefix: Vec<bool> = keys
        .iter()
        .map(|k| keys.iter().filter(|o| *o == k).count() > 1 || (has_body && k == "body"))
        .collect();
    // Unprefixed keys are final; prefixed ones must not collide with them.
    let mut used: HashSet<String> = keys
        .iter()
        .zip(&needs_prefix)
        .filter(|(_, p)| !**p)
        .map(|(k, _)| k.clone())
        .collect();
    if has_body {
        used.insert("body".into());
    }
    let mut out = Vec::new();
    for (i, (loc, name, p)) in raw.iter().enumerate() {
        let location = location_of(loc);
        let mut key = keys[i].clone();
        if needs_prefix[i] {
            let base = format!("{}_{key}", location.label());
            key = fit_key(&base, "");
            let mut n = 2;
            while !used.insert(key.clone()) {
                key = fit_key(&base, &format!("_{n}"));
                n += 1;
            }
        }
        let mut schema = p
            .get("schema")
            .map_or_else(|| json!({}), |s| res.resolve_schema(s));
        if let (Some(d), Some(o)) = (
            p.get("description").and_then(Value::as_str),
            schema.as_object_mut(),
        ) {
            o.entry("description").or_insert_with(|| json!(d));
        }
        let style = p.get("style").and_then(Value::as_str);
        let default_explode = location == ParamLocation::Query && style != Some("spaceDelimited");
        out.push(ParamBinding {
            key,
            name: name.clone(),
            location,
            required: location == ParamLocation::Path
                || p.get("required").and_then(Value::as_bool).unwrap_or(false),
            explode: p
                .get("explode")
                .and_then(Value::as_bool)
                .unwrap_or(default_explode),
            schema,
        });
    }
    out
}

/// Resolves local `$ref`s into self-contained schemas.
struct Resolver<'a> {
    root: &'a Value,
    warnings: Vec<String>,
    nodes: usize,
}

impl<'a> Resolver<'a> {
    fn new(root: &'a Value) -> Self {
        Self {
            root,
            warnings: Vec::new(),
            nodes: 0,
        }
    }

    fn warn(&mut self, msg: String) {
        if self.warnings.len() < 50 && !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    fn lookup(&self, reference: &str) -> Option<&'a Value> {
        let ptr = reference.strip_prefix('#')?;
        self.root.pointer(ptr)
    }

    /// Follow a top-level `$ref` chain (path items, parameters, bodies).
    fn resolve_top(&mut self, v: &Value) -> Option<Value> {
        let mut cur = v.clone();
        for _ in 0..MAX_REF_DEPTH {
            let Some(r) = cur.get("$ref").and_then(Value::as_str).map(str::to_string) else {
                return Some(cur);
            };
            let Some(target) = self.lookup(&r) else {
                self.warn(format!("cannot resolve $ref `{r}`"));
                return None;
            };
            cur = target.clone();
        }
        self.warn("$ref chain too deep".into());
        None
    }

    fn resolve_schema(&mut self, v: &Value) -> Value {
        let mut stack = Vec::new();
        self.walk(v, &mut stack, false)
    }

    fn walk(&mut self, v: &Value, stack: &mut Vec<String>, name_map: bool) -> Value {
        self.nodes += 1;
        if self.nodes > NODE_BUDGET || stack.len() > MAX_REF_DEPTH {
            self.warn("schema too large or deep; truncated".into());
            return json!({});
        }
        match v {
            Value::Array(a) => Value::Array(a.iter().map(|x| self.walk(x, stack, false)).collect()),
            Value::Object(o) => {
                if name_map {
                    // Keys are property names, values are schemas.
                    return Value::Object(
                        o.iter()
                            .map(|(k, x)| (k.clone(), self.walk(x, stack, false)))
                            .collect(),
                    );
                }
                if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                    return self.walk_ref(r, o, stack);
                }
                let mut out = Map::new();
                for (k, x) in o {
                    if matches!(
                        k.as_str(),
                        "xml" | "externalDocs" | "discriminator" | "example" | "examples"
                    ) {
                        continue;
                    }
                    let child = self.walk(
                        x,
                        stack,
                        matches!(k.as_str(), "properties" | "patternProperties" | "$defs"),
                    );
                    out.insert(k.clone(), child);
                }
                // OpenAPI 3.0 `nullable` -> JSON Schema type union.
                let nullable = out.remove("nullable") == Some(json!(true));
                if nullable && let Some(Value::String(t)) = out.get("type").cloned() {
                    out.insert("type".into(), json!([t, "null"]));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }

    fn walk_ref(
        &mut self,
        r: &str,
        siblings: &Map<String, Value>,
        stack: &mut Vec<String>,
    ) -> Value {
        if stack.iter().any(|s| s == r) {
            return json!({
                "type": "object",
                "description": format!("(recursive reference to {r})")
            });
        }
        let Some(target) = self.lookup(r) else {
            self.warn(if r.starts_with('#') {
                format!("cannot resolve $ref `{r}`")
            } else {
                format!("external $ref `{r}` is not followed")
            });
            return json!({});
        };
        stack.push(r.to_string());
        let mut resolved = self.walk(target, stack, false);
        stack.pop();
        if let Some(o) = resolved.as_object_mut() {
            for (k, x) in siblings {
                if k != "$ref" {
                    let child = self.walk(x, stack, false);
                    o.insert(k.clone(), child);
                }
            }
        }
        resolved
    }
}
