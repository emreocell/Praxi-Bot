//! In-place editing of Office (OOXML) packages: replace single parts and copy every
//! other part byte for byte, so other sheets, styles, images and macros-free
//! formatting survive an edit untouched.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use tool_registry::ToolError;
use zip::write::SimpleFileOptions;

use crate::common::{exec, invalid};

/// Largest single part this module reads for editing.
pub(crate) const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) fn open(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, ToolError> {
    zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| invalid(format!("not an Office file: {e}")))
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
/// `remove` are gone. Every other part is copied raw (same compression, same bytes).
pub(crate) fn rewrite(
    bytes: &[u8],
    replace: &HashMap<String, Vec<u8>>,
    remove: &[&str],
) -> Result<Vec<u8>, ToolError> {
    let zerr = |e: zip::result::ZipError| exec(format!("zip error: {e}"));
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
