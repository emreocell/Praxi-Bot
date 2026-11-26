//! Plain-text extraction from `.pptx` slides (reading only), and the `pptx_read` tool.

use std::fmt::Write as _;
use std::io::{Cursor, Read};

use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, clip, exec, parse, parse_ranges, read_limited};

const MAX_SLIDES: usize = 500;
const MAX_SLIDE_XML_BYTES: u64 = 8 * 1024 * 1024;

/// Text of every slide, in slide order, as `## Slide N` sections.
pub(crate) fn plain_text(bytes: &[u8], max_bytes: usize) -> Result<String, ToolError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| exec(format!("not a valid pptx archive: {e}")))?;
    let mut slides: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| {
            let n = n.ok()?;
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse::<u32>()
                .ok()?;
            Some((num, n.into_owned()))
        })
        .collect();
    slides.sort_unstable();
    slides.truncate(MAX_SLIDES);
    let mut out = String::new();
    for (num, name) in slides {
        if out.len() >= max_bytes {
            break;
        }
        let mut xml = Vec::new();
        zip.by_name(&name)
            .map_err(|e| exec(format!("cannot read {name}: {e}")))?
            .take(MAX_SLIDE_XML_BYTES + 1)
            .read_to_end(&mut xml)
            .map_err(|e| exec(format!("cannot read {name}: {e}")))?;
        if xml.len() as u64 > MAX_SLIDE_XML_BYTES {
            return Err(exec(format!("{name} expands beyond the size limit")));
        }
        let text = slide_text(&xml)?;
        if !text.trim().is_empty() {
            let _ = write!(out, "## Slide {num}\n\n{}\n\n", text.trim_end());
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ReadIn {
    path: String,
    /// 1-based slide numbers such as `1-3,7`; default: every slide.
    slides: Option<String>,
}

pub fn read_spec() -> ToolSpec {
    crate::mk_spec(
        "pptx_read",
        "Read the text of a .pptx presentation, slide by slide (titles, text boxes, table cells; no images or speaker notes). Slides are numbered by their file order. Example: {\"path\":\"deck.pptx\",\"slides\":\"1-3\"}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":".pptx file path."},
            "slides":{"type":"string","description":"1-based slides, e.g. \"1-3,7\". Default: all."}
        },"required":["path"]}),
        Effect::Read,
    )
}

pub fn run_read(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: ReadIn = parse(input)?;
    if !std::path::Path::new(&i.path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pptx"))
    {
        return Err(crate::common::invalid(
            "only .pptx files can be read (not .ppt or .pptm)",
        ));
    }
    let bytes = read_limited(cfg, &i.path)?;
    crate::check_zip_limits(&bytes)?;
    let wanted = i
        .slides
        .as_deref()
        .map(|spec| parse_ranges(spec, u32::MAX))
        .transpose()?;
    let all = read_slides(&bytes)?;
    let total = all.len();
    let picked: Vec<&(u32, String)> = all
        .iter()
        .filter(|(n, _)| wanted.as_ref().is_none_or(|w| w.contains(n)))
        .collect();
    let shown = picked.len().min(cfg.max_rows);
    let mut md = format!("# {} ({total} slides)\n\n", i.path);
    let mut out = Vec::new();
    for (n, text) in picked.iter().take(shown).map(|s| (&s.0, &s.1)) {
        let _ = write!(md, "## Slide {n}\n\n{}\n\n", text.trim_end());
        out.push(json!({"number": n, "text": clip(text.trim_end(), cfg.max_cell_chars)}));
    }
    if picked.len() > shown {
        let _ = writeln!(
            md,
            "_...{} more slides (limit {})_",
            picked.len() - shown,
            cfg.max_rows
        );
    }
    let data = json!({
        "path": i.path, "slide_count": total, "slides": out,
        "truncated": picked.len() > shown,
    });
    Ok(Reply::new(cfg, &md, data))
}

/// Every slide as (file number, text), in file-number order; empty slides are kept.
fn read_slides(bytes: &[u8]) -> Result<Vec<(u32, String)>, ToolError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| exec(format!("not a valid pptx archive: {e}")))?;
    let mut names: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| {
            let n = n.ok()?;
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse::<u32>()
                .ok()?;
            Some((num, n.into_owned()))
        })
        .collect();
    names.sort_unstable();
    names.truncate(MAX_SLIDES);
    let mut out = Vec::new();
    for (num, name) in names {
        let mut xml = Vec::new();
        zip.by_name(&name)
            .map_err(|e| exec(format!("cannot read {name}: {e}")))?
            .take(MAX_SLIDE_XML_BYTES + 1)
            .read_to_end(&mut xml)
            .map_err(|e| exec(format!("cannot read {name}: {e}")))?;
        if xml.len() as u64 > MAX_SLIDE_XML_BYTES {
            return Err(exec(format!("{name} expands beyond the size limit")));
        }
        out.push((num, slide_text(&xml)?));
    }
    Ok(out)
}

