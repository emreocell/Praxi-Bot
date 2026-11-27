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
    /// Base64 PNG pages returned inline (previews only).
    pub(crate) images: Vec<String>,
}

impl Reply {
    pub(crate) fn new(cfg: &DocToolsConfig, markdown: &str, data: Value) -> Self {
        Self {
            markdown: truncate_chars(markdown, cfg.max_preview_chars),
            data,
            images: Vec::new(),
        }
    }

    pub(crate) fn into_output(self) -> ToolOutput {
        let mut content = vec![
            ContentPart::Text {
                text: self.markdown,
            },
            ContentPart::Json { data: self.data },
        ];
        content.extend(self.images.into_iter().map(|base64| ContentPart::Binary {
            mime_type: "image/png".into(),
            base64,
        }));
        ToolOutput {
            content,
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
    if let Some(guard) = &cfg.write_guard {
        // The path first (nothing is stat-ed before it is vetted), then what the write adds:
        // its size less the file it replaces.
        guard.check(path).map_err(exec)?;
        let existing = std::fs::metadata(p)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map_or(0, |m| m.len());
        guard
            .check_sized(path, (bytes.len() as u64).saturating_sub(existing))
            .map_err(exec)?;
    }
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

/// Replace the existing file `path` with `bytes`: same checks as [`write_file`] with
/// `overwrite`, but the bytes go to a sibling temp file that is renamed over the original
/// (keeping its permissions), so an interrupted write never leaves a half-written document.
pub(crate) fn replace_file(
    cfg: &DocToolsConfig,
    path: &str,
    bytes: &[u8],
) -> Result<(), ToolError> {
    use std::io::Write as _;
    if bytes.len() > cfg.max_output_bytes {
        return Err(exec(format!(
            "output would be {} bytes, over the {} byte limit",
            bytes.len(),
            cfg.max_output_bytes
        )));
    }
    let p = Path::new(path);
    if let Some(guard) = &cfg.write_guard {
        guard.check(path).map_err(exec)?;
    }
    let meta = std::fs::metadata(p).map_err(|e| exec(format!("cannot stat {path}: {e}")))?;
    if !meta.is_file() {
        return Err(exec(format!("{path} is not a file")));
    }
    if let Some(guard) = &cfg.write_guard {
        guard
            .check_sized(path, (bytes.len() as u64).saturating_sub(meta.len()))
            .map_err(exec)?;
    }
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| exec(format!("cannot write next to {path}: {e}")))?;
    tmp.write_all(bytes)
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| exec(format!("cannot write {path}: {e}")))?;
    std::fs::set_permissions(tmp.path(), meta.permissions())
        .map_err(|e| exec(format!("cannot write {path}: {e}")))?;
    tmp.persist(p)
        .map_err(|e| exec(format!("cannot replace {path}: {}", e.error)))?;
    Ok(())
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

/// Most distinct items a range spec may select (after clamping to the caller's limit).
pub(crate) const MAX_RANGE_ITEMS: u32 = 100_000;

/// Parse a 1-based inclusive page spec such as `"1-3,7"` into a sorted,
/// deduplicated list, clamping upper bounds to `limit`. A selection of more than
/// [`MAX_RANGE_ITEMS`] distinct items is an error: the spans are merged and counted before
/// anything is expanded, so `1-4294967295` costs nothing.
pub(crate) fn parse_ranges(spec: &str, limit: u32) -> Result<Vec<u32>, ToolError> {
    let mut spans: Vec<(u32, u32)> = Vec::new();
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
        let b = b.min(limit);
        if a <= b {
            spans.push((a, b));
        }
    }
    spans.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some((_, end)) if a <= end.saturating_add(1) => *end = (*end).max(b),
            _ => merged.push((a, b)),
        }
    }
    let total: u64 = merged.iter().map(|(a, b)| u64::from(b - a) + 1).sum();
    if total > u64::from(MAX_RANGE_ITEMS) {
        return Err(invalid(format!(
            "the selection covers {total} items; ask for at most {MAX_RANGE_ITEMS} at a time"
        )));
    }
    Ok(merged.into_iter().flat_map(|(a, b)| a..=b).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guard that remembers the sizes it was asked about and refuses writes over `limit`.
    fn sized_guard(
        limit: u64,
    ) -> (
        crate::WriteGuard,
        std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let guard = crate::WriteGuard::sized(move |_, added| {
            log.lock().unwrap().push(added);
            if added > limit {
                return Err(format!("{added} bytes is over the {limit} byte limit"));
            }
            Ok(())
        });
        (guard, seen)
    }

    #[test]
    fn writers_tell_the_guard_what_they_really_add_and_stop_when_it_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.bin");
        let path = path.to_str().unwrap();
        let (guard, seen) = sized_guard(100);
        let cfg = DocToolsConfig {
            write_guard: Some(guard),
            ..DocToolsConfig::default()
        };
        // A new file adds all of its bytes; one that replaces another adds the difference, and
        // a smaller replacement adds nothing.
        write_file(&cfg, path, &[7; 100], false).unwrap();
        write_file(&cfg, path, &[7; 250], true).unwrap_err();
        write_file(&cfg, path, &[7; 120], true).unwrap();
        replace_file(&cfg, path, &[7; 60]).unwrap();
        let seen = seen.lock().unwrap().clone();
        assert!(seen.contains(&100) && seen.contains(&20) && seen.contains(&150));
        assert_eq!(seen.iter().max(), Some(&150), "{seen:?}");
        // A refused write left the file as it was.
        assert_eq!(std::fs::read(path).unwrap(), vec![7; 60]);
        // A plain path guard still works and is never given a size.
        let cfg = DocToolsConfig {
            write_guard: Some(crate::WriteGuard::new(|_| Err("no".into()))),
            ..DocToolsConfig::default()
        };
        assert!(write_file(&cfg, path, &[1; 10], true).is_err());
        assert_eq!(std::fs::read(path).unwrap(), vec![7; 60]);
    }

    #[test]
    fn ranges_parse() {
        assert_eq!(parse_ranges("1-3,7,2", 100).unwrap(), vec![1, 2, 3, 7]);
        assert_eq!(parse_ranges("2-9", 4).unwrap(), vec![2, 3, 4]);
        assert!(parse_ranges("0", 5).is_err());
        assert!(parse_ranges("a", 5).is_err());
    }

    #[test]
    fn a_huge_range_is_refused_not_expanded() {
        // `1-4294967295` once expanded into a 16 GiB vector and aborted the process.
        assert!(parse_ranges("1-4294967295", u32::MAX).is_err());
        assert!(parse_ranges("1-200000", u32::MAX).is_err());
        let max = MAX_RANGE_ITEMS as usize;
        assert_eq!(
            parse_ranges(&format!("1-{max}"), u32::MAX).unwrap().len(),
            max
        );
        assert!(parse_ranges(&format!("1-{}", max + 1), u32::MAX).is_err());
    }

    #[test]
    fn the_cap_counts_distinct_items_across_every_part_of_the_spec() {
        // Disjoint parts add up; overlapping and repeated parts do not.
        assert!(parse_ranges("1-60000,70000-130000", u32::MAX).is_err());
        let repeated = vec!["1-4294967295"; 1000].join(",");
        assert!(parse_ranges(&repeated, u32::MAX).is_err());
        let same = vec!["1-100000"; 50_000].join(",");
        assert_eq!(parse_ranges(&same, u32::MAX).unwrap().len(), 100_000);
        assert_eq!(
            parse_ranges("1-5,3-8,9", 100).unwrap(),
            (1..=9).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ranges_past_the_limit_select_nothing_without_expanding() {
        let none = Vec::<u32>::new();
        assert_eq!(parse_ranges("5-4294967295", 3).unwrap(), none);
        assert_eq!(parse_ranges("4000000000-4294967295", 10).unwrap(), none);
        assert_eq!(parse_ranges("1-4294967295", 4).unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(parse_ranges("1-5", 0).unwrap(), none);
        // Still rejected as malformed, whatever the limit.
        assert!(parse_ranges("0-5", u32::MAX).is_err());
        assert!(parse_ranges("9-3", u32::MAX).is_err());
        assert!(parse_ranges("1-x", u32::MAX).is_err());
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
