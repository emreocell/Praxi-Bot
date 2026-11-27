//! In-place editing of Office (OOXML) packages: replace single parts and copy every
//! other part byte for byte, so other sheets, styles, images and macros-free
//! formatting survive an edit untouched.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use tool_registry::ToolError;
use zip::write::SimpleFileOptions;

use crate::common::{clip, exec, invalid};

/// Largest single part this module reads for editing, or writes back.
pub(crate) const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

/// The refusal for an XML part with a `<!DOCTYPE>`: it is never read for editing or copied
/// into a rewritten part, since entity declarations are not part of any Office format.
pub(crate) fn doctype_refused() -> ToolError {
    invalid("invalid xml: DOCTYPE is not allowed")
}

/// Whether `name` is a zip entry name no Office package has: empty, absolute, a drive path,
/// one with a backslash or NUL, or one with an empty, `.` or `..` segment (a trailing `/`
/// marks a directory entry and is fine).
fn unsafe_entry_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.is_empty()
        || name.contains(['\\', '\0'])
        || name.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || name
            .strip_suffix('/')
            .unwrap_or(name)
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
}

/// Refuse a package whose entry names are ambiguous or point outside the package: the same
/// name twice (names are compared without regard to case, as Office does), or an unsafe name.
fn check_entry_names(zip: &zip::ZipArchive<Cursor<&[u8]>>) -> Result<(), ToolError> {
    let repeated = || invalid("not an Office file: the package repeats an entry name");
    if zip.has_duplicate_names() {
        return Err(repeated());
    }
    let mut seen = HashSet::new();
    for name in zip.file_names() {
        let name = name.map_err(|e| invalid(format!("not an Office file: {e}")))?;
        if unsafe_entry_name(&name) {
            return Err(invalid(format!(
                "not an Office file: unsafe entry name {:?}",
                clip(&name, 60)
            )));
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(repeated());
        }
    }
    Ok(())
}

/// Opens the package; one with a repeated or unsafe entry name is refused.
pub(crate) fn open(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, ToolError> {
    let zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| invalid(format!("not an Office file: {e}")))?;
    check_entry_names(&zip)?;
    Ok(zip)
}

/// Lower-case fragments that mark a macro project: its binary (`vbaProject.bin`, in any
/// spelling or numbering, and its signature or rels), the macro-enabled content types and
/// the Excel 4 macro sheets.
const MACRO_MARKERS: [&str; 3] = ["vbaproject", "macroenabled", "macrosheet"];

fn has_macro_marker(text: &str) -> bool {
    let text = text.to_lowercase();
    MACRO_MARKERS.iter().any(|m| text.contains(m))
}

/// The part as UTF-8 text for the scan: UTF-16 (marked by its byte order mark) is decoded, so
/// character references in it are read like any others.
fn as_utf8(part: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let decode = |rest: &[u8], unit: fn([u8; 2]) -> u16| -> Vec<u8> {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| unit(*c)).collect();
        String::from_utf16_lossy(&units).into_bytes()
    };
    match part {
        [0xFF, 0xFE, rest @ ..] => decode(rest, u16::from_le_bytes).into(),
        [0xFE, 0xFF, rest @ ..] => decode(rest, u16::from_be_bytes).into(),
        _ => part.into(),
    }
}

