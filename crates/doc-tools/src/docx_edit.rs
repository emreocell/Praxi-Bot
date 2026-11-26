//! `docx_append`: add blocks to the end of an existing `.docx`.
//!
//! `docx_write` builds a new document, so using it on an existing file drops its styles,
//! headers, images and layout. This tool rewrites only `word/document.xml` (new blocks go
//! in front of the final section properties) and copies every other part byte for byte.
//! A heading uses the document's own `Heading N` style when it has one, and a table gets
//! plain borders when the document has no `Table Grid` style.

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::Event;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, exec, invalid, parse, read_limited, replace_file};
use crate::docx::{StyleSupport, WBlock, blocks_xml};
use crate::ooxml::{self, attr};

const MAIN_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const MAX_BLOCKS: usize = 10_000;

#[derive(Deserialize)]
struct AppendIn {
    path: String,
    blocks: Vec<WBlock>,
}

pub fn append_spec() -> ToolSpec {
    crate::mk_spec(
        "docx_append",
        "Add blocks to the end of an existing .docx without touching its styles, images, headers or earlier content. Blocks: heading {level 1-6,text}, paragraph {text,bold?}, bullets {items}, table {rows,header?}. Example: {\"path\":\"report.docx\",\"blocks\":[{\"type\":\"heading\",\"level\":2,\"text\":\"Update\"},{\"type\":\"paragraph\",\"text\":\"Done.\"}]}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Existing .docx file."},
            "blocks":{"type":"array","description":"Blocks to add, in order.","items":{"type":"object","properties":{
                "type":{"type":"string","enum":["heading","paragraph","bullets","table"]},
                "level":{"type":"integer","minimum":1,"maximum":6},
                "text":{"type":"string"},
                "bold":{"type":"boolean"},
                "items":{"type":"array","items":{"type":"string"}},
                "rows":{"type":"array","items":{"type":"array","items":{"type":"string"}}},
                "header":{"type":"boolean","description":"Bold the first table row (default true)."}
            },"required":["type"]}}
        },"required":["path","blocks"]}),
        Effect::Write,
    )
}

fn xml_err(e: &impl std::fmt::Display) -> ToolError {
    exec(format!("invalid document.xml: {e}"))
}

/// `document` with `insert` placed in front of the body's own section properties (or at the
/// end of the body when it has none).
fn insert_into_body(document: &[u8], insert: &str) -> Result<Vec<u8>, ToolError> {
    let mut reader = Reader::from_reader(document);
    let mut w = Writer::new(Vec::new());
    let (mut depth, mut done, mut wordml) = (0usize, false, false);
    let put = |w: &mut Writer<Vec<u8>>, ev: Event<'_>| {
        w.write_event(ev)
            .map_err(|e| exec(format!("xml write: {e}")))
    };
    loop {
        let ev = reader.read_event().map_err(|e| xml_err(&e))?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                if depth == 0 {
                    wordml = e.local_name().as_ref() == "document"
                        && e.attributes()
                            .flatten()
                            .any(|a| a.key.as_ref() == "xmlns:w" && a.value.as_ref() == MAIN_NS);
                }
                if depth == 1 && e.local_name().as_ref() == "body" && !wordml {
                    return Err(invalid(
                        "this document does not use the standard `w` prefix",
                    ));
                }
                if depth == 2 && !done && e.local_name().as_ref() == "sectPr" {
                    w.get_mut().extend_from_slice(insert.as_bytes());
                    done = true;
                }
                depth += 1;
                put(&mut w, Event::Start(e))?;
            }
            Event::Empty(e) => {
                if depth == 2 && !done && e.local_name().as_ref() == "sectPr" {
                    w.get_mut().extend_from_slice(insert.as_bytes());
                    done = true;
                }
                if depth == 1 && e.local_name().as_ref() == "body" && wordml && !done {
                    let name = e.name().as_ref().to_owned();
                    put(&mut w, Event::Start(e))?;
                    w.get_mut().extend_from_slice(insert.as_bytes());
                    w.get_mut()
                        .extend_from_slice(format!("</{name}>").as_bytes());
                    done = true;
                } else {
                    put(&mut w, Event::Empty(e))?;
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                if depth == 1 && !done && e.local_name().as_ref() == "body" {
                    w.get_mut().extend_from_slice(insert.as_bytes());
                    done = true;
                }
                put(&mut w, Event::End(e))?;
            }
            other => put(&mut w, other)?,
        }
    }
    if !done {
        return Err(invalid(
            "not a Word document: word/document.xml has no body",
        ));
    }
    Ok(w.into_inner())
}

