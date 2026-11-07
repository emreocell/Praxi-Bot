//! `csv_read` / `csv_transform`: paged reading and a small filter / select /
//! sort / group pipeline over CSV and TSV files.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, clip, exec, invalid, md_table, parse, read_limited, write_file};

/// An in-memory table; the first CSV record is the header.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

fn delimiter_from(spec: Option<&str>, path: &str, text: &str) -> Result<u8, ToolError> {
    if let Some(s) = spec {
        return match s {
            "," | "comma" => Ok(b','),
            ";" | "semicolon" => Ok(b';'),
            "\t" | "tab" => Ok(b'\t'),
            "|" | "pipe" => Ok(b'|'),
            other => Err(invalid(format!(
                "unsupported delimiter '{other}'; use , ; tab or |"
            ))),
        };
    }
    let lower = path.to_ascii_lowercase();
    if std::path::Path::new(&lower)
        .extension()
        .is_some_and(|e| e == "tsv" || e == "tab")
    {
        return Ok(b'\t');
    }
    let first = text.lines().next().unwrap_or("");
    let best = b",;\t|"
        .iter()
        .copied()
        .max_by_key(|d| first.bytes().filter(|b| b == d).count())
        .unwrap_or(b',');
    Ok(if first.bytes().any(|b| b == best) {
        best
    } else {
        b','
    })
}

/// Column and total-cell caps keep a hostile file from exploding memory.
const MAX_COLS: usize = 10_000;
const MAX_CELLS: usize = 5_000_000;

fn load(cfg: &DocToolsConfig, path: &str, delim: Option<&str>) -> Result<(Table, u8), ToolError> {
    let bytes = read_limited(cfg, path)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let d = delimiter_from(delim, path, text)?;
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(d)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut records = rdr.records();
    let headers: Vec<String> = match records.next() {
        Some(r) => r
            .map_err(|e| exec(format!("csv parse error: {e}")))?
            .iter()
            .map(str::to_string)
            .collect(),
        None => return Err(exec(format!("{path} is empty"))),
    };
    if headers.len() > MAX_COLS {
        return Err(exec(format!("header has more than {MAX_COLS} columns")));
    }
    let mut rows = Vec::new();
    let mut cells = headers.len();
    for r in records {
        let r = r.map_err(|e| exec(format!("csv parse error: {e}")))?;
        if r.len() > MAX_COLS {
            return Err(exec(format!("a row has more than {MAX_COLS} columns")));
        }
        cells += r.len().max(1);
        if cells > MAX_CELLS {
            return Err(exec(format!("table has more than {MAX_CELLS} cells")));
        }
        // Rows are not padded: a missing index reads as "" via `get`.
        rows.push(r.iter().map(str::to_string).collect());
    }
    Ok((Table { headers, rows }, d))
}

fn clipped_rows(cfg: &DocToolsConfig, rows: &[Vec<String>]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| r.iter().map(|c| clip(c, cfg.max_cell_chars)).collect())
        .collect()
}

// ---------------------------------------------------------------- csv_read

#[derive(Deserialize)]
struct ReadIn {
    path: String,
    delimiter: Option<String>,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

pub fn read_spec() -> ToolSpec {
    crate::mk_spec(
        "csv_read",
        "Read a CSV/TSV file with a header row: columns, total row count and a page of rows (offset/limit). Delimiter is auto-detected. Example: {\"path\":\"data.csv\",\"offset\":0,\"limit\":50}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"CSV/TSV file path."},
            "delimiter":{"type":"string","description":"One of , ; tab |. Default: auto."},
            "offset":{"type":"integer","minimum":0,"description":"Rows to skip (default 0)."},
            "limit":{"type":"integer","minimum":1,"description":"Rows to return (default config max_rows)."}
        },"required":["path"]}),
        Effect::Read,
    )
}

pub fn run_read(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: ReadIn = parse(input)?;
    let (t, d) = load(cfg, &i.path, i.delimiter.as_deref())?;
    let limit = i
        .limit
        .unwrap_or(cfg.max_rows)
        .clamp(1, cfg.max_rows.max(1));
    let total = t.rows.len();
    let start = i.offset.min(total);
    let end_idx = start.saturating_add(limit).min(total);
    let page = clipped_rows(cfg, &t.rows[start..end_idx]);
    let end = (i.offset + page.len()).min(total);
    let mut md = format!(
        "# {} ({total} rows, {} columns; showing {}-{})\n\n",
        i.path,
        t.headers.len(),
        if page.is_empty() { 0 } else { i.offset + 1 },
        end
    );
    let mut view = vec![t.headers.clone()];
    view.extend(page.iter().cloned());
    md.push_str(&md_table(&view, true, usize::MAX));
    if end < total {
        let _ = write!(md, "\n_…{} more rows; use offset={end}_\n", total - end);
    }
    let data = json!({"path": i.path, "delimiter": char::from(d).to_string(), "columns": t.headers,
        "total_rows": total, "offset": i.offset, "returned": page.len(), "truncated": end < total, "rows": page});
    Ok(Reply::new(cfg, &md, data))
}

