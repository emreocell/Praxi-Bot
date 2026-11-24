//! Static checks on an untrusted document before LibreOffice ever sees it.
//!
//! A document is the attacker here. LibreOffice follows external references while it
//! loads a file (linked images, an attached template, linked OLE objects, DDE fields,
//! mail-merge data sources, ODF `xlink:href` links) and every one of those is an outbound
//! request made by the daemon: SSRF, or an NTLM hash sent to a `\\host\share`. So a
//! document that carries any of them is refused outright. The only external target that
//! is allowed is an ordinary clickable web or mail hyperlink (`http:`, `https:`,
//! `mailto:`), which LibreOffice never fetches during a conversion.
//!
//! This is defence in depth together with the locked-down profile in `convert.rs`; it is
//! a scan, not a parser of every Office feature, so formats it cannot read are refused.

use std::io::Read as _;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use tool_registry::ToolError;

use crate::common::invalid;

/// Largest single XML part that is scanned.
const PART_CAP: u64 = 16 * 1024 * 1024;
/// Most bytes of XML scanned per document.
const TOTAL_CAP: u64 = 96 * 1024 * 1024;

fn external(what: &str) -> ToolError {
    invalid(format!(
        "the document refers to external content ({what}); it is not converted"
    ))
}

fn unreadable() -> ToolError {
    invalid("the document could not be checked for external content; it is not converted")
}

fn macros() -> ToolError {
    invalid("the document contains macros; it is not converted")
}

/// Refuse a document that is not what its extension says, that carries macros, or that
/// refers to anything outside itself.
pub(crate) fn vet_source(ext: &str, bytes: &[u8]) -> Result<(), ToolError> {
    match ext {
        "pdf" => {
            if !bytes.starts_with(b"%PDF") {
                return Err(invalid("the file is not a valid PDF"));
            }
            Ok(())
        }
        "rtf" => {
            if !bytes.starts_with(b"{\\rtf") {
                return Err(invalid("the file is not a valid RTF document"));
            }
            vet_rtf(bytes)
        }
        _ => vet_zip(bytes),
    }
}

/// RTF has no relationships part; look for the control words that link out.
fn vet_rtf(bytes: &[u8]) -> Result<(), ToolError> {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    for marker in [
        "\\objlink",
        "\\objautlink",
        "\\template",
        "\\mmdatasource",
        "\\mmconnectstr",
        "\\mmquery",
        "includepicture",
        "includetext",
        "ddeauto",
    ] {
        if text.contains(marker) {
            return Err(external("a linked object, template, field or data source"));
        }
    }
    Ok(())
}

fn vet_zip(bytes: &[u8]) -> Result<(), ToolError> {
    crate::check_zip_limits(bytes)?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| invalid("the file is not a valid Office document"))?;
    let names: Vec<String> = zip
        .file_names()
        .filter_map(Result::ok)
        .map(|n| n.to_string())
        .collect();
    let odf = names.iter().any(|n| n == "content.xml" || n == "mimetype");
    for name in &names {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("vbaproject.bin")
            || lower.starts_with("basic/")
            || lower.starts_with("scripts/")
        {
            return Err(macros());
        }
        if lower.starts_with("xl/externallinks/") {
            return Err(external("a linked workbook"));
        }
    }
    let mut budget = TOTAL_CAP;
    if let Ok(entry) = zip.by_name("[Content_Types].xml") {
        let mut head = Vec::new();
        let _ = entry.take(256 * 1024).read_to_end(&mut head);
        if String::from_utf8_lossy(&head).contains("macroEnabled") {
            return Err(macros());
        }
    }
    for name in &names {
        let lower = name.to_ascii_lowercase();
        let kind = if ext_is(&lower, "rels") {
            Part::Rels
        } else if odf && is_odf_xml(&lower) {
            Part::Odf {
                meta: lower.ends_with("meta.xml"),
            }
        } else if lower.starts_with("word/") && (ext_is(&lower, "xml") || ext_is(&lower, "vml")) {
            Part::Word
        } else {
            continue;
        };
        let data = read_part(&mut zip, name, &mut budget)?;
        match kind {
            Part::Rels => scan_rels(&data)?,
            Part::Odf { meta } => scan_odf(&data, meta)?,
            Part::Word => scan_word(&data)?,
        }
    }
    Ok(())
}

