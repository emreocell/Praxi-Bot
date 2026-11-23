//! Pure helper tools: `time_now` (clock and date arithmetic) and `calculate`
//! (safe arithmetic). Both are deferred tools (found with `tool_search`).
//!
//! Security properties:
//! * Neither touches files, the network, a process, or any secret; the only
//!   inputs are the model's own short strings and the clock. Class `none`
//!   (no untrusted ingestion, no private read, no egress), category `Meta`
//!   so they never need an approval and never taint a run.
//! * `calculate` is a hand-written parser, not `eval` and not a script
//!   engine: numbers, `+ - * / % ^`, parentheses, and a fixed list of
//!   functions. Length, nesting and token counts are bounded, so no input can
//!   hang or recurse without limit. Results that are not finite are errors.
//! * `time_now` reads the system clock and the system time-zone name only;
//!   all other zones come from the bundled IANA database. Every number in a
//!   date adjustment is bounded, and every chrono operation is the checked
//!   variant, so no input can panic.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{
    DateTime, Datelike, LocalResult, Months, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{
    Effect, PermissionCategory, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
};

use crate::util::{parse, spec};

/// Names of the pure tools.
pub const PURE_TOOL_NAMES: [&str; 2] = ["time_now", "calculate"];

/// Longest expression `calculate` accepts, in characters.
pub const MAX_EXPR_CHARS: usize = 500;
const MAX_DEPTH: usize = 32;
const MAX_TOKENS: usize = 400;
/// Largest absolute value of any single `add` field.
const MAX_ADD: i64 = 100_000;
const MAX_ALSO_IN: usize = 6;

// ───────────────────────────── calculate ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    if src.chars().count() > MAX_EXPR_CHARS {
        return Err(format!(
            "expression is longer than {MAX_EXPR_CHARS} characters"
        ));
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if out.len() > MAX_TOKENS {
            return Err("expression has too many parts".into());
        }
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_')
                {
                    i += 1;
                }
                // Optional exponent: e5, E-3.
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
                let n: f64 = text
                    .parse()
                    .map_err(|_| format!("'{text}' is not a valid number"))?;
                out.push(Tok::Num(n));
            }
            'a'..='z' | 'A'..='Z' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                out.push(Tok::Ident(
                    chars[start..i].iter().collect::<String>().to_lowercase(),
                ));
            }
            '+' | '-' | '*' | '/' | '%' | '^' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            other => return Err(format!("character '{other}' is not allowed")),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("expression is nested too deeply".into());
        }
        Ok(())
    }

    // expr := term (('+'|'-') term)*
    fn expr(&mut self) -> Result<f64, String> {
        self.enter()?;
        let mut v = self.term()?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let r = self.term()?;
            v = if c == '+' { v + r } else { v - r };
        }
        self.depth -= 1;
        Ok(v)
    }

    // term := unary (('*'|'/'|'%') unary)*
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        while let Some(Tok::Op(c @ ('*' | '/' | '%'))) = self.peek().cloned() {
            self.pos += 1;
            let r = self.unary()?;
            v = match c {
                '*' => v * r,
                _ if r == 0.0 => return Err("division by zero".into()),
                '/' => v / r,
                _ => v % r,
            };
        }
        Ok(v)
    }

    // unary := ('-'|'+') unary | power
    fn unary(&mut self) -> Result<f64, String> {
        if let Some(Tok::Op(c @ ('-' | '+'))) = self.peek().cloned() {
            self.pos += 1;
            self.enter()?;
            let v = self.unary()?;
            self.depth -= 1;
            return Ok(if c == '-' { -v } else { v });
        }
        self.power()
    }

    // power := atom ('^' unary)?   (right associative; `-2 ^ 2` is -4)
    fn power(&mut self) -> Result<f64, String> {
        let base = self.atom()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            self.enter()?;
            let exp = self.unary()?;
            self.depth -= 1;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<f64, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::LParen) => {
                let v = self.expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err("a closing ')' is missing".into()),
                }
            }
            Some(Tok::Ident(name)) => {
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::RParen) {
                        self.pos += 1;
                    } else {
                        loop {
                            args.push(self.expr()?);
                            match self.next() {
                                Some(Tok::Comma) => {}
                                Some(Tok::RParen) => break,
                                _ => return Err("a closing ')' is missing".into()),
                            }
                            if args.len() > 16 {
                                return Err("too many arguments".into());
                            }
                        }
                    }
                    call(&name, &args)
                } else {
                    match name.as_str() {
                        "pi" => Ok(std::f64::consts::PI),
                        "e" => Ok(std::f64::consts::E),
                        _ => Err(format!("unknown name '{name}'")),
                    }
                }
            }
            Some(Tok::Op(c)) => Err(format!("unexpected '{c}'")),
            Some(Tok::RParen) => Err("unexpected ')'".into()),
            Some(Tok::Comma) => Err("unexpected ','".into()),
            None => Err("the expression ends too early".into()),
        }
    }
}