// ---------------------------------------------------------------- csv_transform

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Filter {
        column: String,
        cmp: Cmp,
        #[serde(default)]
        value: Value,
    },
    Select {
        columns: Vec<String>,
    },
    Sort {
        column: String,
        #[serde(default)]
        desc: bool,
    },
    Group {
        by: Vec<String>,
        #[serde(default)]
        aggs: Vec<Agg>,
    },
    Limit {
        n: usize,
    },
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Cmp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
    StartsWith,
    EndsWith,
    Empty,
    NotEmpty,
}

#[derive(Deserialize)]
struct Agg {
    column: Option<String>,
    func: Func,
    #[serde(rename = "as")]
    alias: Option<String>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Func {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl Func {
    fn name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

#[derive(Deserialize)]
struct TransformIn {
    path: String,
    delimiter: Option<String>,
    ops: Vec<Op>,
    /// Write the result as CSV here (never overwrites unless `overwrite`).
    output_path: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

pub fn transform_spec() -> ToolSpec {
    crate::mk_spec(
        "csv_transform",
        "Transform a CSV/TSV with an ordered pipeline of ops: filter {column,cmp:eq|ne|gt|ge|lt|le|contains|starts_with|ends_with|empty|not_empty,value}, select {columns}, sort {column,desc?}, group {by,aggs:[{column?,func:count|sum|avg|min|max,as?}]}, limit {n}. Numbers compare numerically. Returns the result; optionally writes it to output_path (declared as a write tool because of that option). Example: {\"path\":\"s.csv\",\"ops\":[{\"op\":\"filter\",\"column\":\"qty\",\"cmp\":\"gt\",\"value\":2},{\"op\":\"group\",\"by\":[\"city\"],\"aggs\":[{\"column\":\"qty\",\"func\":\"sum\",\"as\":\"total\"}]}]}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Input CSV/TSV path."},
            "delimiter":{"type":"string","description":"One of , ; tab |. Default: auto."},
            "ops":{"type":"array","description":"Ordered operations.","items":{"type":"object","properties":{
                "op":{"type":"string","enum":["filter","select","sort","group","limit"]},
                "column":{"type":"string"},
                "cmp":{"type":"string","enum":["eq","ne","gt","ge","lt","le","contains","starts_with","ends_with","empty","not_empty"]},
                "value":{"description":"Comparison value (string or number)."},
                "columns":{"type":"array","items":{"type":"string"}},
                "desc":{"type":"boolean"},
                "by":{"type":"array","items":{"type":"string"}},
                "aggs":{"type":"array","items":{"type":"object","properties":{
                    "column":{"type":"string"},
                    "func":{"type":"string","enum":["count","sum","avg","min","max"]},
                    "as":{"type":"string"}},"required":["func"]}},
                "n":{"type":"integer","minimum":0}
            },"required":["op"]}},
            "output_path":{"type":"string","description":"Write the result as CSV to this path."},
            "overwrite":{"type":"boolean","description":"Allow replacing output_path."}
        },"required":["path","ops"]}),
        Effect::Write,
    )
}

fn col(t: &Table, name: &str) -> Result<usize, ToolError> {
    t.headers.iter().position(|h| h == name).ok_or_else(|| {
        invalid(format!(
            "unknown column '{name}'; columns: {}",
            t.headers.join(", ")
        ))
    })
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|f| f.is_finite())
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    }
}

fn fmt_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        f.to_string()
    }
}

fn cmp_values(a: &str, b: &str) -> Ordering {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => a.cmp(b),
    }
}

fn matches(cell: &str, cmp: Cmp, value: &str) -> bool {
    match cmp {
        Cmp::Eq => cmp_values(cell, value) == Ordering::Equal,
        Cmp::Ne => cmp_values(cell, value) != Ordering::Equal,
        Cmp::Gt => cmp_values(cell, value) == Ordering::Greater,
        Cmp::Ge => cmp_values(cell, value) != Ordering::Less,
        Cmp::Lt => cmp_values(cell, value) == Ordering::Less,
        Cmp::Le => cmp_values(cell, value) != Ordering::Greater,
        Cmp::Contains => cell.to_lowercase().contains(&value.to_lowercase()),
        Cmp::StartsWith => cell.to_lowercase().starts_with(&value.to_lowercase()),
        Cmp::EndsWith => cell.to_lowercase().ends_with(&value.to_lowercase()),
        Cmp::Empty => cell.trim().is_empty(),
        Cmp::NotEmpty => !cell.trim().is_empty(),
    }
}