enum Part {
    Rels,
    Odf { meta: bool },
    Word,
}

fn ext_is(name: &str, ext: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn is_odf_xml(lower: &str) -> bool {
    ["content.xml", "styles.xml", "settings.xml", "meta.xml"]
        .iter()
        .any(|n| lower == *n || lower.ends_with(&format!("/{n}")))
}

fn read_part(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    budget: &mut u64,
) -> Result<Vec<u8>, ToolError> {
    let entry = zip.by_name(name).map_err(|_| unreadable())?;
    if entry.size() > PART_CAP || entry.size() > *budget {
        return Err(invalid(
            "the document is too large to check for external content; it is not converted",
        ));
    }
    let mut data = Vec::new();
    entry
        .take(PART_CAP + 1)
        .read_to_end(&mut data)
        .map_err(|_| unreadable())?;
    if data.len() as u64 > PART_CAP || data.len() as u64 > *budget {
        return Err(invalid(
            "the document is too large to check for external content; it is not converted",
        ));
    }
    *budget -= data.len() as u64;
    Ok(data)
}

fn attr(e: &BytesStart<'_>, local: &str) -> Result<Option<String>, ToolError> {
    for a in e.attributes() {
        let a = a.map_err(|_| unreadable())?;
        if a.key.local_name().as_ref() == local {
            return Ok(Some(a.value.to_string()));
        }
    }
    Ok(None)
}

/// A link target that LibreOffice does not fetch: a web or mail hyperlink.
fn is_plain_hyperlink(target: &str) -> bool {
    let t = target.trim().to_ascii_lowercase();
    t.starts_with("http://") || t.starts_with("https://") || t.starts_with("mailto:")
}

/// OOXML relationships: any `TargetMode="External"` except a plain hyperlink.
fn scan_rels(data: &[u8]) -> Result<(), ToolError> {
    let mut reader = Reader::from_reader(data);
    loop {
        match reader.read_event().map_err(|_| unreadable())? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                let mode = attr(&e, "TargetMode")?.unwrap_or_default();
                // An entity-encoded value is a way to hide the word; never legitimate.
                if mode.contains('&') {
                    return Err(unreadable());
                }
                if !mode.trim().eq_ignore_ascii_case("external") {
                    continue;
                }
                let kind = attr(&e, "Type")?.unwrap_or_default();
                let target = attr(&e, "Target")?.unwrap_or_default();
                if kind.to_ascii_lowercase().ends_with("/hyperlink") && is_plain_hyperlink(&target)
                {
                    continue;
                }
                return Err(external(
                    "a linked image, template, object or other external relationship",
                ));
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Field instructions that fetch or run something outside the document.
fn field_is_dangerous(instruction: &str) -> bool {
    instruction.split_whitespace().any(|w| {
        let w = w.trim_matches('"').to_ascii_lowercase();
        matches!(
            w.as_str(),
            "dde" | "ddeauto" | "includepicture" | "includetext" | "link" | "import"
        )
    })
}

/// WordprocessingML parts: DDE and linking fields, VML links, mail merge, linked OLE.
fn scan_word(data: &[u8]) -> Result<(), ToolError> {
    let mut reader = Reader::from_reader(data);
    let mut field = String::new();
    let mut in_instr = false;
    let flush = |field: &mut String| -> Result<(), ToolError> {
        let bad = field_is_dangerous(field);
        field.clear();
        if bad {
            Err(external("a DDE or linking field"))
        } else {
            Ok(())
        }
    };
    loop {
        match reader.read_event().map_err(|_| unreadable())? {
            Event::Start(e) | Event::Empty(e) => {
                let local = e.local_name();
                match local.as_ref() {
                    "instrText" => in_instr = true,
                    "fldChar" => flush(&mut field)?,
                    "fldSimple" => {
                        if let Some(i) = attr(&e, "instr")?
                            && field_is_dangerous(&i)
                        {
                            return Err(external("a DDE or linking field"));
                        }
                    }
                    "mailMerge" | "odso" => return Err(external("a mail-merge data source")),
                    "OLEObject"
                        if attr(&e, "Type")?.is_some_and(|t| t.eq_ignore_ascii_case("link")) =>
                    {
                        return Err(external("a linked OLE object"));
                    }
                    _ => {}
                }
                for a in e.attributes() {
                    let a = a.map_err(|_| unreadable())?;
                    let key: &str = a.key.as_ref();
                    if key == "o:href" || key == "r:link" {
                        return Err(external("a linked picture or object"));
                    }
                }
            }
            Event::Text(t) if in_instr => field.push_str(&t.into_inner()),
            Event::End(e) if e.local_name().as_ref() == "instrText" => in_instr = false,
            Event::Eof => return flush(&mut field),
            _ => {}
        }
    }
}

/// An `xlink:href` that stays inside the package: no scheme, no absolute or parent path.
fn href_is_internal(href: &str) -> bool {
    let h = href.trim();
    if h.is_empty() || h.starts_with('#') {
        return true;
    }
    if h.starts_with('/') || h.contains(['\\', '%', '&']) || h.split('/').any(|seg| seg == "..") {
        return false;
    }
    match (h.find(':'), h.find('/')) {
        (Some(c), Some(s)) => c > s,
        (Some(_), None) => false,
        _ => true,
    }
}

/// ODF parts: any `xlink:href` that leaves the package, DDE elements, a template link.
fn scan_odf(data: &[u8], meta: bool) -> Result<(), ToolError> {
    let mut reader = Reader::from_reader(data);
    loop {
        match reader.read_event().map_err(|_| unreadable())? {
            Event::Start(e) | Event::Empty(e) => {
                let local = e.local_name();
                if local.as_ref().starts_with("dde-") {
                    return Err(external("a DDE connection"));
                }
                let is_text_link = local.as_ref() == "a";
                for a in e.attributes() {
                    let a = a.map_err(|_| unreadable())?;
                    if a.key.local_name().as_ref() != "href" {
                        continue;
                    }
                    let href = a.value.to_string();
                    let ok = if meta {
                        // meta.xml only links to a template; that is never needed to convert.
                        href.trim().is_empty()
                    } else {
                        href_is_internal(&href) || (is_text_link && is_plain_hyperlink(&href))
                    };
                    if !ok {
                        return Err(external("a linked picture, object, section or template"));
                    }
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyperlinks_pass_but_other_externals_do_not() {
        let rels = |t: &str, ty: &str| {
            format!(
                "<Relationships><Relationship Id=\"1\" Type=\"x/{ty}\" Target=\"{t}\" TargetMode=\"External\"/></Relationships>"
            )
        };
        assert!(scan_rels(rels("https://example.org", "hyperlink").as_bytes()).is_ok());
        assert!(scan_rels(rels("\\\\host\\s", "hyperlink").as_bytes()).is_err());
        assert!(scan_rels(rels("https://example.org", "image").as_bytes()).is_err());
        assert!(scan_rels(b"<Relationships><Relationship").is_err());
    }

    #[test]
    fn fields_are_judged_per_field() {
        let doc = |a: &str, b: &str| {
            format!(
                "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/><w:instrText>{a}</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"end\"/><w:fldChar w:fldCharType=\"begin\"/><w:instrText>{b}</w:instrText><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>"
            )
        };
        assert!(scan_word(doc(" PAGE ", " HYPERLINK \"http://a\" ").as_bytes()).is_ok());
        assert!(scan_word(doc(" PAGE", "DDE x").as_bytes()).is_err());
        assert!(scan_word(doc("INCLUDEPICTURE \"x\"", " PAGE ").as_bytes()).is_err());
    }

    #[test]
    fn odf_links_must_stay_inside_the_package() {
        for ok in ["Pictures/a.png", "#bm", "./Object 1", ""] {
            assert!(href_is_internal(ok), "{ok}");
        }
        for bad in [
            "http://x",
            "file:///x",
            "//host/share",
            "../x",
            "C:\\x",
            "/etc/passwd",
            "a%2e%2e/b",
            "vnd.sun.star.script:x",
        ] {
            assert!(!href_is_internal(bad), "{bad}");
        }
    }
}
