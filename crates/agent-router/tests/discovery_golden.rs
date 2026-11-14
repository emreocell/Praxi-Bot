//! Tool-discovery golden-query baseline (task 039).
//!
//! Ranks a frozen set of Turkish / English queries against a frozen tool corpus
//! with the two lexical discovery indexes that exist today:
//!
//! * `ToolIndex` (what `find_tool` uses): stemming-lite + EN/TR synonyms + BM25.
//! * K2 `Corpus` in BM25-only mode (`NullEmbedder`, no query embedding).
//!
//! Run `cargo test -p agent-router --test discovery_golden -- --nocapture` to
//! print the metrics table. The floor assertions sit slightly below the measured
//! baseline so that a ranking regression fails CI. When ranking *improves*,
//! raise the floors deliberately; never edit the fixture to make a number pass.

#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeMap;
use std::time::Instant;

use agent_router::ToolIndex;
use agent_router::ladder::{Candidate, CandidateKind, Corpus, Embedder, NullEmbedder, Scoring};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, PermissionCategory, Platform, ToolSpec};

const FIXTURE: &str = include_str!("fixtures/discovery_golden.json");
/// Cut-off for recall.
const RECALL_K: usize = 5;
/// How many times the whole query set is hits to average the timing.
const TIMING_PASSES: usize = 10;

#[derive(Deserialize)]
struct Fixture {
    tools: Vec<FxTool>,
    queries: Vec<FxQuery>,
}

#[derive(Deserialize)]
struct FxTool {
    name: String,
    description: String,
    params: Vec<String>,
    effect: String,
}

#[derive(Deserialize)]
struct FxQuery {
    id: String,
    split: String,
    lang: String,
    query: String,
    expect: Vec<String>,
}

fn effect(s: &str) -> Effect {
    match s {
        "read" => Effect::Read,
        "write" => Effect::Write,
        "destructive" => Effect::Destructive,
        other => panic!("unknown effect {other}"),
    }
}

fn to_spec(t: &FxTool) -> ToolSpec {
    let props: serde_json::Map<String, Value> = t
        .params
        .iter()
        .map(|p| (p.clone(), json!({ "type": "string" })))
        .collect();
    ToolSpec {
        name: t.name.clone(),
        description: t.description.clone(),
        input_schema: json!({ "type": "object", "properties": props }),
        effect: effect(&t.effect),
        category: PermissionCategory::Meta,
        platforms: vec![Platform::All],
        content: tool_registry::ContentClass::none(),
    }
}

/// Best-first ranking of tool names for a query.
trait Ranker {
    fn name(&self) -> &'static str;
    fn rank(&self, query: &str) -> Vec<String>;
}

struct ToolIndexRanker {
    index: ToolIndex,
    k: usize,
}

impl Ranker for ToolIndexRanker {
    fn name(&self) -> &'static str {
        "ToolIndex"
    }
    fn rank(&self, query: &str) -> Vec<String> {
        self.index
            .search(query, self.k)
            .into_iter()
            .map(|h| h.name)
            .collect()
    }
}

struct CorpusRanker {
    corpus: Corpus,
}

impl Ranker for CorpusRanker {
    fn name(&self) -> &'static str {
        "Corpus(BM25)"
    }
    fn rank(&self, query: &str) -> Vec<String> {
        // BM25-only: no query embedding.
        self.corpus
            .rank(query, None, Scoring::default())
            .into_iter()
            .filter_map(|s| self.corpus.get(s.index).map(|c| c.name.clone()))
            .collect()
    }
}

/// Running sums for one (index, group).
#[derive(Default, Clone)]
struct Acc {
    /// Queries with at least one expected tool.
    n: usize,
    top1: usize,
    recall: usize,
    mrr: f64,
    /// Confusable queries (non-destructive expectation) where a destructive tool won.
    destructive_picked: usize,
    /// `none` queries.
    n_none: usize,
    /// `none` queries that returned no hit at all.
    none_empty: usize,
}

impl Acc {
    fn pct(&self, x: usize) -> f64 {
        if self.n == 0 {
            f64::NAN
        } else {
            100.0 * x as f64 / self.n as f64
        }
    }
    fn top1_pct(&self) -> f64 {
        self.pct(self.top1)
    }
    fn recall_pct(&self) -> f64 {
        self.pct(self.recall)
    }
    fn mrr_pct(&self) -> f64 {
        if self.n == 0 {
            f64::NAN
        } else {
            100.0 * self.mrr / self.n as f64
        }
    }
    fn none_empty_pct(&self) -> f64 {
        if self.n_none == 0 {
            f64::NAN
        } else {
            100.0 * self.none_empty as f64 / self.n_none as f64
        }
    }
}