fn aggregate(f: Func, values: &[&str], count: usize) -> String {
    let nums: Vec<f64> = values.iter().filter_map(|v| num(v)).collect();
    match f {
        Func::Count => count.to_string(),
        Func::Sum => fmt_num(nums.iter().sum()),
        Func::Avg if nums.is_empty() => String::new(),
        #[allow(clippy::cast_precision_loss)]
        Func::Avg => fmt_num(nums.iter().sum::<f64>() / nums.len() as f64),
        Func::Min => nums
            .iter()
            .copied()
            .reduce(f64::min)
            .map_or_else(String::new, fmt_num),
        Func::Max => nums
            .iter()
            .copied()
            .reduce(f64::max)
            .map_or_else(String::new, fmt_num),
    }
}

fn apply(t: Table, op: &Op) -> Result<Table, ToolError> {
    match op {
        Op::Filter { column, cmp, value } => {
            let c = col(&t, column)?;
            let v = value_text(value);
            if !matches!(cmp, Cmp::Empty | Cmp::NotEmpty) && value.is_null() {
                return Err(invalid("filter needs a value for this cmp"));
            }
            let rows = t
                .rows
                .into_iter()
                .filter(|r| matches(r.get(c).map_or("", String::as_str), *cmp, &v))
                .collect();
            Ok(Table {
                headers: t.headers,
                rows,
            })
        }
        Op::Select { columns } => {
            if columns.is_empty() {
                return Err(invalid("select needs at least one column"));
            }
            let idx: Vec<usize> = columns
                .iter()
                .map(|c| col(&t, c))
                .collect::<Result<_, _>>()?;
            let rows = t
                .rows
                .iter()
                .map(|r| {
                    idx.iter()
                        .map(|i| r.get(*i).cloned().unwrap_or_default())
                        .collect()
                })
                .collect();
            Ok(Table {
                headers: columns.clone(),
                rows,
            })
        }
        Op::Sort { column, desc } => {
            let c = col(&t, column)?;
            let mut rows = t.rows;
            let numeric = rows
                .iter()
                .filter_map(|r| r.get(c))
                .filter(|s| !s.trim().is_empty())
                .all(|s| num(s).is_some());
            rows.sort_by(|a, b| {
                let (x, y) = (
                    a.get(c).map_or("", String::as_str),
                    b.get(c).map_or("", String::as_str),
                );
                // Empty cells always sort last.
                match (x.trim().is_empty(), y.trim().is_empty()) {
                    (true, true) => return Ordering::Equal,
                    (true, false) => return Ordering::Greater,
                    (false, true) => return Ordering::Less,
                    _ => {}
                }
                let o = if numeric { cmp_values(x, y) } else { x.cmp(y) };
                if *desc { o.reverse() } else { o }
            });
            Ok(Table {
                headers: t.headers,
                rows,
            })
        }
        Op::Group { by, aggs } => {
            if by.is_empty() && aggs.is_empty() {
                return Err(invalid("group needs by and/or aggs"));
            }
            let by_idx: Vec<usize> = by.iter().map(|c| col(&t, c)).collect::<Result<_, _>>()?;
            let mut agg_idx = Vec::new();
            let mut headers: Vec<String> = by.clone();
            for a in aggs {
                let idx = match (&a.column, a.func) {
                    (Some(c), _) => Some(col(&t, c)?),
                    (None, Func::Count) => None,
                    (None, f) => return Err(invalid(format!("{} needs a column", f.name()))),
                };
                headers.push(a.alias.clone().unwrap_or_else(|| match &a.column {
                    Some(c) => format!("{}_{c}", a.func.name()),
                    None => a.func.name().to_string(),
                }));
                agg_idx.push(idx);
            }
            let mut order: Vec<Vec<String>> = Vec::new();
            let mut groups: HashMap<Vec<String>, Vec<&Vec<String>>> = HashMap::new();
            for r in &t.rows {
                let key: Vec<String> = by_idx
                    .iter()
                    .map(|i| r.get(*i).cloned().unwrap_or_default())
                    .collect();
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                }
                groups.entry(key).or_default().push(r);
            }
            let rows = order
                .into_iter()
                .map(|key| {
                    let members = &groups[&key];
                    let mut out = key;
                    for (a, idx) in aggs.iter().zip(&agg_idx) {
                        let vals: Vec<&str> = idx.map_or_else(Vec::new, |i| {
                            members
                                .iter()
                                .map(|r| r.get(i).map_or("", String::as_str))
                                .collect()
                        });
                        out.push(aggregate(a.func, &vals, members.len()));
                    }
                    out
                })
                .collect();
            Ok(Table { headers, rows })
        }
        Op::Limit { n } => {
            let mut rows = t.rows;
            rows.truncate(*n);
            Ok(Table {
                headers: t.headers,
                rows,
            })
        }
    }
}

