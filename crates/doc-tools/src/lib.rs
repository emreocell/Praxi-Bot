//! Document and data tools for the agent: PDF, DOCX, XLSX, CSV and JSON.
//!
//! Tools (all permission category `Fs`):
//!
//! | tool            | effect     | what it does                                       |
//! |-----------------|------------|----------------------------------------------------|
//! | `pdf_extract`   | read       | text per page, best-effort tables                  |
//! | `docx_read`     | read       | headings, paragraphs and tables of a `.docx`       |
//! | `docx_write`    | write      | create a `.docx` from headings/paragraphs/tables   |
//! | `xlsx_read`     | read       | sheets, ranges and typed cells (`.xlsx/.xlsb/.xls`) |
//! | `xlsx_write`    | write      | create an `.xlsx` with typed cells and formulas    |
//! | `csv_read`      | read       | paged CSV/TSV reading                              |
//! | `csv_transform` | read/write | filter / select / sort / group / limit pipeline    |
//! | `json_query`    | read       | JSONPath (RFC 9535) query over a file or inline    |
//!
//! Every tool returns two content parts: a compact markdown preview for the
//! model (`ContentPart::Text`) and the structured result (`ContentPart::Json`).
//!
//! # Permissions
//!
//! File paths are always passed in the `path` (and, for `csv_transform`, the
//! optional `output_path`) input fields, exactly like the `fs_*` tools of
//! `builtin-tools`. These tools do not re-implement scope checks; the caller
//! (agent loop / permission engine) gates them by `category` + `effect` and the
//! path fields before `call`. Write tools never overwrite an existing file
//! unless `overwrite: true` is given. Inputs larger than
//! [`DocToolsConfig::max_input_bytes`] are rejected.
//!
//! # Wiring
//!
//! ```ignore
//! doc_tools::register(&registry, &doc_tools::DocToolsConfig::default());
//! ```

// Flat declarative tool tables and dispatchers; short names (r, c, i) are idiomatic here.
#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::doc_markdown
)]

mod common;
mod csv_tools;
mod docx;
mod json_query;
mod pdf;
mod xlsx;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tool_registry::{
    Effect, PermissionCategory, Platform, Tool, ToolContext, ToolError, ToolOutput, ToolRegistry,
    ToolSpec,
};

pub use common::Reply;

/// Limits applied by every doc tool.
#[derive(Debug, Clone)]
pub struct DocToolsConfig {
    /// Largest input file accepted, in bytes (default 25 MiB).
    pub max_input_bytes: u64,
    /// Most rows / items placed in the structured JSON result (default 500).
    pub max_rows: usize,
    /// Longest markdown preview in characters (default 6000).
    pub max_preview_chars: usize,
    /// Longest single cell / paragraph kept in structured output (default 4000).
    pub max_cell_chars: usize,
    /// Largest written output (docx/xlsx/csv), in bytes (default 25 MiB).
    /// Also bounds DOCX read JSON, which has an additional hard 1 MiB cap.
    pub max_output_bytes: usize,
}

impl Default for DocToolsConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 25 * 1024 * 1024,
            max_rows: 500,
            max_preview_chars: 6000,
            max_cell_chars: 4000,
            max_output_bytes: 25 * 1024 * 1024,
        }
    }
}

type Handler = fn(&DocToolsConfig, Value) -> Result<Reply, ToolError>;

struct DocTool {
    spec: ToolSpec,
    cfg: Arc<DocToolsConfig>,
    handler: Handler,
}

#[async_trait]
impl Tool for DocTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let cfg = self.cfg.clone();
        let handler = self.handler;
        let reply = tokio::task::spawn_blocking(move || handler(&cfg, input))
            .await
            .map_err(|e| ToolError::Execution(format!("doc tool task failed: {e}")))??;
        Ok(reply.into_output())
    }
}

/// Build all document tools.
#[must_use]
pub fn tools(cfg: &DocToolsConfig) -> Vec<Arc<dyn Tool>> {
    let cfg = Arc::new(cfg.clone());
    let defs: Vec<(ToolSpec, Handler)> = vec![
        (pdf::spec(), pdf::run),
        (docx::read_spec(), docx::run_read),
        (docx::write_spec(), docx::run_write),
        (xlsx::read_spec(), xlsx::run_read),
        (xlsx::write_spec(), xlsx::run_write),
        (csv_tools::read_spec(), csv_tools::run_read),
        (csv_tools::transform_spec(), csv_tools::run_transform),
        (json_query::spec(), json_query::run),
    ];
    defs.into_iter()
        .map(|(spec, handler)| {
            Arc::new(DocTool {
                spec,
                cfg: cfg.clone(),
                handler,
            }) as Arc<dyn Tool>
        })
        .collect()
}

/// Register every document tool in `registry`.
pub fn register(registry: &ToolRegistry, cfg: &DocToolsConfig) {
    for tool in tools(cfg) {
        registry.register(tool);
    }
}

/// Names of all tools in this crate.
pub const TOOL_NAMES: [&str; 8] = [
    "pdf_extract",
    "docx_read",
    "docx_write",
    "xlsx_read",
    "xlsx_write",
    "csv_read",
    "csv_transform",
    "json_query",
];

pub(crate) fn mk_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    effect: Effect,
) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        effect,
        category: PermissionCategory::Fs,
        platforms: vec![Platform::All],
        content: match name {
            // Documents can carry prompt injection and are private files.
            "pdf_extract" | "docx_read" | "xlsx_read" | "csv_read" | "csv_transform"
            | "json_query" => tool_registry::ContentClass::untrusted_private(),
            "docx_write" | "xlsx_write" => tool_registry::ContentClass::private_read(),
            _ => tool_registry::ContentClass::unspecified(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_all_tools_with_valid_specs() {
        let reg = ToolRegistry::new();
        register(&reg, &DocToolsConfig::default());
        for name in TOOL_NAMES {
            let t = reg.get(name).unwrap_or_else(|| panic!("{name} missing"));
            assert!(t.spec().is_valid(), "{name}");
        }
        assert_eq!(reg.get("docx_write").unwrap().spec().effect, Effect::Write);
        assert_eq!(reg.get("pdf_extract").unwrap().spec().effect, Effect::Read);
    }
}

#[cfg(test)]
mod call_tests {
    use super::*;
    use serde_json::json;
    use tool_registry::ContentPart;

    #[tokio::test]
    async fn call_returns_markdown_and_json_parts() {
        let reg = ToolRegistry::new();
        register(&reg, &DocToolsConfig::default());
        let tool = reg.get("json_query").unwrap();
        let out = tool
            .call(
                &ToolContext::default(),
                json!({"json": {"a": [1, 2]}, "query": "$.a[*]"}),
            )
            .await
            .unwrap();
        assert!(matches!(&out.content[0], ContentPart::Text { text } if text.contains("2 match")));
        assert!(matches!(&out.content[1], ContentPart::Json { data } if data["total"] == 2));
        let err = tool
            .call(&ToolContext::default(), json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
