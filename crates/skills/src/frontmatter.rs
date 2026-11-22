//! Minimal frontmatter parser for `SKILL.md`.
//!
//! Supports the YAML subset skills need: `key: value` scalars (optionally
//! quoted), inline lists `[a, b]`, and one nested block (`when:`) with
//! indented `key: value` lines. Anything else is a validation error.

/// A parsed frontmatter value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Scalar(String),
    List(Vec<String>),
}

/// Parsed frontmatter: top-level entries and the entries of the `when` block.
#[derive(Debug, Default)]
pub(crate) struct Front {
    pub top: Vec<(String, Value)>,
    pub when: Vec<(String, Value)>,
}

/// Split `content` into `(frontmatter_text, body)`.
pub(crate) fn split(content: &str) -> Result<(String, String), String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("missing frontmatter: file must start with a `---` line".into());
    }
    let mut front = Vec::new();
    let mut closed = false;
    let mut body = Vec::new();
    for line in lines {
        if closed {
            body.push(line);
        } else if line.trim_end() == "---" {
            closed = true;
        } else {
            front.push(line);
        }
    }
    if !closed {
        return Err("unterminated frontmatter: missing closing `---` line".into());
    }
    Ok((front.join("\n"), body.join("\n").trim().to_string()))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    // Owner form input uses JSON-compatible double-quoted YAML scalars. Decode
    // those escapes while retaining the legacy minimal-parser behavior for
    // older non-JSON quoted values.
    if s.starts_with('"')
        && let Ok(decoded) = serde_json::from_str::<String>(s)
    {
        return decoded;
    }
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

fn parse_value(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| format!("unterminated list: {raw}"))?;
        let mut items = Vec::new();
        let mut cur = String::new();
        let mut quote: Option<char> = None;
        for c in inner.chars() {
            match quote {
                Some(q) if c == q => {
                    quote = None;
                    cur.push(c);
                }
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                None if c == ',' => {
                    items.push(std::mem::take(&mut cur));
                }
                Some(_) | None => cur.push(c),
            }
        }
        if quote.is_some() {
            return Err(format!("unterminated quote in list: {raw}"));
        }
        items.push(cur);
        let items: Vec<String> = items
            .iter()
            .map(|i| unquote(i))
            .filter(|i| !i.is_empty())
            .collect();
        return Ok(Value::List(items));
    }
    Ok(Value::Scalar(unquote(raw)))
}

/// Parse the frontmatter text (without the `---` fences).
pub(crate) fn parse(text: &str) -> Result<Front, String> {
    let mut front = Front::default();
    let mut in_when = false;
    for (n, line) in text.lines().enumerate() {
        let ln = n + 2; // 1-based, plus the opening fence
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let (key, value) = line
            .trim()
            .split_once(':')
            .ok_or_else(|| format!("line {ln}: expected `key: value`"))?;
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(format!("line {ln}: empty key"));
        }
        if indented {
            if !in_when {
                return Err(format!("line {ln}: unexpected indentation"));
            }
            if front.when.iter().any(|(k, _)| *k == key) {
                return Err(format!("line {ln}: duplicate key `{key}` in `when`"));
            }
            front.when.push((
                key,
                parse_value(value).map_err(|e| format!("line {ln}: {e}"))?,
            ));
        } else {
            in_when = false;
            if front.top.iter().any(|(k, _)| *k == key) {
                return Err(format!("line {ln}: duplicate key `{key}`"));
            }
            if key == "when" && value.trim().is_empty() {
                in_when = true;
                continue;
            }
            front.top.push((
                key,
                parse_value(value).map_err(|e| format!("line {ln}: {e}"))?,
            ));
        }
    }
    Ok(front)
}

#[cfg(test)]
mod property_tests {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::sample::select;

    use super::{Value, parse, split};

    fn unicode_text() -> impl Strategy<Value = String> {
        const SYNTAX_CHARS: &[char] = &[
            '\n', '\r', '\t', '\0', '\u{feff}', '#', '-', ':', '[', ']', ',', '\'', '"', '\\',
        ];
        let character = prop_oneof![4 => any::<char>(), 1 => select(SYNTAX_CHARS)];
        vec(character, 0..256).prop_map(|chars| chars.into_iter().collect())
    }