struct Miss {
    index: &'static str,
    id: String,
    query: String,
    expect: Vec<String>,
    rank: Option<usize>,
    top3: Vec<String>,
}

struct Report {
    /// group key -> accumulator, e.g. `all`, `lang:tr`, `split:seen`.
    groups: BTreeMap<String, Acc>,
    mean_micros: f64,
    misses: Vec<Miss>,
}

fn evaluate(ranker: &dyn Ranker, fx: &Fixture, effects: &BTreeMap<&str, Effect>) -> Report {
    let mut groups: BTreeMap<String, Acc> = BTreeMap::new();
    let mut misses = Vec::new();
    for q in &fx.queries {
        let hits = ranker.rank(&q.query);
        let keys = [
            "all".to_owned(),
            format!("lang:{}", q.lang),
            format!("split:{}", q.split),
        ];
        if q.expect.is_empty() {
            for k in &keys {
                let a = groups.entry(k.clone()).or_default();
                a.n_none += 1;
                a.none_empty += usize::from(hits.is_empty());
            }
            continue;
        }
        let rank = hits
            .iter()
            .position(|n| q.expect.contains(n))
            .map(|p| p + 1);
        let top1 = rank == Some(1);
        let destructive_picked = q.split == "confusable"
            && q.expect
                .iter()
                .all(|e| effects[e.as_str()] != Effect::Destructive)
            && hits
                .first()
                .is_some_and(|n| effects[n.as_str()] == Effect::Destructive);
        for k in &keys {
            let a = groups.entry(k.clone()).or_default();
            a.n += 1;
            a.top1 += usize::from(top1);
            a.recall += usize::from(rank.is_some_and(|r| r <= RECALL_K));
            a.mrr += rank.map_or(0.0, |r| 1.0 / r as f64);
            a.destructive_picked += usize::from(destructive_picked);
        }
        if !top1 {
            misses.push(Miss {
                index: ranker.name(),
                id: q.id.clone(),
                query: q.query.clone(),
                expect: q.expect.clone(),
                rank,
                top3: hits.iter().take(3).cloned().collect(),
            });
        }
    }
    // Timing: rank the whole set several times.
    let start = Instant::now();
    for _ in 0..TIMING_PASSES {
        for q in &fx.queries {
            std::hint::black_box(ranker.rank(std::hint::black_box(&q.query)));
        }
    }
    let mean_micros =
        start.elapsed().as_secs_f64() * 1e6 / (TIMING_PASSES * fx.queries.len()) as f64;
    Report {
        groups,
        mean_micros,
        misses,
    }
}

fn fmt(x: f64) -> String {
    if x.is_nan() {
        "  n/a".to_owned()
    } else {
        format!("{x:5.1}")
    }
}

fn print_report(name: &str, r: &Report) {
    println!(
        "\n== {name}  (mean ranking time {:.1} us/query)",
        r.mean_micros
    );
    println!(
        "{:<18} {:>4} {:>6} {:>8} {:>6} {:>10} {:>9}",
        "group", "n", "top1%", "recall@5", "MRR", "destr.pick", "none-empty%"
    );
    for (key, a) in &r.groups {
        println!(
            "{:<18} {:>4} {:>6} {:>8} {:>6} {:>10} {:>9}",
            key,
            a.n,
            fmt(a.top1_pct()),
            fmt(a.recall_pct()),
            fmt(a.mrr_pct()),
            a.destructive_picked,
            fmt(a.none_empty_pct()),
        );
    }
}

/// Minimum metric values (percent) per index and group, set slightly below the
/// measured baseline (about 2 points of slack, 1 query is ~1.7 points overall).
/// `(index, group, top1, recall@5, mrr)`.
const FLOORS: &[(&str, &str, f64, f64, f64)] = &[
    ("ToolIndex", "all", 53.0, 76.0, 65.0),
    ("ToolIndex", "lang:en", 62.0, 86.0, 73.0),
    ("ToolIndex", "lang:tr", 43.0, 52.0, 49.0),
    ("ToolIndex", "lang:tr_ascii", 26.0, 69.0, 48.0),
    ("ToolIndex", "split:seen", 86.0, 92.0, 88.0),
    ("ToolIndex", "split:paraphrase", 25.0, 52.0, 40.0),
    ("ToolIndex", "split:confusable", 56.0, 98.0, 75.0),
    ("Corpus(BM25)", "all", 51.0, 69.0, 59.0),
    ("Corpus(BM25)", "lang:en", 65.0, 86.0, 74.0),
    ("Corpus(BM25)", "lang:tr", 34.0, 43.0, 38.0),
    ("Corpus(BM25)", "lang:tr_ascii", 12.0, 26.0, 19.0),
    ("Corpus(BM25)", "split:seen", 92.0, 92.0, 92.0),
    ("Corpus(BM25)", "split:paraphrase", 20.0, 34.0, 25.0),
    ("Corpus(BM25)", "split:confusable", 48.0, 98.0, 71.0),
];