/// Whether an XML part of the package (the content types, a relationships part) names a macro
/// project. The raw text is searched first (with NUL bytes dropped, so UTF-16 without a mark is
/// read too), then every attribute value after unescaping, so a character reference such as
/// `macro&#69;nabled` cannot hide the word.
fn declares_macros(part: &[u8], what: &str) -> Result<bool, ToolError> {
    let flat: Vec<u8> = part.iter().copied().filter(|b| *b != 0).collect();
    if has_macro_marker(&String::from_utf8_lossy(&flat)) {
        return Ok(true);
    }
    let text = as_utf8(part);
    let mut reader = Reader::from_reader(&*text);
    loop {
        match reader
            .read_event()
            .map_err(|e| invalid(format!("invalid {what}: {e}")))?
        {
            Event::Eof => return Ok(false),
            Event::DocType(_) => return Err(doctype_refused()),
            Event::Start(e) | Event::Empty(e) => {
                for a in e.attributes().flatten() {
                    let value = quick_xml::escape::unescape(&a.value)
                        .map_err(|e| invalid(format!("invalid {what}: {e}")))?;
                    if has_macro_marker(&value) {
                        return Ok(true);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Relationship parts looked through for a macro project (the content types are always read).
const MAX_RELS_SCANNED: usize = 256;
/// A relationships part larger than this is not read for the scan (no Office file has one).
const MAX_RELS_BYTES: u64 = 8 * 1024 * 1024;

/// Refuse a package that carries a macro project, whatever its entry is called or how its
/// content type or relationship is spelled. `what` names the kind of file (`documents`,
/// `workbooks`).
pub(crate) fn refuse_macros(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    what: &str,
) -> Result<(), ToolError> {
    let refused = || invalid(format!("{what} with macros are never edited"));
    let mut types = None;
    let mut rels = Vec::new();
    for name in zip.file_names() {
        let name = name.map_err(|e| invalid(format!("not an Office file: {e}")))?;
        if has_macro_marker(&name.replace('\\', "/")) {
            return Err(refused());
        }
        if name.eq_ignore_ascii_case("[Content_Types].xml") {
            types = Some(name.into_owned());
        } else if name.to_ascii_lowercase().ends_with(".rels") && rels.len() < MAX_RELS_SCANNED {
            rels.push(name.into_owned());
        }
    }
    if let Some(types) = types
        && let Some(part) = read_part(zip, &types)?
        && declares_macros(&part, "[Content_Types].xml")?
    {
        return Err(refused());
    }
    for name in rels {
        if zip
            .by_name(&name)
            .is_ok_and(|entry| entry.size() > MAX_RELS_BYTES)
        {
            continue;
        }
        if let Some(part) = read_part(zip, &name)?
            && declares_macros(&part, &name)?
        {
            return Err(refused());
        }
    }
    Ok(())
}

/// One part of the package, or `None` when it is absent.
pub(crate) fn read_part(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<Vec<u8>>, ToolError> {
    let Ok(file) = zip.by_name(name) else {
        return Ok(None);
    };
    let mut buf = Vec::new();
    file.take(MAX_PART_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| exec(format!("cannot read {name}: {e}")))?;
    if buf.len() as u64 > MAX_PART_BYTES {
        return Err(exec(format!("{name} expands beyond the size limit")));
    }
    Ok(Some(buf))
}

/// A copy of the package where the parts in `replace` carry new bytes and the parts in
/// `remove` are gone. Every other part is copied raw (same compression, same bytes). A part
/// of more than [`MAX_PART_BYTES`] is refused before anything is compressed.
pub(crate) fn rewrite(
    bytes: &[u8],
    replace: &HashMap<String, Vec<u8>>,
    remove: &[&str],
) -> Result<Vec<u8>, ToolError> {
    let zerr = |e: zip::result::ZipError| exec(format!("zip error: {e}"));
    if let Some((name, _)) = replace
        .iter()
        .find(|(_, new)| new.len() as u64 > MAX_PART_BYTES)
    {
        return Err(exec(format!(
            "{name} would grow beyond the {MAX_PART_BYTES} byte part limit"
        )));
    }
    let mut src = open(bytes)?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for i in 0..src.len() {
        let entry = src.by_index(i).map_err(zerr)?;
        let name = entry.name().map_err(zerr)?.into_owned();
        if remove.contains(&name.as_str()) {
            continue;
        }
        match replace.get(&name) {
            Some(new) => {
                drop(entry);
                out.start_file(&name, opts).map_err(zerr)?;
                out.write_all(new)
                    .map_err(|e| exec(format!("zip error: {e}")))?;
            }
            None => out.raw_copy_file(entry).map_err(zerr)?,
        }
    }
    Ok(out.finish().map_err(zerr)?.into_inner())
}

/// Value of the attribute whose local name is `key` (`r:id` matches `id`), unescaped.
pub(crate) fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == key)
        .and_then(|a| {
            quick_xml::escape::unescape(&a.value)
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
}

/// `xml` without the empty elements `skip` selects (matched on local name and attributes).
/// A `<!DOCTYPE>` is refused, never copied.
pub(crate) fn remove_empty_elements(
    xml: &[u8],
    skip: impl Fn(&BytesStart<'_>) -> bool,
) -> Result<Vec<u8>, ToolError> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::new());
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| exec(format!("invalid xml: {e}")))?;
        match ev {
            Event::Eof => break,
            Event::DocType(_) => return Err(doctype_refused()),
            Event::Empty(e) if skip(&e) => {}
            other => writer
                .write_event(other)
                .map_err(|e| exec(format!("xml write: {e}")))?,
        }
    }
    Ok(writer.into_inner())
}

/// Plain-text-safe form of `s`: XML 1.0 forbids control characters other than tab, CR and LF.
pub(crate) fn xml_text(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}'))
        .collect()
}

pub(crate) type XmlHashMap = HashMap<String, Vec<u8>>;

/// Package builders shared by the tests of the editing tools.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A package of the given parts, in order.
    pub(crate) fn zip_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        for (name, body) in parts {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// Every part of `bytes` as (name, body), in order.
    pub(crate) fn parts_of(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut f = zip.by_index(i).unwrap();
                let mut body = Vec::new();
                f.read_to_end(&mut body).unwrap();
                (f.name().unwrap().into_owned(), body)
            })
            .collect()
    }

    /// `bytes` repacked after `change` edited its part list (add, replace or drop parts).
    pub(crate) fn repack(
        bytes: &[u8],
        change: impl FnOnce(&mut Vec<(String, Vec<u8>)>),
    ) -> Vec<u8> {
        let mut parts = parts_of(bytes);
        change(&mut parts);
        let refs: Vec<(&str, &[u8])> = parts
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        zip_of(&refs)
    }

    /// Rename the entry `from` to `to` (the same length) without any check. The zip writer
    /// refuses a repeated name and some odd names; this makes both.
    pub(crate) fn force_name(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
        assert_eq!(from.len(), to.len());
        let (from, to) = (from.as_bytes(), to.as_bytes());
        let mut out = bytes.to_vec();
        let mut i = 0;
        let mut hits = 0;
        while i + from.len() <= out.len() {
            if &out[i..i + from.len()] == from {
                out[i..i + from.len()].copy_from_slice(to);
                i += from.len();
                hits += 1;
            } else {
                i += 1;
            }
        }
        assert!(hits >= 2, "the name sits in a local and a central header");
        out
    }

    /// `bytes` plus one more entry that carries the (possibly unsafe or repeated) name `name`.
    pub(crate) fn with_entry_named(bytes: &[u8], name: &str) -> Vec<u8> {
        let placeholder = "q".repeat(name.len());
        let with = repack(bytes, |parts| {
            parts.push((placeholder.clone(), b"<x/>".to_vec()));
        });
        force_name(&with, &placeholder, name)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const TYPES: &[u8] = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

    fn package() -> Vec<u8> {
        zip_of(&[
            ("[Content_Types].xml", TYPES),
            ("word/document.xml", b"<d/>"),
            ("word/styles.xml", b"<s/>"),
        ])
    }

    fn with_types(types: &[u8]) -> Vec<u8> {
        zip_of(&[
            ("[Content_Types].xml", types),
            ("word/document.xml", b"<d/>"),
        ])
    }

    fn macros_refused(bytes: &[u8]) -> bool {
        let mut zip = open(bytes).unwrap();
        refuse_macros(&mut zip, "documents").is_err()
    }

    #[test]
    fn a_plain_package_opens_rewrites_and_has_no_macros() {
        let bytes = package();
        let mut zip = open(&bytes).unwrap();
        refuse_macros(&mut zip, "documents").unwrap();
        let mut replace = XmlHashMap::new();
        replace.insert("word/document.xml".to_owned(), b"<new/>".to_vec());
        let out = rewrite(&bytes, &replace, &[]).unwrap();
        let mut zip = open(&out).unwrap();
        assert_eq!(
            read_part(&mut zip, "word/document.xml").unwrap().unwrap(),
            b"<new/>"
        );
        assert_eq!(
            read_part(&mut zip, "word/styles.xml").unwrap().unwrap(),
            b"<s/>"
        );
        // A directory entry (trailing slash) is an ordinary name.
        let with_dir = {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            zip.add_directory("word/", SimpleFileOptions::default())
                .unwrap();
            zip.start_file("word/document.xml", SimpleFileOptions::default())
                .unwrap();
            zip.finish().unwrap().into_inner()
        };
        open(&with_dir).unwrap();
    }

    #[test]
    fn repeated_and_unsafe_entry_names_are_refused_by_open_and_rewrite() {
        let mut bad: Vec<(String, Vec<u8>)> = Vec::new();
        for name in [
            "../evil.xml",
            "a/../b.xml",
            "..\\x.xml",
            "a\\b.xml",
            "/abs.xml",
            "C:/x.xml",
            "c:x.xml",
            "a/./b.xml",
            "a//b.xml",
        ] {
            bad.push((name.to_owned(), with_entry_named(&package(), name)));
        }
        // The same name twice, exactly and in another case.
        bad.push((
            "word/styles.xml twice".to_owned(),
            with_entry_named(&package(), "word/styles.xml"),
        ));
        bad.push((
            "word/Document.XML".to_owned(),
            with_entry_named(&package(), "word/Document.XML"),
        ));
        for (what, bytes) in bad {
            let err = open(&bytes)
                .err()
                .unwrap_or_else(|| panic!("{what} opened"));
            assert!(
                err.to_string().contains("not an Office file"),
                "{what}: {err}"
            );
            assert!(
                rewrite(&bytes, &XmlHashMap::new(), &[]).is_err(),
                "{what} was rewritten"
            );
        }
    }

    #[test]
    fn macro_projects_are_found_under_any_name() {
        for name in [
            "word/vbaProject.bin",
            "word/VBAProject.bin",
            "xl/vbaProject1.bin",
            "XL/VBAPROJECT.BIN",
            "word/_rels/vbaProject.bin.rels",
            "xl/vbaProjectSignature.bin",
            "xl/macrosheets/sheet1.xml",
        ] {
            assert!(
                macros_refused(&with_entry_named(&package(), name)),
                "{name}"
            );
        }
        assert!(!macros_refused(&with_entry_named(
            &package(),
            "word/media/image1.png"
        )));
    }

    #[test]
    fn macro_enabled_content_types_are_found_in_any_spelling() {
        let types = |ct: &str| {
            format!(
                "<Types xmlns=\"x\"><Override PartName=\"/a.xml\" ContentType=\"{ct}\"/></Types>"
            )
            .into_bytes()
        };
        for ct in [
            "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
            "application/vnd.ms-word.document.macroEnabled.main+xml",
            "application/vnd.ms-word.document.MACROENABLED.main+xml",
            "application/vnd.ms-word.document.macro&#69;nabled.main+xml",
            "application/vnd.ms-word.document.macro&#x45;nabled.main+xml",
            "application/vnd.ms-office.vbaProject",
            "application/vnd.ms-excel.macrosheet+xml",
        ] {
            assert!(macros_refused(&with_types(&types(ct))), "{ct}");
        }
        assert!(!macros_refused(&with_types(&types(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
        ))));
        // Another case of the part name, and UTF-16 text, hide nothing either.
        let upper = zip_of(&[
            ("[CONTENT_TYPES].XML", &types("application/x-macroEnabled")),
            ("word/document.xml", b"<d/>"),
        ]);
        assert!(macros_refused(&upper));
        let utf16: Vec<u8> = "<Types ContentType=\"x.macroEnabled\"/>"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(macros_refused(&with_types(&utf16)));
        // UTF-16 with a byte order mark and a character reference inside the word.
        for (mark, big) in [([0xFF, 0xFE], false), ([0xFE, 0xFF], true)] {
            let mut marked = mark.to_vec();
            marked.extend(
                "<Types><O ContentType=\"x.macro&#69;nabled\"/></Types>"
                    .encode_utf16()
                    .flat_map(|u| {
                        if big {
                            u.to_be_bytes()
                        } else {
                            u.to_le_bytes()
                        }
                    }),
            );
            assert!(macros_refused(&with_types(&marked)), "big={big}");
        }
        // A relationship names the project, though no entry or content type does.
        let rels = zip_of(&[
            ("[Content_Types].xml", TYPES),
            ("word/document.xml", b"<d/>"),
            (
                "word/_rels/document.xml.rels",
                b"<Relationships><Relationship Id=\"r\" Type=\"http://schemas.microsoft.com/office/2006/relationships/vba&#80;roject\" Target=\"x.dat\"/></Relationships>",
            ),
        ]);
        assert!(macros_refused(&rels));
        let doctype = b"<!DOCTYPE Types [<!ENTITY a \"b\">]><Types/>";
        assert!(
            macros_refused(&with_types(doctype)),
            "a DOCTYPE in the content types is refused"
        );
    }

    #[test]
    fn rewrite_refuses_a_part_past_the_size_limit_before_zipping_it() {
        let bytes = package();
        let over = XmlHashMap::from([(
            "word/styles.xml".to_owned(),
            vec![b'a'; usize::try_from(MAX_PART_BYTES).unwrap() + 1],
        )]);
        let err = rewrite(&bytes, &over, &[]).unwrap_err();
        assert!(err.to_string().contains("part limit"), "{err}");
    }

    #[test]
    fn removing_empty_elements_refuses_a_doctype() {
        let xml = b"<!DOCTYPE r [<!ENTITY a \"b\">]><r><k/><keep/></r>";
        let err = remove_empty_elements(xml, |e| e.local_name().as_ref() == "k").unwrap_err();
        assert!(err.to_string().contains("DOCTYPE is not allowed"), "{err}");
        let ok = remove_empty_elements(b"<r><k/><keep/></r>", |e| e.local_name().as_ref() == "k")
            .unwrap();
        assert_eq!(ok, b"<r><keep/></r>");
    }
}
