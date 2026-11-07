//! `pdf_extract`: text per page with best-effort table detection (via `lopdf`).

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, exec, invalid, md_table, parse, parse_ranges, read_limited};

const DEFAULT_MAX_CHARS: usize = 50_000;

#[derive(Deserialize)]
struct In {
    path: String,
    /// Page spec such as "1-3,7" (1-based). Default: all pages.
    pages: Option<String>,
    /// Detect tables (lines whose columns are separated by tabs or 2+ spaces).
    #[serde(default)]
    tables: bool,
    /// Total character budget over all returned pages.
    max_chars: Option<usize>,
}

pub fn spec() -> ToolSpec {
    crate::mk_spec(
        "pdf_extract",
        "Extract text from a PDF, page by page; optional best-effort table detection. Scanned (image-only) PDFs have no text layer and return empty pages. Example: {\"path\":\"report.pdf\",\"pages\":\"1-3\",\"tables\":true}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"PDF file path."},
            "pages":{"type":"string","description":"1-based pages, e.g. \"1-3,7\". Default all."},
            "tables":{"type":"boolean","description":"Also return tables detected from column-aligned lines."},
            "max_chars":{"type":"integer","minimum":1,"description":"Total character budget (default 50000)."}
        },"required":["path"]}),
        Effect::Read,
    )
}

/// Split a line into columns on tabs or runs of 2+ spaces.
fn split_columns(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut cur = String::new();
    let mut spaces = 0usize;
    for ch in line.trim().chars() {
        if ch == '\t' {
            cols.push(std::mem::take(&mut cur));
            spaces = 0;
        } else if ch == ' ' {
            spaces += 1;
            cur.push(ch);
        } else {
            if spaces >= 2 {
                let trimmed = cur.trim_end().to_string();
                cols.push(trimmed);
                cur.clear();
            }
            spaces = 0;
            cur.push(ch);
        }
    }
    cols.push(cur.trim_end().to_string());
    cols.retain(|c| !c.is_empty());
    cols
}