    // Proptest defaults to 256 cases and honors PROPTEST_CASES and PROPTEST_RNG_SEED.
    proptest! {
        /// Both entry points tolerate arbitrary valid UTF-8, including text inside fences and lists.
        #[test]
        fn arbitrary_utf8_does_not_panic(input in unicode_text()) {
            let _ = split(&input);
            let _ = parse(&input);
            let (header, _) = split(&format!("---\n{input}\n---\n")).unwrap();
            let _ = parse(&header);
            let _ = parse(&format!("when:\n  apps: [{input}]"));
        }

        /// Valid fences preserve header lines and trim the body after normalizing line endings, with or without a BOM.
        #[test]
        fn split_normalizes_fenced_documents(
            header_line in "[^\r\n]{0,96}",
            body_line in "[^\r\n]{0,96}",
            bom in select(&["", "\u{feff}"] as &[_]),
            eol in select(&["\n", "\r\n"] as &[_]),
        ) {
            let header = format!("\nname: example\n  # {header_line}\t\n");
            let body = format!("# Body\n  {body_line} \n---");
            // Split joins lines with LF and trims outer body whitespace, so rejoining
            // its output can lose information from the original document.
            let document = format!("{bom}--- \t\n{header}\n---\t\n \t\u{2003}\n{body}\n\u{2003}\t ");
            let document = document.replace('\n', eol);
            prop_assert_eq!(split(&document).unwrap(), (header, body));
        }

        /// A malformed opening or closing fence leaves the corresponding delimiter missing.
        #[test]
        fn split_rejects_malformed_delimiters(
            malformed in select(&["", "--", "----", " ---", "--- trailing"] as &[_]),
            text in "[^\r\n]{0,96}",
        ) {
            // Split requires an opening fence on the first line, even when a later line is a fence.
            let missing_opener = format!("{malformed}\n---\n# {text}");
            let missing_closer = format!("---\n# {text}\n{malformed}\n");
            prop_assert!(split(&missing_opener).unwrap_err().starts_with("missing frontmatter:"));
            prop_assert!(split(&missing_closer).unwrap_err().starts_with("unterminated frontmatter:"));
        }

        /// JSON-quoted scalars decode to their original text, including whitespace and escape characters.
        #[test]
        fn parse_decodes_quoted_scalars(text in unicode_text()) {
            let quoted = serde_json::to_string(&text).unwrap();
            let front = parse(&format!("title:  {quoted}\t")).unwrap();
            prop_assert_eq!(front.top, [("title".to_owned(), Value::Scalar(text))]);
            prop_assert_eq!(front.when, []);
        }

        /// Inline lists retain order and quoted commas, skip empty items, and respect the `when` scope.
        #[test]
        fn parse_keeps_list_order_and_when_scope(
            items in vec("[a-z0-9_.-]{1,16}", 0..8),
            first in "[a-z]{1,12}",
            second in "[a-z]{1,12}",
            indent in select(&["  ", "\t"] as &[_]),
        ) {
            let single_quoted = format!("{first},{second}");
            let double_quoted = format!("{second},{first}");
            let plain = items.join(", ");
            let input = format!(
                "apps: []\nwhen:\n{indent}apps: [{plain}, '{single_quoted}', \"{double_quoted}\", '', ]\ntitle: '{first}'"
            );
            let front = parse(&input).unwrap();
            let mut expected = items;
            expected.extend([single_quoted, double_quoted]);
            prop_assert_eq!(front.top, [
                ("apps".to_owned(), Value::List(Vec::new())),
                ("title".to_owned(), Value::Scalar(first)),
            ]);
            prop_assert_eq!(front.when, [("apps".to_owned(), Value::List(expected))]);
            let after_when = format!("{input}\n{indent}keywords: [extra]");
            prop_assert!(parse(&after_when).unwrap_err().contains("unexpected indentation"));
        }

        /// Duplicate keys are rejected within either scope even when key spacing differs.
        #[test]
        fn parse_rejects_duplicate_keys(key in "key_[\\p{L}0-9]{1,12}") {
            let top = format!("{key}: one\n{key} \t: two");
            let nested = format!("when:\n  {key}: one\n\t{key} : two");
            let duplicate = format!("duplicate key `{key}`");
            let duplicate_in_when = format!("{duplicate} in `when`");
            prop_assert!(parse(&top).unwrap_err().contains(&duplicate));
            prop_assert!(parse(&nested).unwrap_err().contains(&duplicate_in_when));
        }

        /// Inline lists reject a missing closing bracket or an unmatched quote.
        #[test]
        fn parse_rejects_unterminated_lists(item in "[a-z0-9_.-]{1,16}") {
            let missing_bracket = format!("apps: [{item}");
            let missing_quote = format!("apps: ['{item}]");
            prop_assert!(parse(&missing_bracket).unwrap_err().contains("unterminated list"));
            prop_assert!(parse(&missing_quote).unwrap_err().contains("unterminated quote in list"));
        }
    }

    /// Empty input yields no entries, and long fenced Unicode input preserves its scalar and body.
    #[test]
    fn empty_and_long_inputs() {
        assert!(split("").is_err());
        assert_eq!(split("---\n---").unwrap(), (String::new(), String::new()));
        let empty = parse("").unwrap();
        assert_eq!(empty.top, []);
        assert_eq!(empty.when, []);

        let value = "İ東京💥".repeat(16_384);
        let header = format!("title: {value}");
        let body = format!("# Body\n{value}");
        let (actual_header, actual_body) = split(&format!("---\n{header}\n---\n{body}\n")).unwrap();
        assert_eq!(actual_header, header);
        assert_eq!(actual_body, body);
        let front = parse(&actual_header).unwrap();
        assert_eq!(front.top, [("title".to_owned(), Value::Scalar(value))]);
        assert_eq!(front.when, []);
    }
}