fn call(name: &str, args: &[f64]) -> Result<f64, String> {
    let one = |f: fn(f64) -> f64| match args {
        [a] => Ok(f(*a)),
        _ => Err(format!("{name} takes exactly one number")),
    };
    match name {
        "sqrt" => match args {
            [a] if *a < 0.0 => Err("sqrt of a negative number".into()),
            _ => one(f64::sqrt),
        },
        "abs" => one(f64::abs),
        "floor" => one(f64::floor),
        "ceil" => one(f64::ceil),
        "round" => match args {
            [a] => Ok(a.round()),
            [a, d] if d.fract() == 0.0 && (0.0..=12.0).contains(d) => {
                // `d` is an integer in 0..=12 here, so the cast is exact.
                #[allow(clippy::cast_possible_truncation)]
                let m = 10f64.powi(*d as i32);
                Ok((a * m).round() / m)
            }
            _ => Err("round takes a number and optionally 0 to 12 decimals".into()),
        },
        "ln" => match args {
            [a] if *a <= 0.0 => Err("ln needs a positive number".into()),
            _ => one(f64::ln),
        },
        "log10" => match args {
            [a] if *a <= 0.0 => Err("log10 needs a positive number".into()),
            _ => one(f64::log10),
        },
        "sin" => one(f64::sin),
        "cos" => one(f64::cos),
        "tan" => one(f64::tan),
        "min" if !args.is_empty() => Ok(args.iter().copied().fold(args[0], f64::min)),
        "max" if !args.is_empty() => Ok(args.iter().copied().fold(args[0], f64::max)),
        "min" | "max" => Err(format!("{name} needs at least one number")),
        _ => Err(format!("unknown function '{name}'")),
    }
}

/// Evaluate `src`.
///
/// # Errors
/// A plain-language message when the expression is invalid, too big, or not finite.
pub fn evaluate(src: &str) -> Result<f64, String> {
    if src.trim().is_empty() {
        return Err("the expression is empty".into());
    }
    let toks = lex(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let v = p.expr()?;
    if p.pos < p.toks.len() {
        return Err("unexpected text after the end of the expression".into());
    }
    if v.is_finite() {
        Ok(v)
    } else {
        Err("the result is too large or not a number".into())
    }
}

/// Plain decimal text: at most 12 decimals, no trailing zeros, and an
/// exponent only for very large or very small values.
#[must_use]
pub fn format_number(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    if v.abs() >= 1e15 || v.abs() < 1e-9 {
        return format!("{v:e}");
    }
    let mut s = format!("{v:.12}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

// ───────────────────────────── time_now ─────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddIn {
    #[serde(default)]
    years: i64,
    #[serde(default)]
    months: i64,
    #[serde(default)]
    weeks: i64,
    #[serde(default)]
    days: i64,
    #[serde(default)]
    hours: i64,
    #[serde(default)]
    minutes: i64,
    #[serde(default)]
    seconds: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeIn {
    timezone: Option<String>,
    at: Option<String>,
    add: Option<AddIn>,
    diff_to: Option<String>,
    #[serde(default)]
    also_in: Vec<String>,
}

fn parse_zone(name: &str) -> Result<Tz, String> {
    name.trim().parse::<Tz>().map_err(|_| {
        format!(
            "'{name}' is not a time zone name; use a name like Europe/Istanbul, America/New_York or UTC"
        )
    })
}

fn default_zone() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|n| n.parse::<Tz>().ok())
        .unwrap_or(Tz::UTC)
}

fn resolve(tz: Tz, naive: NaiveDateTime) -> Result<DateTime<Tz>, String> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Ok(t),
        // A repeated hour at a clock change: take the first one.
        LocalResult::Ambiguous(first, _) => Ok(first),
        LocalResult::None => {
            Err("that local time does not exist in this time zone (the clock skips it)".into())
        }
    }
}

