//! Static checks on an untrusted document before LibreOffice ever sees it.
//!
//! DESIGN: a small, boring, deny-by-default core. The real control against a document
//! that makes the converter reach out is network isolation (`convert_sandbox`); this
//! module only refuses documents that are not *obviously plain*. It does not try to
//! recognise every dangerous Office feature: anything that could refer to the outside
//! world, embed another container or run something is refused, even when harmless.
//!
//! Inputs: PDF (magic only) and zip containers (docx/xlsx/pptx/odt/ods/odp). RTF is not
//! accepted at all. Every XML part is decoded properly (text, CDATA and entity
//! references) and attributes are matched by local name, never by a literal prefix.

use std::collections::HashSet;
use std::io::Read as _;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use tool_registry::ToolError;

use crate::common::invalid;

/// Largest single XML part that is scanned.
const PART_CAP: u64 = 16 * 1024 * 1024;
/// Most bytes of XML scanned per document.
const TOTAL_CAP: u64 = 96 * 1024 * 1024;
/// Most entries in a container (decompression-bomb / parser-confusion guard).
const MAX_ENTRIES: usize = 2_000;
/// Largest allowed expansion of one entry (zip bomb).
const MAX_RATIO: u64 = 200;
/// Entries smaller than this are not ratio-checked.
const RATIO_FLOOR: u64 = 1024 * 1024;

fn refuse(what: &str) -> ToolError {
    invalid(format!(
        "the document is not plain enough to convert safely ({what}); it is not converted"
    ))
}

fn unreadable() -> ToolError {
    invalid("the document could not be checked; it is not converted")
}

/// Refuse a document that is not what its extension says or that is not obviously plain.
pub(crate) fn vet_source(ext: &str, bytes: &[u8]) -> Result<(), ToolError> {
    match ext {
        "pdf" => {
            if !bytes.starts_with(b"%PDF") {
                return Err(invalid("the file is not a valid PDF"));
            }
            Ok(())
        }
        // RTF is a free-form text format with fields, escapes and linked objects that
        // cannot be vetted reliably; it is refused outright.
        "rtf" => Err(invalid(
            "RTF input is not supported (save it as docx or odt first)",
        )),
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" => vet_zip(bytes),
        _ => Err(invalid("this file type cannot be converted")),
    }
}

/// Parts a plain document may contain, by extension (lower case, `""` = none).
/// Everything else (`bin`, `ole`, `html`, `rtf`, `svg`, nested containers, scripts) is refused.
const PLAIN_PART_EXT: &[&str] = &[
    "xml", "rels", "vml", "rdf", "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "tif", "tiff",
    "emf", "wmf", "odttf", "ttf", "otf", "fntdata", "",
];