/// Group consecutive multi-column lines (same column count, >= 2 rows) into tables.
pub(crate) fn detect_tables(text: &str) -> Vec<Vec<Vec<String>>> {
    let mut tables = Vec::new();
    let mut cur: Vec<Vec<String>> = Vec::new();
    let flush = |cur: &mut Vec<Vec<String>>, tables: &mut Vec<Vec<Vec<String>>>| {
        if cur.len() >= 2 {
            tables.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for line in text.lines() {
        let cols = split_columns(line);
        if cols.len() >= 2 && cur.first().is_none_or(|r| r.len() == cols.len()) {
            cur.push(cols);
        } else {
            flush(&mut cur, &mut tables);
            if cols.len() >= 2 {
                cur.push(cols);
            }
        }
    }
    flush(&mut cur, &mut tables);
    tables
}

pub fn run(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: In = parse(input)?;
    let bytes = read_limited(cfg, &i.path)?;
    let doc = lopdf::Document::load_mem(&bytes)
        .map_err(|e| exec(format!("cannot parse PDF {}: {e}", i.path)))?;
    if doc.is_encrypted() {
        return Err(exec(format!("{} is encrypted; decrypt it first", i.path)));
    }
    let all: Vec<u32> = doc.get_pages().keys().copied().collect();
    let total_pages = all.len();
    let wanted: Vec<u32> = match &i.pages {
        Some(spec) => {
            let max = u32::try_from(total_pages).unwrap_or(u32::MAX);
            let sel = parse_ranges(spec, max)?;
            if sel.is_empty() {
                return Err(invalid(format!(
                    "no page in '{spec}'; PDF has {total_pages} pages"
                )));
            }
            sel
        }
        None => all,
    };
    let mut budget = i.max_chars.unwrap_or(DEFAULT_MAX_CHARS);
    let mut truncated = false;
    let mut pages_json = Vec::new();
    let mut md = format!("# {} ({total_pages} pages)\n", i.path);
    for n in wanted {
        let (text, extract_err) = match doc.extract_text(&[n]) {
            Ok(t) => (t, None),
            Err(e) => (String::new(), Some(e.to_string())),
        };
        let text = text.trim().to_string();
        let chars = text.chars().count();
        let shown: String = if chars > budget {
            truncated = true;
            text.chars().take(budget).collect()
        } else {
            text.clone()
        };
        budget = budget.saturating_sub(shown.chars().count());
        let mut page = json!({"page": n, "chars": chars, "text": shown});
        if let Some(e) = &extract_err {
            page["error"] = json!(e);
        }
        let _ = write!(
            md,
            "\n## Page {n}\n\n{}\n",
            if let Some(e) = &extract_err {
                format!("_(text extraction failed: {e})_")
            } else if shown.is_empty() {
                "_(no text layer)_".to_string()
            } else {
                shown.clone()
            }
        );
        if i.tables {
            let tables = detect_tables(&text);
            for (ti, t) in tables.iter().enumerate() {
                let _ = write!(md, "\n_Table {}_\n\n{}", ti + 1, md_table(t, true, 30));
            }
            page["tables"] = json!(tables);
        }
        pages_json.push(page);
        if budget == 0 {
            truncated = true;
            break;
        }
    }
    if truncated {
        md.push_str("\n_…text truncated by max_chars_\n");
    }
    let data = json!({"path": i.path, "total_pages": total_pages, "pages": pages_json, "truncated": truncated});
    Ok(Reply::new(cfg, &md, data))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    /// Build a PDF with one text line per entry; each entry is one page.
    pub(crate) fn make_pdf(pages: &[&[&str]]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut kids = Vec::new();
        for lines in pages {
            let mut ops = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("TL", vec![14.into()]),
                Operation::new("Td", vec![50.into(), 700.into()]),
            ];
            for l in *lines {
                ops.push(Operation::new("Tj", vec![Object::string_literal(*l)]));
                ops.push(Operation::new("T*", vec![]));
            }
            ops.push(Operation::new("ET", vec![]));
            let content = Content { operations: ops };
            let cid = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let pid = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => cid,
            });
            kids.push(pid.into());
        }
        let count = i64::try_from(kids.len()).unwrap();
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => count,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    #[test]
    fn extracts_pages_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.pdf");
        std::fs::write(&p, make_pdf(&[&["Hello world"], &["Second page text"]])).unwrap();
        let cfg = DocToolsConfig::default();
        let r = run(&cfg, json!({"path": p.to_str().unwrap()})).unwrap();
        assert_eq!(r.data["total_pages"], 2);
        assert!(
            r.data["pages"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Hello world")
        );
        assert!(r.markdown.contains("## Page 2"));
        let r = run(&cfg, json!({"path": p.to_str().unwrap(), "pages": "2"})).unwrap();
        assert_eq!(r.data["pages"].as_array().unwrap().len(), 1);
        assert!(
            r.data["pages"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Second")
        );
    }

    #[test]
    fn max_chars_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.pdf");
        std::fs::write(&p, make_pdf(&[&["Hello world and more"]])).unwrap();
        let r = run(
            &DocToolsConfig::default(),
            json!({"path": p.to_str().unwrap(), "max_chars": 5}),
        )
        .unwrap();
        assert_eq!(r.data["truncated"], true);
        assert_eq!(
            r.data["pages"][0]["text"].as_str().unwrap().chars().count(),
            5
        );
    }

    #[test]
    fn rejects_garbage_and_oversize() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.pdf");
        std::fs::write(&p, b"not a pdf").unwrap();
        let cfg = DocToolsConfig::default();
        assert!(run(&cfg, json!({"path": p.to_str().unwrap()})).is_err());
        let small = DocToolsConfig {
            max_input_bytes: 4,
            ..cfg
        };
        let e = run(&small, json!({"path": p.to_str().unwrap()})).unwrap_err();
        assert!(e.to_string().contains("input limit"));
    }

    #[test]
    fn table_heuristic() {
        let text =
            "Intro line\nName    Qty    Price\nApple   3      1.50\nPear    5      2.00\nfooter";
        let t = detect_tables(text);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].len(), 3);
        assert_eq!(t[0][1], vec!["Apple", "3", "1.50"]);
    }
}