/// Maximum number of confusable queries where a destructive tool wins top-1
/// although the expected tool is not destructive: `(index, max)`.
const MAX_DESTRUCTIVE_PICKED: &[(&str, usize)] = &[("ToolIndex", 3), ("Corpus(BM25)", 5)];

/// Minimum percentage of `none` queries that must return no hit: `(index, min)`.
const MIN_NONE_EMPTY_PCT: &[(&str, f64)] = &[("ToolIndex", 48.0), ("Corpus(BM25)", 64.0)];

#[test]
fn discovery_golden_baseline() {
    let fx: Fixture = serde_json::from_str(FIXTURE).expect("fixture parses");
    assert!(fx.tools.len() >= 50, "corpus too small: {}", fx.tools.len());
    assert!(
        (50..=60).contains(&fx.queries.len()),
        "query count {}",
        fx.queries.len()
    );
    let effects: BTreeMap<&str, Effect> = fx
        .tools
        .iter()
        .map(|t| (t.name.as_str(), effect(&t.effect)))
        .collect();
    for q in &fx.queries {
        for e in &q.expect {
            assert!(
                effects.contains_key(e.as_str()),
                "{}: unknown tool {e}",
                q.id
            );
        }
        assert!(
            ["seen", "paraphrase", "confusable", "none"].contains(&q.split.as_str()),
            "{}: bad split",
            q.id
        );
        assert_eq!(q.expect.is_empty(), q.split == "none", "{}", q.id);
    }
    assert!(!NullEmbedder.is_enabled());

    let specs: Vec<ToolSpec> = fx.tools.iter().map(to_spec).collect();
    let candidates: Vec<Candidate> = fx
        .tools
        .iter()
        .map(|t| Candidate::new(CandidateKind::Tool, &t.name, &t.description))
        .collect();
    let rankers: Vec<Box<dyn Ranker>> = vec![
        Box::new(ToolIndexRanker {
            index: ToolIndex::build(&specs),
            k: specs.len(),
        }),
        Box::new(CorpusRanker {
            corpus: Corpus::new(candidates),
        }),
    ];

    let mut all_misses: Vec<Miss> = Vec::new();
    let mut reports = Vec::new();
    for ranker in &rankers {
        let report = evaluate(ranker.as_ref(), &fx, &effects);
        print_report(ranker.name(), &report);
        reports.push((ranker.name(), report));
    }
    for (_, report) in &mut reports {
        all_misses.append(&mut report.misses);
    }
    // Worst misses: not found at all first, then lowest rank.
    all_misses.sort_by_key(|m| std::cmp::Reverse(m.rank.unwrap_or(usize::MAX)));
    println!("\n== worst misses (top-1 wrong), both indexes");
    for m in all_misses.iter().take(10) {
        println!(
            "[{}] {} {:?} expect={:?} rank={} top3={:?}",
            m.index,
            m.id,
            m.query,
            m.expect,
            m.rank.map_or("-".to_owned(), |r| r.to_string()),
            m.top3
        );
    }

    let get = |index: &str| {
        &reports
            .iter()
            .find(|(n, _)| *n == index)
            .unwrap_or_else(|| panic!("no report for {index}"))
            .1
    };
    for &(index, group, top1, recall, mrr) in FLOORS {
        let a = &get(index).groups[group];
        assert!(
            a.top1_pct() >= top1,
            "{index} {group} top1 {:.1} < {top1}",
            a.top1_pct()
        );
        assert!(
            a.recall_pct() >= recall,
            "{index} {group} recall@5 {:.1} < {recall}",
            a.recall_pct()
        );
        assert!(
            a.mrr_pct() >= mrr,
            "{index} {group} MRR {:.1} < {mrr}",
            a.mrr_pct()
        );
    }
    for &(index, max) in MAX_DESTRUCTIVE_PICKED {
        let got = get(index).groups["all"].destructive_picked;
        assert!(got <= max, "{index}: destructive picked {got} > {max}");
    }
    for &(index, min) in MIN_NONE_EMPTY_PCT {
        let got = get(index).groups["all"].none_empty_pct();
        assert!(got >= min, "{index}: none-empty {got:.1} < {min}");
    }
}
