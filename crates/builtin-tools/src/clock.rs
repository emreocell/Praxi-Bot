//! `time_now` and `calculate`: the clock and exact arithmetic.
//!
//! Models guess the date and miscalculate; these two tools answer from the
//! machine clock and a small hand-written expression evaluator. Both are pure:
//! no files, no network, no private content. Category `Meta` (always allowed,
//! no approval) with an empty content class, so they never taint a run and a
//! tainted run is never asked about them.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, Offset, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{exec_err, parse, spec};

/// Names of the clock tools.
pub const CLOCK_TOOL_NAMES: [&str; 2] = ["time_now", "calculate"];

/// Longest expression `calculate` accepts, in characters.
pub const MAX_EXPR_CHARS: usize = 500;
const MAX_DEPTH: usize = 48;

#[derive(Clone, Copy)]
enum Op {
    Time,
    Calc,
}

struct ClockTool {
    spec: ToolSpec,
    op: Op,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeIn {
    timezone: Option<String>,
    at: Option<String>,
    #[serde(default)]
    add_minutes: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CalcIn {
    expression: String,
}

/// Build `time_now` and `calculate`.
#[must_use]
pub fn tools() -> Vec<Arc<dyn Tool>> {
    let cat = PermissionCategory::Meta;
    vec![
        Arc::new(ClockTool {
            spec: spec(
                "time_now",
                "Current date and time from the computer's clock, optionally in another IANA timezone, for a given instant, or shifted by minutes. Use it instead of guessing the date. Example: {\"timezone\":\"Europe/Berlin\"} or {\"at\":\"2026-03-01T09:00:00+00:00\",\"timezone\":\"Asia/Tokyo\",\"add_minutes\":90}",
                json!({"type":"object","properties":{
                    "timezone":{"type":"string","maxLength":64,"description":"IANA name such as Europe/Istanbul. Default: the computer's local zone."},
                    "at":{"type":"string","maxLength":64,"description":"RFC 3339 instant to convert instead of now."},
                    "add_minutes":{"type":"integer","minimum":-5_256_000,"maximum":5_256_000,"description":"Shift the instant by this many minutes."}
                },"additionalProperties":false}),
                Effect::Read,
                cat,
            ),
            op: Op::Time,
        }),
        Arc::new(ClockTool {
            spec: spec(
                "calculate",
                "Evaluate an arithmetic expression exactly (no code runs): + - * / % ^, parentheses, sqrt abs round floor ceil ln log10 sin cos tan min max, constants pi and e. Use it for any sum, percentage or unit math. Example: {\"expression\":\"1250 * 1.2 / 3\"}",
                json!({"type":"object","required":["expression"],"properties":{
                    "expression":{"type":"string","maxLength":MAX_EXPR_CHARS}
                },"additionalProperties":false}),
                Effect::Read,
                cat,
            ),
            op: Op::Calc,
        }),
    ]
}

#[async_trait]
impl Tool for ClockTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.op {
            Op::Time => {
                let i: TimeIn = parse(input)?;
                time_now(&i)
            }
            Op::Calc => {
                let i: CalcIn = parse(input)?;
                if i.expression.chars().count() > MAX_EXPR_CHARS {
                    return Err(ToolError::InvalidInput("expression is too long".into()));
                }
                let v = eval(&i.expression).map_err(ToolError::InvalidInput)?;
                if !v.is_finite() {
                    return Err(exec_err("the result is not a finite number"));
                }
                Ok(ToolOutput::text(fmt_num(v)))
            }
        }
    }
}

fn time_now(i: &TimeIn) -> Result<ToolOutput, ToolError> {
    let base: DateTime<Utc> = match &i.at {
        Some(at) => DateTime::parse_from_rfc3339(at.trim())
            .map_err(|e| ToolError::InvalidInput(format!("`at` is not RFC 3339: {e}")))?
            .with_timezone(&Utc),
        None => Utc::now(),
    };
    let shifted = base
        .checked_add_signed(Duration::minutes(i.add_minutes))
        .ok_or_else(|| ToolError::InvalidInput("the shifted time is out of range".into()))?;
    let (rfc, zone, weekday) = if let Some(name) = &i.timezone {
        let tz: chrono_tz::Tz = name
            .trim()
            .parse()
            .map_err(|_| ToolError::InvalidInput(format!("unknown timezone {name:?}")))?;
        let t = shifted.with_timezone(&tz);
        (t.to_rfc3339(), name.trim().to_owned(), t.weekday())
    } else {
        let off: FixedOffset = shifted.with_timezone(&Local).offset().fix();
        let t = off.from_utc_datetime(&shifted.naive_utc());
        (t.to_rfc3339(), "local".to_owned(), t.weekday())
    };
    Ok(ToolOutput::text(format!(
        "{rfc}\nweekday: {weekday}\ntimezone: {zone}\nunix: {}\nutc: {}",
        shifted.timestamp(),
        shifted.to_rfc3339()
    )))
}

#[allow(clippy::float_cmp)]
fn fmt_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.10}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

