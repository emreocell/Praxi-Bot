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
    let mut out = String::new();
    for p in parse(t)? {
        match p {
            Piece::Lit(l) => out.push_str(&l),
            Piece::Var(v) => out.push_str(
                vars.get(v)
                    .ok_or_else(|| WorkflowError::Template(format!("unbound parameter `{v}`")))?,
            ),
        }
    }
    Ok(out)
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
}
