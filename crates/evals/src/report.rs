//! `bot eval report`: a markdown before/after table from two result files.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::metrics::RunRecord;

/// Mean of the runs of one task in one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Agg {
    /// Number of runs.
    pub runs: u32,
    /// Successful runs.
    pub passed: u32,
    /// Mean wall time in seconds.
    pub wall_s: f64,
    /// Mean model calls.
    pub llm_calls: f64,
    /// Mean prompt tokens.
    pub input_tokens: f64,
    /// Mean completion tokens.
    pub output_tokens: f64,
    /// Mean cached tokens.
    pub cached_tokens: f64,
    /// Mean cost (`None` when no run had a price).
    pub cost_usd: Option<f64>,
    /// Mean tool calls (all tools).
    pub tool_calls: f64,
    /// Mean perception tree / ocr / grid.
    pub perception: (f64, f64, f64),
}

/// Group records by name and average them.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn aggregate(records: &[RunRecord]) -> BTreeMap<String, Agg> {
    let mut groups: BTreeMap<String, Vec<&RunRecord>> = BTreeMap::new();
    for r in records {
        groups.entry(r.name.clone()).or_default().push(r);
    }
    groups
        .into_iter()
        .map(|(name, rs)| {
            let n = rs.len() as f64;
            let mean = |f: &dyn Fn(&RunRecord) -> f64| rs.iter().map(|r| f(r)).sum::<f64>() / n;
            let costs: Vec<f64> = rs.iter().filter_map(|r| r.cost_usd).collect();
            let agg = Agg {
                runs: u32::try_from(rs.len()).unwrap_or(u32::MAX),
                passed: u32::try_from(rs.iter().filter(|r| r.success).count()).unwrap_or(u32::MAX),
                wall_s: mean(&|r| r.wall_ms as f64 / 1000.0),
                llm_calls: mean(&|r| f64::from(r.llm_calls)),
                input_tokens: mean(&|r| r.input_tokens as f64),
                output_tokens: mean(&|r| r.output_tokens as f64),
                cached_tokens: mean(&|r| r.cached_tokens as f64),
                cost_usd: (!costs.is_empty())
                    .then(|| costs.iter().sum::<f64>() / costs.len() as f64),
                tool_calls: mean(&|r| r.tool_calls.values().map(|v| f64::from(*v)).sum()),
                perception: (
                    mean(&|r| f64::from(r.perception.tree)),
                    mean(&|r| f64::from(r.perception.ocr)),
                    mean(&|r| f64::from(r.perception.grid)),
                ),
            };
            (name, agg)
        })
        .collect()
}

