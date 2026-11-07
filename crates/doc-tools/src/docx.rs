//! `docx_read` / `docx_write`: headings, paragraphs and simple tables.
//!
//! Implemented directly on `zip` + `quick-xml` (WordprocessingML subset), so no
//! heavy document dependency is needed.

use std::fmt::Write as _;
use std::io::{Cursor, Read, Write};

use quick_xml::Reader;
use quick_xml::escape::escape;
use quick_xml::events::{BytesStart, Event};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};
use zip::write::SimpleFileOptions;

use crate::DocToolsConfig;
use crate::common::{Reply, clip, exec, invalid, md_table, parse, read_limited, write_file};

// ---------------------------------------------------------------- reading

#[derive(Deserialize)]
struct ReadIn {
    path: String,
}

pub fn read_spec() -> ToolSpec {
    crate::mk_spec(
        "docx_read",
        "Read a .docx: headings (with level), paragraphs and tables in document order. Example: {\"path\":\"report.docx\"}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":".docx file path."}
        },"required":["path"]}),
        Effect::Read,
    )
}

/// One parsed document block.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    Heading(u8, String),
    Paragraph(String),
    Table(Vec<Vec<String>>),
}

fn attr_val(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == key)
        .map(|a| a.value.to_string())
}

fn heading_level(style: &str) -> Option<u8> {
    let s = style.to_ascii_lowercase();
    if s == "title" {
        return Some(1);
    }
    let rest = s.strip_prefix("heading")?.trim();
    rest.parse::<u8>().ok().filter(|n| (1..=9).contains(n))
}

