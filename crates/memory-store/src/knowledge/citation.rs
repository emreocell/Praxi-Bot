//! Citations for hits from selected files and folders: the file path, the PDF
//! page or sheet, and a ready-to-use markdown link the chat can make clickable.
//!
//! Everything here is derived from catalog fields the host controls (source
//! kind and locator, document locator, chunk section and lines), never from a
//! model or from document text. Line numbers refer to the indexed text: they are
//! the original lines for Markdown, plain text and code, and extracted-text
//! lines for PDF, Word and spreadsheet files.

use std::fmt::Write as _;
use std::path::Path;

use uuid::Uuid;

/// Scheme of the link the chat turns into "open this file at this page".
pub const CITATION_SCHEME: &str = "praxi-doc";
/// Suffix the file adapter adds to a document locator.
const EXTRACTED: &str = "#extracted-text";

/// Where a hit came from, in a form people and the chat can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// Full path on the machine that holds the index.
    pub path: String,
    /// One-based PDF page, when the chunk sits under a `Page N` heading of a PDF.
    pub page: Option<u32>,
    /// Markdown link: label with file name and place, target `praxi-doc://...`.
    pub markdown: String,
}

/// Build the citation for a chunk of a file or folder source; `None` for every
/// other source kind (notes, conversations, saved pages, ...).
///
/// `kind` is the stored source kind, with or without JSON quotes.
#[must_use]
pub fn cite(
    source_id: Uuid,
    kind: &str,
    source_locator: &str,
    document_locator: &str,
    section: &str,
    lines: (usize, usize),
) -> Option<Citation> {
    let relative = document_locator
        .strip_suffix(EXTRACTED)
        .unwrap_or(document_locator);
    let (path, relative) = match kind.trim_matches('"') {
        // A file source's locator is the file itself; its document is just the name.
        "file" => (source_locator.to_owned(), file_name(source_locator)?),
        "folder" => (
            Path::new(source_locator)
                .join(relative)
                .display()
                .to_string(),
            relative.to_owned(),
        ),
        _ => return None,
    };
    let name = relative.rsplit('/').next().unwrap_or(&relative);
    let extension = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let last_heading = section.rsplit(" / ").next().unwrap_or_default();
    let page = (extension == "pdf")
        .then(|| {
            last_heading
                .strip_prefix("Page ")?
                .trim()
                .parse::<u32>()
                .ok()
        })
        .flatten();
    let place = match (extension.as_str(), page) {
        ("pdf", Some(page)) => format!("p. {page}"),
        ("xlsx", _) if !last_heading.is_empty() => format!("sheet {}", plain(last_heading)),
        _ if lines.0 == lines.1 => format!("line {}", lines.0),
        _ => format!("lines {}-{}", lines.0, lines.1),
    };
    let mut target = format!("{CITATION_SCHEME}://{source_id}/{}", encode(&relative));
    let _ = write!(target, "?lines={}-{}", lines.0, lines.1);
    if let Some(page) = page {
        let _ = write!(target, "&page={page}");
    }
    Some(Citation {
        markdown: format!("[{}, {place}]({target})", plain(name)),
        path,
        page,
    })
}

fn file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

/// Keep link text from closing or reopening markdown, and drop control text.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '[' | ']' | '(' | ')' | '<' | '>' | '`' | '*' | '\\' => '_',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

/// Percent-encode a relative path, keeping `/` between its segments.
fn encode(relative: &str) -> String {
    let mut out = String::with_capacity(relative.len());
    for byte in relative.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: Uuid = Uuid::from_u128(7);

    #[test]
    fn pdf_page_citation_names_file_page_and_link() {
        let citation = cite(
            SOURCE,
            "\"folder\"",
            "/home/ada/Documents",
            "taxes/2024 return.pdf#extracted-text",
            "/home/ada/Documents/taxes/2024 return.pdf / Page 3",
            (12, 19),
        )
        .unwrap();
        // Built the way the citation builds it, so the separators are the platform's own.
        let expected = Path::new("/home/ada/Documents")
            .join("taxes/2024 return.pdf")
            .display()
            .to_string();
        assert_eq!(citation.path, expected);
        assert_eq!(citation.page, Some(3));
        assert_eq!(
            citation.markdown,
            "[2024 return.pdf, p. 3](praxi-doc://00000000-0000-0000-0000-000000000007/taxes/2024%20return.pdf?lines=12-19&page=3)"
        );
    }

    #[test]
    fn text_files_cite_lines_and_single_lines_read_naturally() {
        let range = cite(
            SOURCE,
            "folder",
            "/d",
            "notes/todo.md#extracted-text",
            "Todo",
            (4, 9),
        )
        .unwrap();
        assert!(range.markdown.starts_with("[todo.md, lines 4-9]("));
        assert_eq!(range.page, None);
        let single = cite(SOURCE, "folder", "/d", "a.txt#extracted-text", "", (5, 5)).unwrap();
        assert!(single.markdown.starts_with("[a.txt, line 5]("));
    }

    #[test]
    fn spreadsheet_citation_names_the_sheet() {
        let citation = cite(
            SOURCE,
            "folder",
            "/d",
            "budget.xlsx#extracted-text",
            "/d/budget.xlsx / Q1 plan",
            (2, 8),
        )
        .unwrap();
        assert!(
            citation
                .markdown
                .starts_with("[budget.xlsx, sheet Q1 plan](")
        );
    }

    #[test]
    fn a_single_file_source_cites_the_file_itself() {
        let citation = cite(
            SOURCE,
            "file",
            "/home/ada/lease.pdf",
            "lease.pdf#extracted-text",
            "/home/ada/lease.pdf / Page 2",
            (1, 3),
        )
        .unwrap();
        assert_eq!(citation.path, "/home/ada/lease.pdf");
        assert!(
            citation
                .markdown
                .contains("praxi-doc://00000000-0000-0000-0000-000000000007/lease.pdf?")
        );
    }

    #[test]
    fn other_source_kinds_have_no_file_citation() {
        for kind in ["note", "saved_web", "conversation", "email", "shared_brain"] {
            assert!(cite(SOURCE, kind, "x", "y", "", (1, 1)).is_none(), "{kind}");
        }
    }

    #[test]
    fn hostile_file_names_cannot_break_out_of_the_link() {
        let citation = cite(
            SOURCE,
            "folder",
            "/d",
            "x](https://evil.example)[y (1).txt#extracted-text",
            "",
            (1, 1),
        )
        .unwrap();
        // Brackets in the name never reach the link text; the target stays encoded.
        let text_end = citation.markdown.find("](praxi-doc://").unwrap();
        assert!(!citation.markdown[1..text_end].contains(['[', ']', '(', ')']));
        assert_eq!(citation.markdown.matches("](").count(), 1);
        assert!(!citation.markdown.contains("https://evil.example"));
        assert!(
            !citation.markdown[text_end + 2..].contains([' ', '(', ')', '[', ']'])
                || citation.markdown.ends_with(')')
        );
    }

    #[test]
    fn non_ascii_names_are_encoded_as_utf8_bytes() {
        let citation = cite(
            SOURCE,
            "folder",
            "/d",
            "Yıllık rapor.txt#extracted-text",
            "",
            (1, 2),
        )
        .unwrap();
        assert!(citation.markdown.contains("Y%C4%B1ll%C4%B1k%20rapor.txt"));
        assert!(
            citation
                .markdown
                .starts_with("[Yıllık rapor.txt, lines 1-2](")
        );
    }
}
