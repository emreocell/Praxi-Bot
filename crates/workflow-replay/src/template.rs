//! `{param}` templates.
//!
//! `{name}` is replaced by the parameter value; `{{` and `}}` are literal
//! braces. Anything else containing a brace is an error, so a typo never
//! silently types `{nmae}` into a window.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::WorkflowError;

/// Bound parameter values (already validated against their declared type).
pub type Vars = BTreeMap<String, String>;

enum Piece<'a> {
    Lit(String),
    Var(&'a str),
}

fn parse(t: &str) -> Result<Vec<Piece<'_>>, WorkflowError> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut chars = t.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' => {
                if matches!(chars.peek(), Some((_, '{'))) {
                    chars.next();
                    lit.push('{');
                    continue;
                }
                let rest = &t[i + 1..];
                let Some(end) = rest.find('}') else {
                    return Err(WorkflowError::Template(format!("unclosed `{{` in {t:?}")));
                };
                let name = &rest[..end];
                if !is_ident(name) {
                    return Err(WorkflowError::Template(format!(
                        "invalid parameter reference `{{{name}}}` in {t:?}"
                    )));
                }
                if !lit.is_empty() {
                    out.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                out.push(Piece::Var(name));
                // Skip the name and the closing brace.
                for _ in 0..=name.chars().count() {
                    chars.next();
                }
            }
            '}' => {
                if matches!(chars.peek(), Some((_, '}'))) {
                    chars.next();
                    lit.push('}');
                } else {
                    return Err(WorkflowError::Template(format!(
                        "stray `}}` in {t:?} (write `}}}}` for a literal brace)"
                    )));
                }
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Ok(out)
}

/// Whether `s` is a valid parameter name: `[A-Za-z_][A-Za-z0-9_]*`.
#[must_use]
pub fn is_ident(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Names referenced by a template.
///
/// # Errors
/// Malformed template.
pub fn referenced(t: &str) -> Result<Vec<String>, WorkflowError> {
    Ok(parse(t)?
        .into_iter()
        .filter_map(|p| match p {
            Piece::Var(v) => Some(v.to_owned()),
            Piece::Lit(_) => None,
        })
        .collect())
}

/// Substitute parameters into a template.
///
/// # Errors
/// Malformed template or an unbound parameter.
pub fn render(t: &str, vars: &Vars) -> Result<String, WorkflowError> {
    render_with(t, vars, |v| Ok(v.to_owned()))
}

fn render_with(
    t: &str,
    vars: &Vars,
    quote: impl Fn(&str) -> Result<String, WorkflowError>,
) -> Result<String, WorkflowError> {
    let mut out = String::new();
    for p in parse(t)? {
        match p {
            Piece::Lit(l) => out.push_str(&l),
            Piece::Var(v) => out
                .push_str(&quote(vars.get(v).ok_or_else(|| {
                    WorkflowError::Template(format!("unbound parameter `{v}`"))
                })?)?),
        }
    }
    Ok(out)
}

/// Quoting rules of the shell a command line is handed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellDialect {
    /// `sh` / `bash`.
    Posix,
    /// Windows PowerShell.
    PowerShell,
    /// `cmd.exe`.
    Cmd,
}

impl ShellDialect {
    /// Dialect for a `shell` step / `shell_run` `shell` value (`None` = host default,
    /// the same default `shell_run` uses).
    #[must_use]
    pub fn from_name(name: Option<&str>) -> Self {
        match name.map(str::to_ascii_lowercase).as_deref() {
            Some("bash" | "sh") => Self::Posix,
            Some("cmd") => Self::Cmd,
            Some("powershell") => Self::PowerShell,
            _ => {
                if cfg!(windows) {
                    Self::PowerShell
                } else {
                    Self::Posix
                }
            }
        }
    }

    /// Quote `value` as exactly one literal shell word.
    ///
    /// # Errors
    /// `cmd.exe` has no reliable quoting for `"`, `%`, `!` and line breaks, so
    /// values containing them are refused.
    pub fn quote(self, value: &str) -> Result<String, WorkflowError> {
        match self {
            Self::Posix => Ok(format!("'{}'", value.replace('\'', "'\\''"))),
            Self::PowerShell => {
                // PowerShell also treats the typographic single quotes as delimiters.
                let mut out = String::with_capacity(value.len() + 2);
                out.push('\'');
                for c in value.chars() {
                    out.push(c);
                    if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                        out.push(c);
                    }
                }
                out.push('\'');
                Ok(out)
            }
            Self::Cmd => {
                if value
                    .chars()
                    .any(|c| matches!(c, '"' | '%' | '!' | '\r' | '\n' | '\0'))
                {
                    return Err(WorkflowError::Template(
                        "parameter value cannot be passed safely to cmd.exe (contains one of \" % ! or a line break); use powershell or bash".into(),
                    ));
                }
                let trailing = value.chars().rev().take_while(|c| *c == '\\').count();
                Ok(format!("\"{value}{}\"", "\\".repeat(trailing)))
            }
        }
    }
}