/// Whether `styles` (the document's `word/styles.xml`) defines the style `id`.
fn defines_style(styles: Option<&[u8]>, id: &str) -> bool {
    let Some(styles) = styles else {
        return false;
    };
    let mut reader = Reader::from_reader(styles);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => return false,
            Ok(Event::Start(e) | Event::Empty(e))
                if e.local_name().as_ref() == "style"
                    && attr(&e, "styleId").as_deref() == Some(id) =>
            {
                return true;
            }
            _ => {}
        }
    }
}

pub fn run_append(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: AppendIn = parse(input)?;
    if i.blocks.is_empty() {
        return Err(invalid("blocks must not be empty"));
    }
    if i.blocks.len() > MAX_BLOCKS {
        return Err(invalid(format!("more than {MAX_BLOCKS} blocks")));
    }
    let is_docx = std::path::Path::new(&i.path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("docx"));
    if !is_docx {
        return Err(invalid(
            "only .docx files can be edited in place (not .doc or .docm)",
        ));
    }
    let bytes = read_limited(cfg, &i.path)?;
    crate::check_zip_limits(&bytes)?;
    let mut zip = ooxml::open(&bytes)?;
    if zip.by_name("word/vbaProject.bin").is_ok() {
        return Err(invalid("documents with macros are never edited"));
    }
    let document = ooxml::read_part(&mut zip, "word/document.xml")?
        .ok_or_else(|| invalid("not a Word document: word/document.xml is missing"))?;
    let styles = ooxml::read_part(&mut zip, "word/styles.xml")?;
    drop(zip);
    let xml = blocks_xml(
        &i.blocks,
        StyleSupport {
            headings: defines_style(styles.as_deref(), "Heading1"),
            table_grid: defines_style(styles.as_deref(), "TableGrid"),
        },
    )?;
    let edited = insert_into_body(&document, &xml)?;
    let mut replace = ooxml::XmlHashMap::new();
    replace.insert("word/document.xml".to_owned(), edited);
    let out = ooxml::rewrite(&bytes, &replace, &[])?;
    replace_file(cfg, &i.path, &out)?;
    let md = format!(
        "Added {} block(s) to the end of {}. Earlier content, styles and images were kept.\n",
        i.blocks.len(),
        i.path
    );
    Ok(Reply::new(
        cfg,
        &md,
        json!({"path": i.path, "blocks_added": i.blocks.len(), "bytes": out.len()}),
    ))
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;
    use crate::docx::{self, WBlock};

    fn part(bytes: &[u8], name: &str) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = String::new();
        zip.by_name(name).unwrap().read_to_string(&mut out).unwrap();
        out
    }

    fn fixture(dir: &std::path::Path) -> (String, Vec<u8>) {
        let bytes = docx::build_docx(&[
            WBlock::Heading {
                text: "Original".into(),
                level: 1,
            },
            WBlock::Paragraph {
                text: "First paragraph".into(),
                bold: false,
            },
        ])
        .unwrap();
        let path = dir.join("report.docx").to_str().unwrap().to_owned();
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    #[test]
    fn append_adds_blocks_after_the_old_content_and_keeps_every_other_part() {
        let dir = tempfile::tempdir().unwrap();
        let (p, before) = fixture(dir.path());
        let cfg = DocToolsConfig::default();
        let r = run_append(
            &cfg,
            json!({"path": p, "blocks": [
                {"type": "heading", "level": 2, "text": "Update"},
                {"type": "paragraph", "text": "Added <text> & more", "bold": true},
                {"type": "bullets", "items": ["one", "two"]},
                {"type": "table", "rows": [["a", "b"], ["1", "2"]]}
            ]}),
        )
        .unwrap();
        assert_eq!(r.data["blocks_added"], 4);
        let read = crate::docx::run_read(&cfg, json!({"path": p})).unwrap();
        let blocks = read.data["blocks"].as_array().unwrap();
        let texts: Vec<&str> = blocks.iter().filter_map(|b| b["text"].as_str()).collect();
        assert_eq!(
            texts[..2],
            ["Original", "First paragraph"],
            "old content stays first"
        );
        assert_eq!(blocks[2]["type"], "heading");
        assert_eq!(blocks[2]["level"], 2);
        assert_eq!(blocks[3]["text"], "Added <text> & more");
        assert!(texts.contains(&"\u{2022} two"));
        assert_eq!(blocks.last().unwrap()["type"], "table");

        let after = std::fs::read(&p).unwrap();
        for name in [
            "word/styles.xml",
            "[Content_Types].xml",
            "_rels/.rels",
            "word/_rels/document.xml.rels",
        ] {
            assert_eq!(part(&after, name), part(&before, name), "{name}");
        }
        let document = part(&after, "word/document.xml");
        let new_content = document.find("Update").unwrap();
        assert!(
            new_content < document.find("<w:sectPr>").unwrap(),
            "blocks sit before the section properties"
        );
        assert!(document.ends_with("</w:sectPr></w:body></w:document>"));
        assert!(document.contains("w:val=\"TableGrid\""));
    }

    #[test]
    fn a_document_without_styles_gets_plain_headings_and_bordered_tables() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bare.docx").to_str().unwrap().to_owned();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(
            b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>Old</w:t></w:r></w:p></w:body></w:document>",
        )
        .unwrap();
        std::fs::write(&p, zip.finish().unwrap().into_inner()).unwrap();
        let cfg = DocToolsConfig::default();
        run_append(
            &cfg,
            json!({"path": p, "blocks": [
                {"type": "heading", "text": "New"},
                {"type": "table", "rows": [["a"]]}
            ]}),
        )
        .unwrap();
        let document = part(&std::fs::read(&p).unwrap(), "word/document.xml");
        assert!(
            !document.contains("Heading1") && !document.contains("TableGrid"),
            "{document}"
        );
        assert!(document.contains("<w:tblBorders>") && document.contains("<w:b/>"));
        assert!(document.find("Old").unwrap() < document.find("New").unwrap());
        let read = crate::docx::run_read(&cfg, json!({"path": p})).unwrap();
        assert_eq!(read.data["blocks"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn bad_requests_leave_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (p, before) = fixture(dir.path());
        let cfg = DocToolsConfig::default();
        for bad in [
            json!({"path": p, "blocks": []}),
            json!({"path": p, "blocks": [{"type": "table", "rows": []}]}),
            json!({"path": p, "blocks": [{"type": "nope"}]}),
        ] {
            assert!(run_append(&cfg, bad.clone()).is_err(), "{bad}");
        }
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, "x").unwrap();
        assert!(
            run_append(
                &cfg,
                json!({"path": txt, "blocks": [{"type":"paragraph","text":"x"}]})
            )
            .is_err()
        );
        let guarded = DocToolsConfig {
            write_guard: Some(crate::WriteGuard::new(|_| Err("protected".into()))),
            ..DocToolsConfig::default()
        };
        let err = run_append(
            &guarded,
            json!({"path": p, "blocks": [{"type":"paragraph","text":"x"}]}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("protected"));
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }
}