fn parse_when(text: &str, tz: Tz) -> Result<DateTime<Tz>, String> {
    let t = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Ok(dt.with_timezone(&tz));
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(t, fmt) {
            return resolve(tz, n);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return resolve(tz, d.and_hms_opt(0, 0, 0).ok_or("invalid date")?);
    }
    Err(format!(
        "'{t}' is not a date; use 2026-03-31, 2026-03-31 14:30 or 2026-03-31T14:30:00+03:00"
    ))
}

fn apply_add(base: DateTime<Tz>, add: &AddIn) -> Result<DateTime<Tz>, String> {
    for v in [
        add.years,
        add.months,
        add.weeks,
        add.days,
        add.hours,
        add.minutes,
        add.seconds,
    ] {
        if v.abs() > MAX_ADD {
            return Err(format!(
                "each amount must be between -{MAX_ADD} and {MAX_ADD}"
            ));
        }
    }
    let range = || "the result is outside the supported dates".to_string();
    let tz = base.timezone();
    let mut naive = base.naive_local();
    let months = add.years * 12 + add.months;
    if months != 0 {
        let m = Months::new(u32::try_from(months.unsigned_abs()).map_err(|_| range())?);
        naive = if months > 0 {
            naive.checked_add_months(m)
        } else {
            naive.checked_sub_months(m)
        }
        .ok_or_else(range)?;
    }
    let days = add.weeks * 7 + add.days;
    if days != 0 {
        naive = naive
            .checked_add_signed(TimeDelta::try_days(days).ok_or_else(range)?)
            .ok_or_else(range)?;
    }
    let mut out = resolve(tz, naive)?;
    let secs = add.hours * 3600 + add.minutes * 60 + add.seconds;
    if secs != 0 {
        out = out
            .checked_add_signed(TimeDelta::try_seconds(secs).ok_or_else(range)?)
            .ok_or_else(range)?;
    }
    Ok(out)
}

fn describe(label: &str, t: &DateTime<Tz>) -> String {
    format!(
        "{label}: {} ({}, {}, UTC{})",
        t.format("%Y-%m-%d %H:%M:%S"),
        t.format("%A"),
        t.timezone().name(),
        t.format("%:z"),
    )
}

fn span(secs: i64) -> String {
    let sign = if secs < 0 { "-" } else { "" };
    let s = secs.unsigned_abs();
    format!(
        "{sign}{} days {} hours {} minutes {} seconds",
        s / 86_400,
        s % 86_400 / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// Run `time_now` against a fixed clock (tests) or the real one.
fn run_time(input: &TimeIn, now: DateTime<Utc>, default_tz: Tz) -> Result<String, String> {
    if input.also_in.len() > MAX_ALSO_IN {
        return Err(format!("also_in takes at most {MAX_ALSO_IN} zones"));
    }
    let tz = match input.timezone.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(name) => parse_zone(name)?,
        None => default_tz,
    };
    let (mut cur, clock) = match input.at.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(at) => (parse_when(at, tz)?, false),
        None => (now.with_timezone(&tz), true),
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        describe(if clock { "Now" } else { "Start" }, &cur)
    );
    if let Some(add) = &input.add {
        cur = apply_add(cur, add)?;
        let _ = writeln!(out, "{}", describe("Result", &cur));
    }
    let _ = writeln!(
        out,
        "ISO 8601: {}\nUnix seconds: {}\nDay of year: {}, ISO week: {}",
        cur.to_rfc3339(),
        cur.timestamp(),
        cur.ordinal(),
        cur.iso_week().week()
    );
    if let Some(other) = input.diff_to.as_deref().filter(|s| !s.trim().is_empty()) {
        let target = parse_when(other, tz)?;
        let secs = target.signed_duration_since(cur).num_seconds();
        let _ = writeln!(
            out,
            "Until {}: {}",
            target.format("%Y-%m-%d %H:%M"),
            span(secs)
        );
    }
    for name in &input.also_in {
        let z = parse_zone(name)?;
        let _ = writeln!(out, "{}", describe("Also", &cur.with_timezone(&z)));
    }
    Ok(out.trim_end().to_owned())
}