fn slide_text(xml: &[u8]) -> Result<String, ToolError> {
    let mut reader = Reader::from_reader(xml);
    let mut out = String::new();
    let mut in_t = false;
    loop {
        match reader
            .read_event()
            .map_err(|e| exec(format!("invalid slide xml: {e}")))?
        {
            Event::Eof => break,
            Event::Start(e) if e.local_name().as_ref() == "t" => in_t = true,
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "p" => out.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_t => {
                let s: &str = &t;
                out.push_str(s);
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn reads_slide_text_in_order() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (n, t) in [(2, "Second"), (1, "First")] {
            zip.start_file(format!("ppt/slides/slide{n}.xml"), opts)
                .unwrap();
            write!(
                zip,
                "<p:sld xmlns:a=\"a\" xmlns:p=\"p\"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"
            )
            .unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let text = plain_text(&bytes, 10_000).unwrap();
        assert!(text.find("First").unwrap() < text.find("Second").unwrap());
        assert!(text.contains("## Slide 1"));
    }

    fn deck(slides: &[(u32, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (n, t) in slides {
            zip.start_file(format!("ppt/slides/slide{n}.xml"), opts)
                .unwrap();
            write!(
                zip,
                "<p:sld xmlns:a=\"a\" xmlns:p=\"p\"><a:p><a:r><a:t>{t}</a:t></a:r></a:p><a:p><a:r><a:t>line two</a:t></a:r></a:p></p:sld>"
            )
            .unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn the_tool_reads_all_slides_or_a_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pptx");
        std::fs::write(&path, deck(&[(1, "Title"), (2, "Agenda"), (3, "Close")])).unwrap();
        let cfg = DocToolsConfig::default();
        let all = run_read(&cfg, json!({"path": path})).unwrap();
        assert_eq!(all.data["slide_count"], 3);
        assert_eq!(all.data["slides"][1]["text"], "Agenda\nline two");
        assert!(all.markdown.contains("## Slide 3"));
        let some = run_read(&cfg, json!({"path": path, "slides": "1,3"})).unwrap();
        let numbers: Vec<_> = some.data["slides"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["number"].clone())
            .collect();
        assert_eq!(numbers, [json!(1), json!(3)]);
        assert!(!some.markdown.contains("Agenda"));
        let capped = DocToolsConfig {
            max_rows: 2,
            ..DocToolsConfig::default()
        };
        assert_eq!(
            run_read(&capped, json!({"path": path})).unwrap().data["truncated"],
            true
        );
        let junk = dir.path().join("junk.pptx");
        std::fs::write(&junk, b"junk").unwrap();
        assert!(run_read(&cfg, json!({"path": junk})).is_err());
        let wrong = dir.path().join("deck.ppt");
        std::fs::write(&wrong, deck(&[(1, "x")])).unwrap();
        assert!(run_read(&cfg, json!({"path": wrong})).is_err());
    }
}