/// Evaluate an arithmetic expression. Pure; no variables, no I/O.
fn eval(src: &str) -> Result<f64, String> {
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.expr()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected character at position {}", p.i + 1));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("expression is nested too deeply".into());
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<f64, String> {
        self.enter()?;
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                break;
            }
        }
        self.depth -= 1;
        Ok(v)
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                let d = self.unary()?;
                if d == 0.0 {
                    return Err("division by zero".into());
                }
                v /= d;
            } else if self.eat(b'%') {
                let d = self.unary()?;
                if d == 0.0 {
                    return Err("division by zero".into());
                }
                v %= d;
            } else {
                break;
            }
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<f64, String> {
        if self.eat(b'-') {
            self.enter()?;
            let v = self.unary()?;
            self.depth -= 1;
            return Ok(-v);
        }
        if self.eat(b'+') {
            self.enter()?;
            let v = self.unary()?;
            self.depth -= 1;
            return Ok(v);
        }
        self.power()
    }

    fn power(&mut self) -> Result<f64, String> {
        let base = self.atom()?;
        if self.eat(b'^') {
            self.enter()?;
            let exp = self.unary()?; // right associative
            self.depth -= 1;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<f64, String> {
        self.ws();
        let start = self.i;
        match self.s.get(self.i) {
            Some(b'(') => {
                self.i += 1;
                let v = self.expr()?;
                if !self.eat(b')') {
                    return Err("missing )".into());
                }
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || *c == b'.' => {
                while self
                    .s
                    .get(self.i)
                    .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i])
                    .unwrap_or_default()
                    .parse::<f64>()
                    .map_err(|_| format!("bad number at position {}", start + 1))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                while self.s.get(self.i).is_some_and(u8::is_ascii_alphanumeric) {
                    self.i += 1;
                }
                let name = std::str::from_utf8(&self.s[start..self.i])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                match name.as_str() {
                    "pi" => return Ok(std::f64::consts::PI),
                    "e" => return Ok(std::f64::consts::E),
                    _ => {}
                }
                if !self.eat(b'(') {
                    return Err(format!("unknown name {name:?}"));
                }
                let mut args = vec![self.expr()?];
                while self.eat(b',') {
                    args.push(self.expr()?);
                    if args.len() > 8 {
                        return Err("too many arguments".into());
                    }
                }
                if !self.eat(b')') {
                    return Err("missing )".into());
                }
                call(&name, &args)
            }
            _ => Err(format!("unexpected character at position {}", start + 1)),
        }
    }
}

fn call(name: &str, a: &[f64]) -> Result<f64, String> {
    let one = |f: fn(f64) -> f64| {
        if a.len() == 1 {
            Ok(f(a[0]))
        } else {
            Err(format!("{name} takes one argument"))
        }
    };
    match name {
        "sqrt" => one(f64::sqrt),
        "abs" => one(f64::abs),
        "round" => one(f64::round),
        "floor" => one(f64::floor),
        "ceil" => one(f64::ceil),
        "ln" => one(f64::ln),
        "log10" => one(f64::log10),
        "sin" => one(f64::sin),
        "cos" => one(f64::cos),
        "tan" => one(f64::tan),
        "min" => Ok(a.iter().copied().fold(f64::INFINITY, f64::min)),
        "max" => Ok(a.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
        _ => Err(format!("unknown function {name:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tool_registry::ContentPart;

    fn run(name: &str, input: Value) -> Result<String, ToolError> {
        let t = tools().into_iter().find(|t| t.spec().name == name).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let o = rt.block_on(t.call(&ToolContext::default(), input))?;
        let ContentPart::Text { text } = &o.content[0] else {
            panic!()
        };
        Ok(text.clone())
    }

    #[test]
    fn calculates_with_precedence_and_rejects_bad_input() {
        let c = |e: &str| run("calculate", json!({"expression": e}));
        assert_eq!(c("1250 * 1.2 / 3").unwrap(), "500");
        assert_eq!(c("2 ^ 3 ^ 2").unwrap(), "512");
        assert_eq!(c("-(2 + 3) * max(1, 4)").unwrap(), "-20");
        assert!(c("1 / 0").is_err());
        assert!(c("system('x')").is_err());
        assert!(c(&"(".repeat(200)).is_err());
        assert!(c("10 ^ 10 ^ 10").is_err(), "infinite result is refused");
    }

    #[test]
    fn time_converts_zones_and_rejects_unknown() {
        let t = run(
            "time_now",
            json!({"at":"2026-03-01T09:00:00+00:00","timezone":"Asia/Tokyo","add_minutes":90}),
        )
        .unwrap();
        assert!(t.starts_with("2026-03-01T19:30:00+09:00"), "{t}");
        assert!(t.contains("weekday: Sun"));
        assert!(run("time_now", json!({"timezone":"Mars/Base"})).is_err());
        assert!(run("time_now", json!({})).unwrap().contains("unix:"));
    }

    #[test]
    fn clock_tools_are_pure() {
        for t in tools() {
            let c = t.spec().content;
            assert!(!c.egress && !c.reads_private && !c.ingests_untrusted);
        }
    }
}