// ───────────────────────────── tool wiring ─────────────────────────────

#[derive(Clone, Copy)]
enum Op {
    Time,
    Calc,
}

struct PureTool {
    spec: ToolSpec,
    op: Op,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CalcIn {
    expression: String,
}

/// Build `time_now` and `calculate`.
#[must_use]
pub fn tools() -> Vec<Arc<dyn Tool>> {
    // Category Meta: pure, no approval, never taints a run.
    let cat = PermissionCategory::Meta;
    vec![
        Arc::new(PureTool {
            spec: spec(
                "time_now",
                "Current date and time in any time zone, plus date arithmetic: add days, months or hours, count the time until a date, or show the same moment in other zones. Use it instead of guessing the date. Example: {\"timezone\":\"Europe/Istanbul\",\"add\":{\"days\":30}}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "timezone":{"type":"string","description":"IANA zone name such as Europe/Berlin. Default: this computer's zone."},
                    "at":{"type":"string","description":"Start moment: 2026-03-31, 2026-03-31 14:30 or an ISO 8601 time. Default: now."},
                    "add":{"type":"object","additionalProperties":false,"description":"Calendar amounts to add (negative to subtract).","properties":{
                        "years":{"type":"integer"},"months":{"type":"integer"},"weeks":{"type":"integer"},"days":{"type":"integer"},
                        "hours":{"type":"integer"},"minutes":{"type":"integer"},"seconds":{"type":"integer"}}},
                    "diff_to":{"type":"string","description":"Another date or time: report how long from the start (after any add) until then."},
                    "also_in":{"type":"array","maxItems":MAX_ALSO_IN,"items":{"type":"string"},"description":"Show the result in these other zones."}
                }}),
                Effect::Read,
                cat,
            ),
            op: Op::Time,
        }),
        Arc::new(PureTool {
            spec: spec(
                "calculate",
                "Exact arithmetic without code: numbers, + - * / % ^, parentheses, pi, e and sqrt, abs, round(x, decimals), floor, ceil, ln, log10, sin, cos, tan, min, max. Use it for sums, percentages and unit maths instead of computing in your head. Example: {\"expression\":\"1250 * 1.18 - 40\"}",
                json!({"type":"object","additionalProperties":false,"properties":{
                    "expression":{"type":"string","maxLength":MAX_EXPR_CHARS,"description":"The expression, for example (3 + 4) * 2 / 7 or 15 / 100 * 480."}
                },"required":["expression"]}),
                Effect::Read,
                cat,
            ),
            op: Op::Calc,
        }),
    ]
}