pub fn run_transform(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: TransformIn = parse(input)?;
    if i.ops.is_empty() {
        return Err(invalid("ops must not be empty"));
    }
    let (mut t, d) = load(cfg, &i.path, i.delimiter.as_deref())?;
    let input_rows = t.rows.len();
    for op in &i.ops {
        t = apply(t, op)?;
    }
    let total = t.rows.len();
    let mut written = None;
    if let Some(out) = &i.output_path {
        let mut w = csv::WriterBuilder::new()
            .delimiter(d)
            .from_writer(Vec::new());
        w.write_record(&t.headers)
            .map_err(|e| exec(format!("csv write error: {e}")))?;
        for r in &t.rows {
            w.write_record(r)
                .map_err(|e| exec(format!("csv write error: {e}")))?;
        }
        let bytes = w
            .into_inner()
            .map_err(|e| exec(format!("csv write error: {e}")))?;
        write_file(cfg, out, &bytes, i.overwrite)?;
        written = Some(out.clone());
    }
    let shown = clipped_rows(cfg, &t.rows[..total.min(cfg.max_rows)]);
    let mut md = format!(
        "# {input_rows} rows -> {total} rows ({} columns)\n\n",
        t.headers.len()
    );
    let mut view = vec![t.headers.clone()];
    view.extend(shown.iter().cloned());
    md.push_str(&md_table(&view, true, usize::MAX));
    if total > shown.len() {
        let _ = write!(md, "\n_…{} more rows not shown_\n", total - shown.len());
    }
    if let Some(w) = &written {
        let _ = write!(md, "\nWrote full result to {w}\n");
    }
    let data = json!({"path": i.path, "input_rows": input_rows, "total_rows": total, "columns": t.headers,
        "truncated": total > shown.len(), "rows": shown, "output_path": written});
    Ok(Reply::new(cfg, &md, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALES: &str = "city,item,qty,price\nParis,apple,3,1.5\nRome,pear,10,2\nParis,pear,5,2\nRome,apple,,1.5\nOslo,apple,2,3\n";

    fn fixture(content: &str, name: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        let s = p.to_str().unwrap().to_string();
        (dir, s)
    }

    #[test]
    fn read_pages_and_detects_delimiters() {
        let (_d, p) = fixture(SALES, "s.csv");
        let cfg = DocToolsConfig::default();
        let r = run_read(&cfg, json!({"path": p, "offset": 1, "limit": 2})).unwrap();
        assert_eq!(r.data["total_rows"], 5);
        assert_eq!(r.data["returned"], 2);
        assert_eq!(r.data["rows"][0][1], "pear");
        assert_eq!(r.data["truncated"], true);
        assert!(r.markdown.contains("offset=3"));

        let (_d2, p2) = fixture("\u{feff}a;b\n1;\"x;y\"\n", "semi.csv");
        let r = run_read(&cfg, json!({"path": p2})).unwrap();
        assert_eq!(r.data["delimiter"], ";");
        assert_eq!(r.data["columns"], json!(["a", "b"]));
        assert_eq!(r.data["rows"][0][1], "x;y");

        let (_d3, p3) = fixture("a\tb\n1\t2\n", "t.tsv");
        assert_eq!(
            run_read(&cfg, json!({"path": p3})).unwrap().data["rows"][0][1],
            "2"
        );
    }

    #[test]
    fn filter_sort_select() {
        let (_d, p) = fixture(SALES, "s.csv");
        let cfg = DocToolsConfig::default();
        let r = run_transform(
            &cfg,
            json!({"path": p, "ops": [
                {"op":"filter","column":"qty","cmp":"ge","value":3},
                {"op":"sort","column":"qty","desc":true},
                {"op":"select","columns":["item","qty"]}
            ]}),
        )
        .unwrap();
        assert_eq!(
            r.data["rows"],
            json!([["pear", "10"], ["pear", "5"], ["apple", "3"]])
        );
        assert_eq!(r.data["columns"], json!(["item", "qty"]));
    }

    #[test]
    fn numeric_sort_beats_lexical_and_empty_last() {
        let (_d, p) = fixture(SALES, "s.csv");
        let r = run_transform(
            &DocToolsConfig::default(),
            json!({"path": p, "ops": [{"op":"sort","column":"qty"},{"op":"select","columns":["qty"]}]}),
        )
        .unwrap();
        assert_eq!(r.data["rows"], json!([["2"], ["3"], ["5"], ["10"], [""]]));
    }

    #[test]
    fn group_aggregates() {
        let (_d, p) = fixture(SALES, "s.csv");
        let r = run_transform(
            &DocToolsConfig::default(),
            json!({"path": p, "ops": [{"op":"group","by":["city"],"aggs":[
                {"func":"count"},
                {"column":"qty","func":"sum","as":"total"},
                {"column":"price","func":"avg"},
                {"column":"qty","func":"max"}
            ]}]}),
        )
        .unwrap();
        assert_eq!(
            r.data["columns"],
            json!(["city", "count", "total", "avg_price", "max_qty"])
        );
        assert_eq!(r.data["rows"][0], json!(["Paris", "2", "8", "1.75", "5"]));
        assert_eq!(r.data["rows"][1], json!(["Rome", "2", "10", "1.75", "10"]));
        assert_eq!(r.data["rows"][2], json!(["Oslo", "1", "2", "3", "2"]));
    }

    #[test]
    fn text_filters_and_limit() {
        let (_d, p) = fixture(SALES, "s.csv");
        let cfg = DocToolsConfig::default();
        let r = run_transform(
            &cfg,
            json!({"path": p, "ops": [{"op":"filter","column":"city","cmp":"contains","value":"ar"},{"op":"limit","n":1}]}),
        )
        .unwrap();
        assert_eq!(r.data["total_rows"], 1);
        let r = run_transform(
            &cfg,
            json!({"path": p, "ops": [{"op":"filter","column":"qty","cmp":"empty"}]}),
        )
        .unwrap();
        assert_eq!(r.data["rows"][0][0], "Rome");
    }

    #[test]
    fn writes_output_without_overwriting() {
        let (d, p) = fixture(SALES, "s.csv");
        let out = d.path().join("out.csv").to_str().unwrap().to_string();
        let cfg = DocToolsConfig::default();
        let input = json!({"path": p, "output_path": out, "ops": [{"op":"filter","column":"city","cmp":"eq","value":"Rome"}]});
        run_transform(&cfg, input.clone()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "city,item,qty,price\nRome,pear,10,2\nRome,apple,,1.5\n"
        );
        assert!(run_transform(&cfg, input.clone()).is_err());
        let mut again = input;
        again["overwrite"] = json!(true);
        run_transform(&cfg, again).unwrap();
    }

    #[test]
    fn bad_input_errors() {
        let (_d, p) = fixture(SALES, "s.csv");
        let cfg = DocToolsConfig::default();
        let e = run_transform(
            &cfg,
            json!({"path": p, "ops": [{"op":"sort","column":"nope"}]}),
        )
        .unwrap_err();
        assert!(e.to_string().contains("columns: city"));
        assert!(run_transform(&cfg, json!({"path": p, "ops": []})).is_err());
        assert!(
            run_transform(
                &cfg,
                json!({"path": p, "ops": [{"op":"filter","column":"qty","cmp":"gt"}]})
            )
            .is_err()
        );
        assert!(
            run_transform(
                &cfg,
                json!({"path": p, "ops": [{"op":"group","by":["city"],"aggs":[{"func":"sum"}]}]})
            )
            .is_err()
        );
        assert!(run_read(&cfg, json!({"path": p, "delimiter": "x"})).is_err());
        let small = DocToolsConfig {
            max_input_bytes: 5,
            ..cfg
        };
        assert!(run_read(&small, json!({"path": p})).is_err());
    }

    #[test]
    fn wide_header_is_rejected_and_rows_not_padded() {
        let cfg = DocToolsConfig::default();
        let wide = format!(
            "{}
x
",
            ",".repeat(MAX_COLS + 5)
        );
        let (_d, p) = fixture(&wide, "w.csv");
        assert!(run_read(&cfg, json!({"path": p})).is_err());
        let (_d, p) = fixture(
            "a,b,c
1
2,3
",
            "n.csv",
        );
        let (t, _) = load(&cfg, &p, None).unwrap();
        assert_eq!(t.rows[0], vec!["1".to_string()]);
        let r = run_read(&cfg, json!({"path": p, "offset": 1})).unwrap();
        assert_eq!(r.data["returned"], 1);
    }
}