fn ext_of(name: &str) -> String {
    let file = name.rsplit('/').next().unwrap_or(name);
    file.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn vet_zip(bytes: &[u8]) -> Result<(), ToolError> {
    crate::check_zip_limits(bytes)?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| invalid("the file is not a valid Office document"))?;
    if zip.len() > MAX_ENTRIES {
        return Err(refuse("too many parts"));
    }
    // Pass 1, names only: this is where hidden parts and duplicates are caught.
    let mut seen = HashSet::new();
    let mut names = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|_| unreadable())?;
        let name = entry.name().map_err(|_| unreadable())?.into_owned();
        let lower = name.to_ascii_lowercase();
        // A duplicate name: the checker and LibreOffice could read different copies.
        if !seen.insert(lower.clone()) {
            return Err(refuse("two parts share one name"));
        }
        // Path tricks in a part name.
        if name.contains(['\\', ':', '\0'])
            || name.starts_with('/')
            || name.split('/').any(|s| s == "..")
        {
            return Err(refuse("a part has an unusual name"));
        }
        // Zip bomb: a tiny entry that expands enormously.
        if entry.size() > RATIO_FLOOR && entry.size() / entry.compressed_size().max(1) > MAX_RATIO {
            return Err(refuse("a part is compressed suspiciously well"));
        }
        if entry.is_dir() {
            continue;
        }
        // Macros.
        if lower.contains("vbaproject")
            || lower.starts_with("basic/")
            || lower.contains("/basic/")
            || lower.starts_with("scripts/")
        {
            return Err(invalid("the document contains macros; it is not converted"));
        }
        // Embedded objects and ActiveX: OLE blobs, nested documents, controls.
        if lower.contains("embeddings/") || lower.contains("activex") || lower.contains("oleobject")
        {
            return Err(refuse("an embedded object"));
        }
        // Linked workbooks, data connections and queries (fetch on load).
        if lower.contains("externallink")
            || lower.contains("connections")
            || lower.contains("querytable")
            || lower.contains("webextension")
        {
            return Err(refuse("a linked data source"));
        }
        // HTML/MHT/RTF chunks merged into the document (altChunk) and other odd types.
        if lower.contains("afchunk") || !PLAIN_PART_EXT.contains(&ext_of(&lower).as_str()) {
            return Err(refuse("a part of an unusual type"));
        }
        names.push((i, name));
    }
    let odf = seen.contains("mimetype") || seen.contains("content.xml");
    let mut budget = TOTAL_CAP;
    for (i, name) in names {
        let entry = zip.by_index(i).map_err(|_| unreadable())?;
        let lower = name.to_ascii_lowercase();
        let xml = matches!(ext_of(&lower).as_str(), "xml" | "rels" | "vml" | "rdf");
        if xml && (entry.size() > PART_CAP || entry.size() > budget) {
            return Err(refuse("a part is too large to check"));
        }
        let mut data = Vec::new();
        entry
            .take(if xml { PART_CAP + 1 } else { 8 })
            .read_to_end(&mut data)
            .map_err(|_| unreadable())?;
        // A nested zip or OLE file hidden under any name or extension.
        if data.starts_with(b"PK\x03\x04")
            || data.starts_with(b"PK\x05\x06")
            || data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0])
        {
            return Err(refuse("a nested container"));
        }
        if !xml {
            continue;
        }
        if data.len() as u64 > PART_CAP || data.len() as u64 > budget {
            return Err(refuse("a part is too large to check"));
        }
        budget -= data.len() as u64;
        if ext_of(&lower) == "rels" {
            scan_rels(&data)?;
        } else {
            scan_xml(&data, &lower, odf)?;
        }
    }
    if let Ok(mut ct) = zip.by_name("[Content_Types].xml") {
        let mut head = Vec::new();
        let _ = (&mut ct).take(256 * 1024).read_to_end(&mut head);
        if String::from_utf8_lossy(&head).contains("macroEnabled") {
            return Err(invalid("the document contains macros; it is not converted"));
        }
    }
    Ok(())
}