/// Parse `word/document.xml` into blocks.
pub(crate) fn parse_document_xml(xml: &[u8]) -> Result<Vec<Block>, ToolError> {
    let mut reader = Reader::from_reader(xml);
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut style: Option<String> = None;
    let mut in_p = false;
    let mut in_t = false;
    let mut tbl_depth = 0usize;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| exec(format!("invalid document.xml: {e}")))?;
        match ev {
            Event::Eof => break,
            Event::Empty(e) => match e.local_name().as_ref() {
                "pStyle" if in_p => style = attr_val(&e, "val"),
                "tab" if in_p => para.push('\t'),
                "br" | "cr" if in_p => para.push('\n'),
                _ => {}
            },
            Event::Start(e) => match e.local_name().as_ref() {
                "p" => {
                    in_p = true;
                    para.clear();
                    style = None;
                }
                "pStyle" if in_p => style = attr_val(&e, "val"),
                "t" if in_p => in_t = true,
                "tab" if in_p => para.push('\t'),
                "br" | "cr" if in_p => para.push('\n'),
                "tbl" => {
                    tbl_depth += 1;
                    if tbl_depth == 1 {
                        rows.clear();
                    }
                }
                "tr" if tbl_depth == 1 => row.clear(),
                "tc" if tbl_depth == 1 => cell.clear(),
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "p" => {
                    in_p = false;
                    let text = std::mem::take(&mut para);
                    if tbl_depth > 0 {
                        if !cell.is_empty() && !text.is_empty() {
                            cell.push('\n');
                        }
                        cell.push_str(&text);
                    } else if !text.trim().is_empty() {
                        match style.as_deref().and_then(heading_level) {
                            Some(l) => blocks.push(Block::Heading(l, text)),
                            None => blocks.push(Block::Paragraph(text)),
                        }
                    }
                }
                "tc" if tbl_depth == 1 => row.push(std::mem::take(&mut cell)),
                "tr" if tbl_depth == 1 => rows.push(std::mem::take(&mut row)),
                "tbl" => {
                    tbl_depth = tbl_depth.saturating_sub(1);
                    if tbl_depth == 0 {
                        blocks.push(Block::Table(std::mem::take(&mut rows)));
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_t => {
                let s: &str = &t;
                para.push_str(s);
            }
            Event::GeneralRef(r) if in_t => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    para.push(c);
                } else {
                    let name: &str = &r;
                    match name {
                        "amp" => para.push('&'),
                        "lt" => para.push('<'),
                        "gt" => para.push('>'),
                        "quot" => para.push('"'),
                        "apos" => para.push('\''),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(blocks)
}

fn read_zip_entry(
    zip: &mut zip::ZipArchive<Cursor<Vec<u8>>>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, ToolError> {
    let f = zip
        .by_name(name)
        .map_err(|e| exec(format!("missing {name} in docx: {e}")))?;
    let mut buf = Vec::new();
    f.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| exec(format!("cannot read {name}: {e}")))?;
    if buf.len() as u64 > limit {
        return Err(exec(format!("{name} expands beyond the size limit")));
    }
    Ok(buf)
}

pub fn run_read(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: ReadIn = parse(input)?;
    let bytes = read_limited(cfg, &i.path)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| exec(format!("{} is not a valid .docx (zip): {e}", i.path)))?;
    let xml = read_zip_entry(
        &mut zip,
        "word/document.xml",
        cfg.max_input_bytes.saturating_mul(4),
    )?;
    let blocks = parse_document_xml(&xml)?;
    let total = blocks.len();
    let (mut md, mut out) = (String::new(), Vec::new());
    let (mut headings, mut paragraphs, mut tables) = (0, 0, 0);
    for b in &blocks {
        match b {
            Block::Heading(..) => headings += 1,
            Block::Paragraph(_) => paragraphs += 1,
            Block::Table(_) => tables += 1,
        }
    }
    for b in blocks.iter().take(cfg.max_rows) {
        match b {
            Block::Heading(l, t) => {
                let _ = write!(md, "{} {t}\n\n", "#".repeat(usize::from(*l)));
                out.push(json!({"type":"heading","level":l,"text":clip(t, cfg.max_cell_chars)}));
            }
            Block::Paragraph(t) => {
                let _ = write!(md, "{t}\n\n");
                out.push(json!({"type":"paragraph","text":clip(t, cfg.max_cell_chars)}));
            }
            Block::Table(rows) => {
                md.push_str(&md_table(rows, true, 30));
                md.push('\n');
                let rows: Vec<Vec<String>> = rows
                    .iter()
                    .map(|r| r.iter().map(|c| clip(c, cfg.max_cell_chars)).collect())
                    .collect();
                out.push(json!({"type":"table","rows":rows}));
            }
        }
    }
    let truncated = total > cfg.max_rows;
    if truncated {
        let _ = writeln!(md, "_…{} more blocks_", total - cfg.max_rows);
    }
    let data = json!({"path": i.path, "headings": headings, "paragraphs": paragraphs,
        "tables": tables, "total_blocks": total, "truncated": truncated, "blocks": out});
    Ok(Reply::new(cfg, &md, data))
}

// ---------------------------------------------------------------- writing

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WBlock {
    Heading {
        text: String,
        #[serde(default = "one")]
        level: u8,
    },
    Paragraph {
        text: String,
        #[serde(default)]
        bold: bool,
    },
    Bullets {
        items: Vec<String>,
    },
    Table {
        rows: Vec<Vec<String>>,
        #[serde(default = "yes")]
        header: bool,
    },
}

fn one() -> u8 {
    1
}
fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct WriteIn {
    path: String,
    blocks: Vec<WBlock>,
    #[serde(default)]
    overwrite: bool,
}

pub fn write_spec() -> ToolSpec {
    crate::mk_spec(
        "docx_write",
        "Create a .docx from blocks: heading {level 1-6,text}, paragraph {text,bold?}, bullets {items}, table {rows,header?}. Fails if the file exists unless overwrite. Example: {\"path\":\"out.docx\",\"blocks\":[{\"type\":\"heading\",\"level\":1,\"text\":\"Report\"},{\"type\":\"paragraph\",\"text\":\"Hello\"},{\"type\":\"table\",\"rows\":[[\"a\",\"b\"],[\"1\",\"2\"]]}]}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Output .docx path."},
            "blocks":{"type":"array","description":"Blocks in order.","items":{"type":"object","properties":{
                "type":{"type":"string","enum":["heading","paragraph","bullets","table"]},
                "level":{"type":"integer","minimum":1,"maximum":6},
                "text":{"type":"string"},
                "bold":{"type":"boolean"},
                "items":{"type":"array","items":{"type":"string"}},
                "rows":{"type":"array","items":{"type":"array","items":{"type":"string"}}},
                "header":{"type":"boolean","description":"Bold the first table row (default true)."}
            },"required":["type"]}},
            "overwrite":{"type":"boolean","description":"Replace an existing file."}
        },"required":["path","blocks"]}),
        Effect::Write,
    )
}