/// Like [`render`], but every substituted parameter value is quoted as one
/// literal word for `dialect`, so a value can never inject extra commands.
/// Write placeholders unquoted in the template: `echo {msg}`, not `echo "{msg}"`.
///
/// # Errors
/// As [`render`], plus values `dialect` cannot quote safely and templates with
/// a placeholder inside a quoted string.
pub fn render_shell(t: &str, vars: &Vars, dialect: ShellDialect) -> Result<String, WorkflowError> {
    reject_quoted_placeholders(t, dialect)?;
    render_with(t, vars, |v| dialect.quote(v))
}

/// Refuse templates where a placeholder sits inside a quoted string of the
/// target shell: our own quoting would then nest in the author's quotes
/// (`echo "{x}"` with `$(id)` still runs the substitution in posix, and an
/// unescaped `&` survives in cmd).
const BACKSLASH: char = '\u{5c}';

fn reject_quoted_placeholders(t: &str, dialect: ShellDialect) -> Result<(), WorkflowError> {
    #[derive(Clone, Copy, PartialEq)]
    enum Q {
        None,
        Single,
        Double,
    }
    let escape = match dialect {
        ShellDialect::Posix => Some(BACKSLASH),
        ShellDialect::PowerShell => Some('`'),
        ShellDialect::Cmd => None,
    };
    let is_single = |c: char| match dialect {
        ShellDialect::Posix => c == '\'',
        ShellDialect::PowerShell => {
            matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
        }
        ShellDialect::Cmd => false,
    };
    // Typographic double quotes are quote characters for PowerShell and are
    // treated as such for cmd.exe too (programs there may re-parse them).
    let is_double = |c: char| {
        c == '"'
            || (dialect != ShellDialect::Posix && matches!(c, '\u{201C}' | '\u{201D}' | '\u{201E}'))
    };
    let mut q = Q::None;
    for p in parse(t)? {
        match p {
            Piece::Var(v) => {
                if q != Q::None {
                    return Err(WorkflowError::Template(format!(
                        "placeholder `{{{v}}}` is inside a quoted string in a shell template; \
                         write it unquoted (values are quoted automatically): {t:?}"
                    )));
                }
            }
            Piece::Lit(l) => {
                let mut chars = l.chars();
                while let Some(c) = chars.next() {
                    match q {
                        Q::Single => {
                            if is_single(c) {
                                q = Q::None;
                            }
                        }
                        Q::Double | Q::None => {
                            if dialect == ShellDialect::Cmd
                                && c == '^'
                                && chars.as_str().starts_with('"')
                            {
                                // `^"` is a quote cmd.exe may or may not honour: treat it
                                // as a quoting context so nothing is substituted in it.
                                chars.next();
                                q = if q == Q::Double { Q::None } else { Q::Double };
                            } else if Some(c) == escape {
                                chars.next();
                            } else if is_double(c) {
                                q = if q == Q::Double { Q::None } else { Q::Double };
                            } else if q == Q::None && is_single(c) {
                                q = Q::Single;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Escape literal text so [`render`] returns it unchanged.
#[must_use]
pub fn escape_literal(s: &str) -> String {
    s.replace('{', "{{").replace('}', "}}")
}

/// Render every string inside a JSON value. A string that is exactly one
/// `{param}` whose declared type is number or bool becomes a typed JSON value.
///
/// # Errors
/// As [`render`].
pub fn render_value(
    v: &Value,
    vars: &Vars,
    typed: &BTreeMap<String, crate::format::ParamType>,
) -> Result<Value, WorkflowError> {
    use crate::format::ParamType;
    Ok(match v {
        Value::String(s) => {
            let whole = s
                .strip_prefix('{')
                .and_then(|r| r.strip_suffix('}'))
                .filter(|n| is_ident(n));
            if let Some(name) = whole {
                let raw = vars.get(name).ok_or_else(|| {
                    WorkflowError::Template(format!("unbound parameter `{name}`"))
                })?;
                match typed.get(name) {
                    Some(ParamType::Number) => {
                        if let Ok(i) = raw.parse::<i64>() {
                            return Ok(Value::from(i));
                        }
                        if let Some(n) = raw
                            .parse::<f64>()
                            .ok()
                            .and_then(serde_json::Number::from_f64)
                        {
                            return Ok(Value::Number(n));
                        }
                    }
                    Some(ParamType::Bool) => {
                        if let Ok(b) = raw.parse::<bool>() {
                            return Ok(Value::Bool(b));
                        }
                    }
                    _ => {}
                }
                Value::String(raw.clone())
            } else {
                Value::String(render(s, vars)?)
            }
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| render_value(x, vars, typed))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), render_value(x, vars, typed)?)))
                .collect::<Result<_, WorkflowError>>()?,
        ),
        other => other.clone(),
    })
}

/// Names referenced anywhere inside a JSON value's strings.
///
/// # Errors
/// Malformed template.
pub fn referenced_in_value(v: &Value, out: &mut Vec<String>) -> Result<(), WorkflowError> {
    match v {
        Value::String(s) => out.extend(referenced(s)?),
        Value::Array(a) => {
            for x in a {
                referenced_in_value(x, out)?;
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                referenced_in_value(x, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vars {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn substitutes_and_unescapes() {
        let v = vars(&[("name", "Ada")]);
        assert_eq!(render("Hi {name}! {{ok}}", &v).unwrap(), "Hi Ada! {ok}");
    }

    #[test]
    fn rejects_unbound_unclosed_and_stray() {
        let v = vars(&[]);
        assert!(render("{x}", &v).is_err());
        assert!(render("{x", &v).is_err());
        assert!(render("a}b", &v).is_err());
        assert!(render("{not valid}", &v).is_err());
    }

    #[test]
    fn escape_round_trips() {
        let lit = "{\"a\": {1}} }";
        assert_eq!(render(&escape_literal(lit), &vars(&[])).unwrap(), lit);
    }

    #[test]
    fn referenced_lists_names() {
        assert_eq!(referenced("{a} and {b} {{c}}").unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn cmd_caret_quote_and_typographic_quotes_are_quoting_contexts() {
        let v = vars(&[("x", "a")]);
        for t in [
            "echo ^\"{x}^\"",
            "echo ^\"{x}",
            "echo \u{201C}{x}\u{201D}",
            "echo \u{201E}{x}",
        ] {
            assert!(render_shell(t, &v, ShellDialect::Cmd).is_err(), "{t}");
        }
        assert!(render_shell("echo \u{201C}{x}\u{201D}", &v, ShellDialect::PowerShell).is_err());
        assert!(render_shell("echo {x}", &v, ShellDialect::Cmd).is_ok());
    }

    #[test]
    fn shell_templates_with_quoted_placeholders_are_rejected() {
        let v = vars(&[("x", "$(id)")]);
        for (t, d) in [
            ("echo \"{x}\"", ShellDialect::Posix),
            ("echo 'a {x}'", ShellDialect::Posix),
            ("echo \"a\" \"b {x}\"", ShellDialect::Posix),
            ("echo \"{x}\"", ShellDialect::Cmd),
            ("echo \"{x}\"", ShellDialect::PowerShell),
            ("echo '{x}'", ShellDialect::PowerShell),
        ] {
            assert!(render_shell(t, &v, d).is_err(), "{t} {d:?}");
        }
        // Closed quotes, escaped quotes and quotes inside literals are fine.
        assert!(render_shell("echo \"a\" {x}", &v, ShellDialect::Posix).is_ok());
        assert!(render_shell("echo \\\" {x}", &v, ShellDialect::Posix).is_ok());
        assert!(render_shell("echo \"it's\" {x}", &v, ShellDialect::Posix).is_ok());
        assert!(render_shell("echo '\"' {x}", &v, ShellDialect::Posix).is_ok());
        assert!(render_shell("echo --k={x}", &v, ShellDialect::Posix).is_ok());
    }

    #[test]
    fn shell_rendering_quotes_values() {
        let v = vars(&[("x", "a'b; rm -rf /")]);
        assert_eq!(
            render_shell("echo {x}", &v, ShellDialect::Posix).unwrap(),
            "echo 'a'\\''b; rm -rf /'"
        );
        assert_eq!(
            render_shell("echo {x}", &v, ShellDialect::PowerShell).unwrap(),
            "echo 'a''b; rm -rf /'"
        );
        assert_eq!(
            render_shell(
                "echo {x}",
                &vars(&[("x", "\u{2019}; calc")]),
                ShellDialect::PowerShell
            )
            .unwrap(),
            "echo '\u{2019}\u{2019}; calc'"
        );
        assert!(
            render_shell("echo {x}", &vars(&[("x", "a\" & calc")]), ShellDialect::Cmd).is_err()
        );
        assert_eq!(
            render_shell("echo {x}", &vars(&[("x", "a & b")]), ShellDialect::Cmd).unwrap(),
            "echo \"a & b\""
        );
    }
}
