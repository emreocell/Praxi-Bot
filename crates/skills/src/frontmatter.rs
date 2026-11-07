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