#[async_trait]
impl Tool for PureTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
        let text = match self.op {
            Op::Time => {
                let i: TimeIn = parse(input)?;
                run_time(&i, Utc::now(), default_zone()).map_err(ToolError::InvalidInput)?
            }
            Op::Calc => {
                let i: CalcIn = parse(input)?;
                let v = evaluate(&i.expression).map_err(ToolError::InvalidInput)?;
                format!("{} = {}", i.expression.trim(), format_number(v))
            }
        };
        Ok(ToolOutput::text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 28, 21, 30, 0).unwrap()
    }

    fn t(input: Value) -> Result<String, String> {
        run_time(&serde_json::from_value(input).unwrap(), fixed(), Tz::UTC)
    }

    #[test]
    fn calculates_with_precedence_and_functions() {
        assert_eq!(format_number(evaluate("1 + 2 * 3").unwrap()), "7");
        assert_eq!(format_number(evaluate("-2 ^ 2").unwrap()), "-4");
        assert_eq!(format_number(evaluate("2 ^ 3 ^ 2").unwrap()), "512");
        assert_eq!(format_number(evaluate("0.1 + 0.2").unwrap()), "0.3");
        assert_eq!(format_number(evaluate("round(10 / 3, 2)").unwrap()), "3.33");
        assert_eq!(format_number(evaluate("max(1, 9, 4) % 4").unwrap()), "1");
        assert_eq!(format_number(evaluate("1_000 * 2e2").unwrap()), "200000");
    }

    #[test]
    fn calculate_refuses_code_and_runaway_input() {
        for bad in [
            "__import__('os')",
            "system(\"calc\")",
            "1; 2",
            "$(id)",
            "x + 1",
            "1 / 0",
            "sqrt(-1)",
            "(1 + 2",
            "1 +",
            "",
            "9 ^ 9 ^ 9 ^ 9",
        ] {
            assert!(evaluate(bad).is_err(), "{bad} must be refused");
        }
        assert!(
            evaluate(&"(".repeat(200)).is_err(),
            "deep nesting is bounded"
        );
        assert!(
            evaluate(&"1+".repeat(300)).is_err(),
            "token count is bounded"
        );
        assert!(evaluate(&"9".repeat(MAX_EXPR_CHARS + 1)).is_err());
    }

    #[test]
    fn time_adds_calendar_amounts_in_the_zone() {
        let r = t(json!({"timezone":"Europe/Istanbul","add":{"days":1,"hours":2}})).unwrap();
        // 21:30 UTC = 00:30 on the 29th in Istanbul (UTC+3); +1 day +2 hours.
        assert!(r.contains("Now: 2026-03-29 00:30:00"), "{r}");
        assert!(r.contains("Result: 2026-03-30 02:30:00"), "{r}");
    }

    #[test]
    fn time_handles_month_end_and_clock_change() {
        let r = t(json!({"at":"2026-01-31","add":{"months":1}})).unwrap();
        assert!(r.contains("Result: 2026-02-28"), "{r}");
        // Berlin skips 02:00-03:00 on 2026-03-29.
        let r = t(json!({"timezone":"Europe/Berlin","at":"2026-03-29 02:30"}));
        assert!(r.is_err());
        // A whole day across the change keeps the wall-clock time.
        let r = t(json!({"timezone":"Europe/Berlin","at":"2026-03-28 12:00","add":{"days":1}}))
            .unwrap();
        assert!(r.contains("Result: 2026-03-29 12:00:00"), "{r}");
    }

    #[test]
    fn time_reports_differences_and_other_zones() {
        let r = t(
            json!({"at":"2026-03-28 00:00","diff_to":"2026-04-01 06:00","also_in":["Asia/Tokyo"]}),
        )
        .unwrap();
        assert!(r.contains("4 days 6 hours"), "{r}");
        assert!(r.contains("Also: 2026-03-28 09:00:00"), "{r}");
    }

    #[test]
    fn time_input_is_bounded_and_strict() {
        assert!(t(json!({"add":{"days":100_001}})).is_err());
        assert!(t(json!({"add":{"years":-100_001}})).is_err());
        assert!(t(json!({"timezone":"Not/AZone"})).is_err());
        assert!(t(json!({"at":"yesterday"})).is_err());
        assert!(
            serde_json::from_value::<TimeIn>(json!({"command":"rm"})).is_err(),
            "unknown fields are rejected"
        );
    }

    #[test]
    fn specs_are_pure_and_never_need_approval_or_taint() {
        for tool in tools() {
            let s = tool.spec();
            assert_eq!(s.category, PermissionCategory::Meta, "{}", s.name);
            assert_eq!(s.effect, Effect::Read);
            let c = s.content;
            assert!(c.is_declared());
            assert!(
                !c.ingests_untrusted && !c.reads_private && !c.egress && !c.executes,
                "{} must be class none",
                s.name
            );
            assert!(s.is_valid());
        }
    }
}