/// Attribute value, fully decoded, by local name (any prefix or namespace).
fn attr(e: &BytesStart<'_>, local: &str) -> Result<Option<String>, ToolError> {
    for a in e.attributes() {
        let a = a.map_err(|_| unreadable())?;
        if a.key.local_name().into_inner() == local {
            return Ok(Some(
                a.normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|_| unreadable())?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

/// Collects element text: plain text, CDATA and entity / character references.
fn push_text(buf: &mut String, ev: &Event<'_>) -> Result<(), ToolError> {
    match ev {
        Event::Text(t) => buf.push_str(t),
        Event::CData(c) => buf.push_str(c),
        Event::GeneralRef(r) => {
            if let Some(ch) = r.resolve_char_ref().map_err(|_| unreadable())? {
                buf.push(ch);
            } else {
                // Only the five predefined entities exist; a custom one is a hiding place.
                buf.push(match &**r {
                    "lt" => '<',
                    "gt" => '>',
                    "amp" => '&',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => return Err(unreadable()),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

/// OOXML relationships. RULE: no relationship may be external (a linked image, template,
/// OLE object or hyperlink is a fetch by id), and none may point at a scheme or a UNC
/// path even when `TargetMode` is missing (stops SSRF / NTLM leak via a mislabelled rel).
/// Template, sub-document and altChunk relations are refused whatever their target.
fn scan_rels(data: &[u8]) -> Result<(), ToolError> {
    let mut reader = Reader::from_reader(data);
    loop {
        match reader.read_event().map_err(|_| unreadable())? {
            Event::DocType(_) => return Err(unreadable()),
            Event::Start(e) | Event::Empty(e) if e.local_name().into_inner() == "Relationship" => {
                let mode = attr(&e, "TargetMode")?.unwrap_or_default();
                let kind = attr(&e, "Type")?.unwrap_or_default().to_ascii_lowercase();
                let target = attr(&e, "Target")?.unwrap_or_default();
                if !mode.trim().is_empty() && !mode.trim().eq_ignore_ascii_case("internal") {
                    return Err(refuse("an external link"));
                }
                if target.contains([':', '\\']) || target.trim_start().starts_with("//") {
                    return Err(refuse("a link with a scheme or network path"));
                }
                for bad in [
                    "attachedtemplate",
                    "subdocument",
                    "afchunk",
                    "oleobject",
                    "package",
                    "externallink",
                    "frame",
                    "mailmergesource",
                    "recipientdata",
                ] {
                    if kind.contains(bad) {
                        return Err(refuse("a template, sub-document or embedded object"));
                    }
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Field codes a plain document needs for layout. RULE: everything else (INCLUDEPICTURE,
/// INCLUDETEXT, LINK, DDE, IMPORT, HYPERLINK, DATABASE, ...) is refused.
const FIELD_ALLOW: &[&str] = &[
    "PAGE",
    "NUMPAGES",
    "SECTION",
    "SECTIONPAGES",
    "TOC",
    "PAGEREF",
    "REF",
    "DATE",
    "TIME",
    "CREATEDATE",
    "SAVEDATE",
    "SEQ",
    "TITLE",
    "AUTHOR",
];

fn field_ok(instruction: &str) -> bool {
    let t = instruction.trim();
    if t.is_empty() {
        return true;
    }
    if t.contains("://") || t.contains("\\\\") || t.to_ascii_lowercase().contains("file:") {
        return false;
    }
    let first = t
        .split(|c: char| c.is_whitespace() || c == '\\')
        .next()
        .unwrap_or("")
        .trim_matches('"')
        .to_ascii_uppercase();
    FIELD_ALLOW.contains(&first.as_str())
}

/// Function names that fetch or read something outside the sheet. RULE: refuse a formula
/// that mentions one (WEBSERVICE/FILTERXML/IMAGE/DDE/HYPERLINK/INDIRECT/CALL/RTD), or a
/// URL scheme, a `file:` reference or a UNC path (an external-reference formula).
fn formula_ok(formula: &str) -> bool {
    let f = formula.to_ascii_lowercase();
    let calls = |name: &str| {
        f.match_indices(name)
            .any(|(at, _)| f[at + name.len()..].trim_start().starts_with('('))
    };
    ![
        "webservice",
        "filterxml",
        "image",
        "dde",
        "hyperlink",
        "indirect",
        "call",
        "register",
        "rtd",
        "sql.request",
    ]
    .iter()
    .any(|n| calls(n))
        && !["://", "file:", "http", "ftp:", "\\\\", "//"]
            .iter()
            .any(|bad| f.contains(bad))
}

/// Elements that always mean "something outside this document". RULE: refuse.
fn element_refused(local: &str) -> bool {
    matches!(
        local,
        // OOXML: merged foreign content, template, sub-document, mail merge, data sources
        "altChunk"
            | "attachedTemplate"
            | "subDoc"
            | "mailMerge"
            | "odso"
            | "externalReference"
            | "externalReferences"
            | "externalData"
            | "externalBook"
            | "ddeLink"
            | "oleLink"
            | "connection"
            | "connections"
            | "dbPr"
            | "webPr"
            | "queryTable"
            | "olePr"
            // ODF: DDE, linked tables, database sources, scripts
            | "table-source"
            | "script"
            | "event-listener"
            | "scripts"
    ) || local.starts_with("dde-")
        || local.starts_with("database-source")
}

/// One XML part (WordprocessingML, SpreadsheetML, PresentationML, DrawingML, VML, ODF).
fn scan_xml(data: &[u8], name: &str, odf: bool) -> Result<(), ToolError> {
    let meta = name.ends_with("meta.xml");
    let mut reader = Reader::from_reader(data);
    // What is being collected: a field instruction, or a formula.
    let mut field = String::new();
    let mut in_instr = false;
    let mut formula = String::new();
    let mut in_formula = false;
    loop {
        let ev = reader.read_event().map_err(|_| unreadable())?;
        match &ev {
            // Entity declarations can hide any word from a scanner.
            Event::DocType(_) => return Err(unreadable()),
            Event::Start(e) | Event::Empty(e) => {
                let open = matches!(ev, Event::Start(_));
                let local = e.local_name().into_inner().to_owned();
                if element_refused(&local) {
                    return Err(refuse("a link, data source, merge or script"));
                }
                match local.as_str() {
                    "instrText" | "delInstrText" => {
                        // Pieces of one field are joined; only a field boundary clears.
                        in_instr = open;
                    }
                    "fldChar" => {
                        if !field_ok(&field) {
                            return Err(refuse("a field that is not plain layout"));
                        }
                        field.clear();
                    }
                    "fldSimple" => {
                        if let Some(i) = attr(e, "instr")?
                            && !field_ok(&i)
                        {
                            return Err(refuse("a field that is not plain layout"));
                        }
                    }
                    // OOXML formulas and defined names; DataValidation / conditional formats.
                    "f" | "formula" | "formula1" | "formula2" | "definedName" => {
                        in_formula = open;
                        formula.clear();
                    }
                    _ => {}
                }
                for a in e.attributes() {
                    let a = a.map_err(|_| unreadable())?;
                    let key = a.key.local_name().into_inner().to_owned();
                    let value = a
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|_| unreadable())?;
                    match key.as_str() {
                        // `r:link` (linked picture / media / OLE), VML `o:href`, `href` of
                        // any kind: a reference by name or id to outside content.
                        // ODF `xlink:href` may only point inside the package or at a bookmark.
                        "link" | "href" | "lowsrc" => {
                            let h = value.trim();
                            let internal = odf
                                && !meta
                                && key == "href"
                                && (h.is_empty()
                                    || h.starts_with('#')
                                    || (!h.starts_with('/')
                                        && !h.contains([':', '\\', '%', '&'])
                                        && !h.split('/').any(|s| s == "..")));
                            if !(internal || meta && h.is_empty()) {
                                return Err(refuse("a link to outside content"));
                            }
                        }
                        // ODF spreadsheet formula attribute.
                        "formula" if !formula_ok(&value) => {
                            return Err(refuse("a formula that reaches outside the sheet"));
                        }
                        _ => {}
                    }
                }
            }
            Event::Text(_) | Event::CData(_) | Event::GeneralRef(_) => {
                if in_instr {
                    push_text(&mut field, &ev)?;
                }
                if in_formula {
                    push_text(&mut formula, &ev)?;
                }
            }
            Event::End(e) => match e.local_name().into_inner() {
                "instrText" | "delInstrText" => in_instr = false,
                "f" | "formula" | "formula1" | "formula2" | "definedName" => {
                    in_formula = false;
                    if !formula_ok(&formula) {
                        return Err(refuse("a formula that reaches outside the sheet"));
                    }
                    formula.clear();
                }
                _ => {}
            },
            Event::Eof => {
                return if field_ok(&field) {
                    Ok(())
                } else {
                    Err(refuse("a field that is not plain layout"))
                };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rels(target: &str, mode: &str, kind: &str) -> String {
        format!(
            "<Relationships><Relationship Id=\"1\" Type=\"x/{kind}\" Target=\"{target}\" {mode}/></Relationships>"
        )
    }

    #[test]
    fn rule_no_external_relationship_whatever_its_type() {
        // A plain "hyperlink" type is the disguise: LibreOffice resolves by id.
        for (t, m, k) in [
            (
                "https://example.org",
                "TargetMode=\"External\"",
                "hyperlink",
            ),
            ("\\\\host\\s\\p.png", "TargetMode=\"External\"", "hyperlink"),
            ("x.png", "TargetMode=\"&#69;xternal\"", "image"),
            ("x.png", "TargetMode='external'", "image"),
            ("http://a/p.png", "", "image"), // scheme without a mode
            ("t.dotm", "", "attachedTemplate"),
            ("a.html", "", "aFChunk"),
        ] {
            assert!(scan_rels(rels(t, m, k).as_bytes()).is_err(), "{t} {m} {k}");
        }
        assert!(scan_rels(rels("media/a.png", "", "image").as_bytes()).is_ok());
        assert!(scan_rels(b"<Relationships><Relationship").is_err());
    }

    fn doc(a: &str) -> String {
        format!(
            "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/><w:instrText>{a}</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>"
        )
    }

    #[test]
    fn rule_field_codes_are_allow_listed_and_fully_decoded() {
        assert!(scan_xml(doc(" PAGE ").as_bytes(), "word/document.xml", false).is_ok());
        assert!(
            scan_xml(
                doc(" TOC \\o \"1-3\" \\h").as_bytes(),
                "word/document.xml",
                false
            )
            .is_ok()
        );
        for bad in [
            "DDE x",
            "&#68;DE x",                                      // character reference
            "INCLUDE&#80;ICTURE \"x\"",                       // reference inside the word
            "<![CDATA[INCLUDEPICTURE \"\\\\a\\s\\x.png\"]]>", // CDATA
            "HYPERLINK \"http://a\"",
            "PAGE \"file:///etc/passwd\"",
        ] {
            assert!(
                scan_xml(doc(bad).as_bytes(), "word/document.xml", false).is_err(),
                "{bad}"
            );
        }
        // A custom entity is refused, as is a DOCTYPE.
        assert!(scan_xml(doc("&evil;").as_bytes(), "word/document.xml", false).is_err());
        assert!(
            scan_xml(
                b"<!DOCTYPE x [<!ENTITY a 'b'>]><x/>",
                "word/document.xml",
                false
            )
            .is_err()
        );
    }

    #[test]
    fn rule_attributes_are_matched_by_local_name_not_prefix() {
        for x in [
            "<v:imagedata x:href=\"http://a/p.png\"/>",
            "<a:blip r:link=\"rId9\"/>",
            "<w:attachedTemplate r:id=\"rId1\"/>",
            "<w:altChunk r:id=\"rId1\"/>",
            "<w:subDoc r:id=\"rId1\"/>",
            "<w:mailMerge/>",
        ] {
            assert!(
                scan_xml(x.as_bytes(), "word/document.xml", false).is_err(),
                "{x}"
            );
        }
        assert!(
            scan_xml(
                b"<a:blip r:embed=\"rId9\"/>",
                "ppt/slides/slide1.xml",
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn rule_spreadsheet_formulas_cannot_reach_outside() {
        let sheet =
            |f: &str| format!("<worksheet><sheetData><c><f>{f}</f></c></sheetData></worksheet>");
        for bad in [
            "WEBSERVICE(\"http://attacker/\")",
            "_xlfn.WEBSERVICE(\"file:///C:/secret\")",
            "FILTERXML(A1,\"//a\")",
            "IMAGE(\"https://x/p.png\")",
            "DDE(\"cmd\",\"a\",\"b\")",
            "INDIRECT(\"[x]a!A1\")",
            "&#87;EBSERVICE(A1)",
            "<![CDATA[WEBSERVICE(A1)]]>",
            "'\\\\\\\\host\\\\s'!A1",
        ] {
            assert!(
                scan_xml(sheet(bad).as_bytes(), "xl/worksheets/sheet1.xml", false).is_err(),
                "{bad}"
            );
        }
        assert!(
            scan_xml(
                sheet("SUM(A1:A9)").as_bytes(),
                "xl/worksheets/sheet1.xml",
                false
            )
            .is_ok()
        );
        // ODF `table:formula`.
        let ods =
            "<table:table-cell table:formula=\"of:=COM.MICROSOFT.WEBSERVICE(&quot;x&quot;)\"/>";
        assert!(scan_xml(ods.as_bytes(), "content.xml", true).is_err());
        let ok = "<table:table-cell table:formula=\"of:=SUM([.A1:.A3])\"/>";
        assert!(scan_xml(ok.as_bytes(), "content.xml", true).is_ok());
    }

    #[test]
    fn rule_odf_links_stay_inside_the_package() {
        for ok in ["Pictures/a.png", "#bm", "./Object 1", ""] {
            let x = format!("<draw:image xlink:href=\"{ok}\"/>");
            assert!(scan_xml(x.as_bytes(), "content.xml", true).is_ok(), "{ok}");
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
            let x = format!("<draw:image xlink:href=\"{bad}\"/>");
            assert!(
                scan_xml(x.as_bytes(), "content.xml", true).is_err(),
                "{bad}"
            );
        }
        // Even a plain web hyperlink is refused: it is never needed to convert.
        assert!(scan_xml(b"<text:a xlink:href=\"https://a\"/>", "content.xml", true).is_err());
        assert!(scan_xml(b"<meta:template xlink:href=\"t.ott\"/>", "meta.xml", true).is_err());
        assert!(scan_xml(b"<table:table-source/>", "content.xml", true).is_err());
    }

    type Case<'a> = (&'a str, Vec<(&'a str, &'a [u8])>);

    fn zip_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        for (name, body) in parts {
            zip.start_file(*name, o).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn rule_container_must_be_plain() {
        let ok = [("word/document.xml", &b"<w/>"[..])];
        assert!(vet_zip(&zip_of(&ok)).is_ok());
        let cases: Vec<Case> = vec![
            ("embeddings", vec![("word/embeddings/o.docx", b"x")]),
            ("bin", vec![("word/printerSettings1.bin", b"x")]),
            ("ole", vec![("word/oleObject1", b"x")]),
            ("altchunk", vec![("word/afchunk.htm", b"<html/>")]),
            (
                "nested zip by content",
                vec![("word/media/a.png", b"PK\x03\x04zz")],
            ),
            (
                "nested OLE by content",
                vec![("word/media/a.png", &[0xD0, 0xCF, 0x11, 0xE0, 1])],
            ),
            ("odd type", vec![("word/a.html", b"x")]),
            ("svg", vec![("word/media/a.svg", b"<svg/>")]),
            ("connections", vec![("xl/connections.xml", b"<c/>")]),
            ("macro", vec![("word/vbaProject.bin", b"x")]),
            ("traversal", vec![("../a.xml", b"<a/>")]),
        ];
        for (what, parts) in cases {
            assert!(vet_zip(&zip_of(&parts)).is_err(), "{what}");
        }
    }

    #[test]
    fn rule_duplicate_part_names_are_refused_case_insensitively() {
        // The writer refuses duplicates, so write "a.xml" + "b.xml" and rename the second
        // to "A.xml" in both the local header and the central directory (same length).
        let mut z = zip_of(&[("a.xml", b"<a/>"), ("b.xml", b"<a/>")]);
        let mut n = 0;
        let mut i = 0;
        while i + 5 <= z.len() {
            if &z[i..i + 5] == b"b.xml" {
                z[i] = b'A';
                n += 1;
            }
            i += 1;
        }
        assert_eq!(n, 2);
        assert!(vet_zip(&z).is_err());
    }

    #[test]
    fn rule_rtf_and_unknown_types_are_refused() {
        assert!(vet_source("rtf", b"{\\rtf1 hello}").is_err());
        assert!(vet_source("exe", b"MZ").is_err());
        assert!(vet_source("pdf", b"%PDF-1.4").is_ok());
    }
}
