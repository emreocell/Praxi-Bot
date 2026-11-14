//! Shared helpers: input parsing, size-limited reads, previews, markdown tables.

use std::fmt::Write as _;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tool_registry::{ContentPart, ToolError, ToolOutput};

use crate::DocToolsConfig;

/// Result of a tool run: a markdown preview and the structured data.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Compact markdown shown to the model.
    pub markdown: String,
    /// Structured result.
    pub data: Value,
}

impl Reply {
    pub(crate) fn new(cfg: &DocToolsConfig, markdown: &str, data: Value) -> Self {
        Self {
            markdown: truncate_chars(markdown, cfg.max_preview_chars),
            data,
        }
    }

    pub(crate) fn into_output(self) -> ToolOutput {
        ToolOutput {
            content: vec![
                ContentPart::Text {
                    text: self.markdown,
                },
                ContentPart::Json { data: self.data },
            ],
            spill: None,
            exposure: None,
        }
    }
}

pub(crate) fn parse<T: DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

pub(crate) fn invalid(msg: impl Into<String>) -> ToolError {
    ToolError::InvalidInput(msg.into())
}

pub(crate) fn exec(msg: impl Into<String>) -> ToolError {
    ToolError::Execution(msg.into())
}

/// Read a file, refusing anything larger than `cfg.max_input_bytes`.
pub(crate) fn read_limited(cfg: &DocToolsConfig, path: &str) -> Result<Vec<u8>, ToolError> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| exec(format!("cannot stat {path}: {e}")))?;
    if !meta.is_file() {
        return Err(exec(format!("{path} is not a file")));
    }
    if meta.len() > cfg.max_input_bytes {
        return Err(exec(format!(
            "{path} is {} bytes, over the {} byte input limit",
            meta.len(),
            cfg.max_input_bytes
        )));
    }
    std::fs::read(p).map_err(|e| exec(format!("cannot read {path}: {e}")))
}

/// Write `bytes` to `path`; fails if it exists unless `overwrite`.
pub(crate) fn write_file(
    cfg: &DocToolsConfig,
    path: &str,
    bytes: &[u8],
    overwrite: bool,
) -> Result<(), ToolError> {
    if bytes.len() > cfg.max_output_bytes {
        return Err(exec(format!(
            "output would be {} bytes, over the {} byte limit",
            bytes.len(),
            cfg.max_output_bytes
        )));
    }
    let p = Path::new(path);
    if p.is_dir() {
        return Err(exec(format!("{path} is a directory")));
    }
    if p.exists() && !overwrite {
        return Err(exec(format!(
            "{path} already exists; pass overwrite:true to replace it"
        )));
    }
    if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| exec(format!("create dir: {e}")))?;
    }
    std::fs::write(p, bytes).map_err(|e| exec(format!("cannot write {path}: {e}")))
}

/// Truncate to at most `max` characters, adding a marker when cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[preview truncated]");
    out
}

/// Truncate a cell to `max` characters (with an ellipsis), no marker line.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn md_cell(s: &str) -> String {
    clip(&s.replace('|', "\\|").replace(['\n', '\r'], " "), 60)
}

/// Render rows as a markdown table. The first row is the header when
/// `header` is true, otherwise generic column names are used.
pub(crate) fn md_table(rows: &[Vec<String>], header: bool, max_rows: usize) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return "_(empty)_\n".to_string();
    }
    let mut out = String::new();
    let (head, body): (Vec<String>, &[Vec<String>]) = if header {
        (rows[0].clone(), &rows[1..])
    } else {
        ((1..=width).map(|i| format!("c{i}")).collect(), rows)
    };
    let cell = |r: &[String], i: usize| r.get(i).map_or_else(String::new, |c| md_cell(c));
    let _ = writeln!(
        out,
        "| {} |",
        (0..width)
            .map(|i| cell(&head, i))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let _ = writeln!(out, "|{}|", vec![" --- "; width].join("|"));
    for r in body.iter().take(max_rows) {
        let _ = writeln!(
            out,
            "| {} |",
            (0..width)
                .map(|i| cell(r, i))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    if body.len() > max_rows {
        let _ = writeln!(out, "\n_…{} more rows_", body.len() - max_rows);
    }
    out
}

/// Parse a 1-based inclusive page spec such as `"1-3,7"` into a sorted,
/// deduplicated list, clamping upper bounds to `limit`.
pub(crate) fn parse_ranges(spec: &str, limit: u32) -> Result<Vec<u32>, ToolError> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let a: u32 = a
            .parse()
            .map_err(|_| invalid(format!("bad range '{part}'")))?;
        let b: u32 = b
            .parse()
            .map_err(|_| invalid(format!("bad range '{part}'")))?;
        if a == 0 || b < a {
            return Err(invalid(format!("bad range '{part}'")));
        }
        out.extend(a..=b.min(limit));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse() {
        assert_eq!(parse_ranges("1-3,7,2", 100).unwrap(), vec![1, 2, 3, 7]);
        assert_eq!(parse_ranges("2-9", 4).unwrap(), vec![2, 3, 4]);
        assert!(parse_ranges("0", 5).is_err());
        assert!(parse_ranges("a", 5).is_err());
    }

    #[test]
    fn table_escapes_pipes() {
        let t = md_table(
            &[vec!["a|b".into(), "c".into()], vec!["1".into(), "2".into()]],
            true,
            10,
        );
        assert!(t.contains("a\\|b"));
        assert!(t.contains("| 1 | 2 |"));
    }

    #[test]
    fn clip_is_char_safe() {
        assert_eq!(clip("ééééé", 2), "éé…");
    }
}