fn run_xml(text: &str, bold: bool) -> String {
    // XML 1.0 forbids control characters other than tab and newline.
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}'))
        .collect();
    let text = text.as_str();
    // Preserve tabs and line breaks as WordprocessingML elements.
    let mut out = String::from("<w:r>");
    if bold {
        out.push_str("<w:rPr><w:b/></w:rPr>");
    }
    for (li, line) in text.split('\n').enumerate() {
        if li > 0 {
            out.push_str("<w:br/>");
        }
        for (ti, seg) in line.split('\t').enumerate() {
            if ti > 0 {
                out.push_str("<w:tab/>");
            }
            if !seg.is_empty() {
                let _ = write!(out, "<w:t xml:space=\"preserve\">{}</w:t>", escape(seg));
            }
        }
    }
    out.push_str("</w:r>");
    out
}

fn para_xml(style: Option<&str>, text: &str, bold: bool) -> String {
    let ppr = style.map_or_else(String::new, |s| {
        format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>")
    });
    format!("<w:p>{ppr}{}</w:p>", run_xml(text, bold))
}

fn table_xml(rows: &[Vec<String>], header: bool) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut out = String::from(
        "<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr><w:tblGrid>",
    );
    for _ in 0..width {
        out.push_str("<w:gridCol w:w=\"2000\"/>");
    }
    out.push_str("</w:tblGrid>");
    for (ri, r) in rows.iter().enumerate() {
        out.push_str("<w:tr>");
        for ci in 0..width {
            let text = r.get(ci).map_or("", String::as_str);
            let _ = write!(
                out,
                "<w:tc><w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/></w:tcPr>{}</w:tc>",
                para_xml(None, text, header && ri == 0)
            );
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
    out
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

fn styles_xml() -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
    );
    for n in 1..=6u32 {
        let size = 40u32.saturating_sub(n * 4).max(24);
        let _ = write!(
            s,
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{n}\"><w:name w:val=\"heading {n}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{size}\"/></w:rPr></w:style>",
            n - 1
        );
    }
    s.push_str(r#"<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style></w:styles>"#);
    s
}

fn build_docx(blocks: &[WBlock]) -> Result<Vec<u8>, ToolError> {
    let mut body = String::new();
    for b in blocks {
        match b {
            WBlock::Heading { text, level } => {
                let l = (*level).clamp(1, 6);
                body.push_str(&para_xml(Some(&format!("Heading{l}")), text, false));
            }
            WBlock::Paragraph { text, bold } => body.push_str(&para_xml(None, text, *bold)),
            WBlock::Bullets { items } => {
                for it in items {
                    body.push_str(&para_xml(None, &format!("\u{2022} {it}"), false));
                }
            }
            WBlock::Table { rows, header } => {
                if rows.is_empty() {
                    return Err(invalid("table needs at least one row"));
                }
                body.push_str(&table_xml(rows, *header));
                // Word requires a paragraph between adjacent tables / after the last table.
                body.push_str("<w:p/>");
            }
        }
    }
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default();
    let parts: [(&str, String); 5] = [
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", ROOT_RELS.to_string()),
        ("word/_rels/document.xml.rels", DOC_RELS.to_string()),
        ("word/styles.xml", styles_xml()),
        ("word/document.xml", document),
    ];
    for (name, content) in parts {
        zip.start_file(name, opts)
            .map_err(|e| exec(format!("zip error: {e}")))?;
        zip.write_all(content.as_bytes())
            .map_err(|e| exec(format!("zip error: {e}")))?;
    }
    let cur = zip.finish().map_err(|e| exec(format!("zip error: {e}")))?;
    Ok(cur.into_inner())
}

pub fn run_write(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: WriteIn = parse(input)?;
    if i.blocks.is_empty() {
        return Err(invalid("blocks must not be empty"));
    }
    let bytes = build_docx(&i.blocks)?;
    write_file(cfg, &i.path, &bytes, i.overwrite)?;
    let count = |f: fn(&WBlock) -> bool| i.blocks.iter().filter(|b| f(b)).count();
    let headings = count(|b| matches!(b, WBlock::Heading { .. }));
    let tables = count(|b| matches!(b, WBlock::Table { .. }));
    let md = format!(
        "Wrote {} ({} bytes): {} blocks, {headings} headings, {tables} tables.\n",
        i.path,
        bytes.len(),
        i.blocks.len()
    );
    let data = json!({"path": i.path, "bytes": bytes.len(), "blocks": i.blocks.len(),
        "headings": headings, "tables": tables});
    Ok(Reply::new(cfg, &md, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_stripped() {
        let x = run_xml("a\u{0}b\u{1b}c\td", false);
        assert!(!x.contains('\u{0}') && !x.contains('\u{1b}'));
        assert!(x.contains("abc") && x.contains("<w:tab/>"));
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.docx");
        let path = p.to_str().unwrap();
        let cfg = DocToolsConfig::default();
        let w = run_write(
            &cfg,
            json!({"path": path, "blocks": [
                {"type":"heading","level":2,"text":"Q&A <1>"},
                {"type":"paragraph","text":"line one\nline two\tTabbed","bold":true},
                {"type":"bullets","items":["x","y"]},
                {"type":"table","rows":[["Name","Qty"],["Apple","3"],["Pear"]]}
            ]}),
        )
        .unwrap();
        assert_eq!(w.data["tables"], 1);
        let r = run_read(&cfg, json!({"path": path})).unwrap();
        let blocks = r.data["blocks"].as_array().unwrap();
        assert_eq!(
            blocks[0],
            json!({"type":"heading","level":2,"text":"Q&A <1>"})
        );
        assert_eq!(blocks[1]["text"], "line one\nline two\tTabbed");
        assert_eq!(blocks[2]["text"], "\u{2022} x");
        let table = blocks.iter().find(|b| b["type"] == "table").unwrap();
        assert_eq!(table["rows"][1], json!(["Apple", "3"]));
        assert_eq!(table["rows"][2], json!(["Pear", ""]));
        assert!(r.markdown.starts_with("## Q&A <1>"));
        assert_eq!(r.data["headings"], 1);
    }

    #[test]
    fn refuses_overwrite_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.docx");
        let input =
            json!({"path": p.to_str().unwrap(), "blocks": [{"type":"paragraph","text":"a"}]});
        let cfg = DocToolsConfig::default();
        run_write(&cfg, input.clone()).unwrap();
        assert!(run_write(&cfg, input.clone()).is_err());
        let mut again = input;
        again["overwrite"] = json!(true);
        run_write(&cfg, again).unwrap();
    }

    #[test]
    fn rejects_non_docx_and_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.docx");
        std::fs::write(&p, b"plain").unwrap();
        let cfg = DocToolsConfig::default();
        assert!(run_read(&cfg, json!({"path": p.to_str().unwrap()})).is_err());
        assert!(run_write(&cfg, json!({"path": "a.docx", "blocks": []})).is_err());
        assert!(run_write(&cfg, json!({"path": "a.docx", "blocks": [{"type":"nope"}]})).is_err());
    }

    #[test]
    fn heading_styles() {
        assert_eq!(heading_level("Heading3"), Some(3));
        assert_eq!(heading_level("heading 2"), Some(2));
        assert_eq!(heading_level("Title"), Some(1));
        assert_eq!(heading_level("Normal"), None);
    }
}