fn num(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn pair(a: Option<f64>, b: Option<f64>, fmt: fn(f64) -> String) -> String {
    match (a, b) {
        (Some(a), Some(b)) => {
            let d = if a.abs() < f64::EPSILON {
                if b.abs() < f64::EPSILON {
                    String::new()
                } else {
                    " (new)".into()
                }
            } else {
                let pct = (b - a) / a * 100.0;
                if pct.abs() < 0.5 {
                    " (=)".into()
                } else {
                    format!(" ({pct:+.0}%)")
                }
            };
            format!("{} → {}{d}", fmt(a), fmt(b))
        }
        (Some(a), None) => format!("{} → -", fmt(a)),
        (None, Some(b)) => format!("- → {}", fmt(b)),
        (None, None) => "-".into(),
    }
}

fn usd(v: f64) -> String {
    format!("${v:.4}")
}

fn rate(a: Option<&Agg>) -> Option<String> {
    a.map(|a| format!("{}/{}", a.passed, a.runs))
}

/// Render the comparison of `before` and `after` as markdown.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn render(
    before: &[RunRecord],
    after: &[RunRecord],
    before_label: &str,
    after_label: &str,
) -> String {
    let base = aggregate(before);
    let next = aggregate(after);
    let mut names: Vec<&String> = base.keys().chain(next.keys()).collect();
    names.sort();
    names.dedup();

    let mut out = String::new();
    let _ = writeln!(out, "# Eval report: {before_label} → {after_label}\n");
    let _ = writeln!(
        out,
        "| Task | Pass | Wall (s) | LLM calls | Input tok | Output tok | Cached tok | Cost | Tool calls | Perception tree/ocr/grid |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|");
    for n in &names {
        let (was, now) = (base.get(*n), next.get(*n));
        let f = |g: fn(&Agg) -> f64| pair(was.map(g), now.map(g), num);
        let pass = match (rate(was), rate(now)) {
            (Some(p), Some(q)) => format!("{p} → {q}"),
            (Some(p), None) => format!("{p} → -"),
            (None, Some(q)) => format!("- → {q}"),
            (None, None) => "-".into(),
        };
        let per = |g: &Agg| {
            format!(
                "{}/{}/{}",
                num(g.perception.0),
                num(g.perception.1),
                num(g.perception.2)
            )
        };
        let _ = writeln!(
            out,
            "| {n} | {pass} | {} | {} | {} | {} | {} | {} | {} | {} → {} |",
            pair(was.map(|g| g.wall_s), now.map(|g| g.wall_s), |v| format!(
                "{v:.2}"
            )),
            f(|g| g.llm_calls),
            f(|g| g.input_tokens),
            f(|g| g.output_tokens),
            f(|g| g.cached_tokens),
            pair(
                was.and_then(|g| g.cost_usd),
                now.and_then(|g| g.cost_usd),
                usd
            ),
            f(|g| g.tool_calls),
            was.map_or_else(|| "-".into(), per),
            now.map_or_else(|| "-".into(), per),
        );
    }

    // Totals over the tasks present in both files.
    let both: Vec<&str> = names
        .iter()
        .filter(|n| base.contains_key(n.as_str()) && next.contains_key(n.as_str()))
        .map(|n| n.as_str())
        .collect();
    let sum = |m: &BTreeMap<String, Agg>, g: fn(&Agg) -> f64| -> f64 {
        both.iter().map(|n| g(&m[*n])).sum()
    };
    let total = |g: fn(&Agg) -> f64, fmt: fn(f64) -> String| {
        pair(Some(sum(&base, g)), Some(sum(&next, g)), fmt)
    };
    let passed = |m: &BTreeMap<String, Agg>| -> (u32, u32) {
        both.iter()
            .fold((0, 0), |(p, r), n| (p + m[*n].passed, r + m[*n].runs))
    };
    let (bp, br) = passed(&base);
    let (ap, ar) = passed(&next);
    let _ = writeln!(
        out,
        "\n**Totals over {} task(s) present in both runs**\n",
        both.len()
    );
    let _ = writeln!(out, "- Pass: {bp}/{br} → {ap}/{ar}");
    let _ = writeln!(
        out,
        "- Wall (s): {}",
        total(|g| g.wall_s, |v| format!("{v:.2}"))
    );
    let _ = writeln!(out, "- LLM calls: {}", total(|g| g.llm_calls, num));
    let _ = writeln!(out, "- Input tokens: {}", total(|g| g.input_tokens, num));
    let _ = writeln!(out, "- Output tokens: {}", total(|g| g.output_tokens, num));
    let _ = writeln!(out, "- Tool calls: {}", total(|g| g.tool_calls, num));
    let bc: Vec<f64> = both.iter().filter_map(|n| base[*n].cost_usd).collect();
    let ac: Vec<f64> = both.iter().filter_map(|n| next[*n].cost_usd).collect();
    if !bc.is_empty() || !ac.is_empty() {
        let _ = writeln!(
            out,
            "- Cost: {}",
            pair(
                (!bc.is_empty()).then(|| bc.iter().sum()),
                (!ac.is_empty()).then(|| ac.iter().sum()),
                usd
            )
        );
    }

    let regressions: Vec<&&str> = both
        .iter()
        .filter(|n| next[**n].passed < base[**n].passed)
        .collect();
    if !regressions.is_empty() {
        let _ = writeln!(out, "\n**Regressions (fewer passes than before):**");
        for n in regressions {
            let _ = writeln!(out, "- {n}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Perception;

    fn rec(name: &str, ok: bool, wall: u64, calls: u32, tin: u64, cost: Option<f64>) -> RunRecord {
        RunRecord {
            schema: 1,
            ts_ms: 0,
            tier: 1,
            name: name.into(),
            model: "fake/m".into(),
            success: ok,
            failures: vec![],
            state: "succeeded".into(),
            error: None,
            wall_ms: wall,
            llm_calls: calls,
            input_tokens: tin,
            output_tokens: 10,
            cached_tokens: 0,
            cost_usd: cost,
            tool_calls: BTreeMap::from([("t".to_owned(), calls)]),
            perception: Perception::default(),
        }
    }

    #[test]
    fn aggregate_averages_repeats() {
        let a = aggregate(&[
            rec("x", true, 1000, 2, 100, None),
            rec("x", false, 3000, 4, 300, None),
        ]);
        let x = &a["x"];
        assert_eq!((x.runs, x.passed), (2, 1));
        assert!((x.wall_s - 2.0).abs() < 1e-9);
        assert!((x.llm_calls - 3.0).abs() < 1e-9);
        assert!((x.input_tokens - 200.0).abs() < 1e-9);
        assert!(x.cost_usd.is_none());
    }

    #[test]
    fn report_shows_deltas_regressions_and_missing_tasks() {
        let before = [
            rec("a", true, 2000, 4, 400, Some(0.04)),
            rec("b", true, 1000, 2, 100, None),
        ];
        let after = [
            rec("a", false, 1000, 2, 200, Some(0.02)),
            rec("c", true, 500, 1, 50, None),
        ];
        let md = render(&before, &after, "before", "after");
        assert!(md.starts_with("# Eval report: before → after"));
        assert!(
            md.contains("| a | 1/1 → 0/1 | 2.00 → 1.00 (-50%) | 4 → 2 (-50%) | 400 → 200 (-50%)"),
            "{md}"
        );
        assert!(md.contains("$0.0400 → $0.0200 (-50%)"), "{md}");
        assert!(md.contains("| b | 1/1 → - |"), "{md}");
        assert!(md.contains("| c | - → 1/1 |"), "{md}");
        assert!(md.contains("Totals over 1 task(s)"), "{md}");
        assert!(md.contains("Regressions"), "{md}");
        assert!(md.contains("- a"), "{md}");
    }
}
