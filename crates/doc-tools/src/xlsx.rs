//! `xlsx_read` / `xlsx_write`: spreadsheets with typed cells.
//!
//! Reading uses `calamine` (xlsx, xlsm, xlsb, xls); writing uses
//! `rust_xlsxwriter` (xlsx only).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Cursor;

use calamine::{Data, Reader, open_workbook_auto_from_rs};
use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, clip, exec, invalid, md_table, parse, read_limited, write_file};

const MAX_WRITE_CELLS: usize = 2_000_000;

// ---------------------------------------------------------------- reading

#[derive(Deserialize)]
struct ReadIn {
    path: String,
    /// Sheet name (default: first sheet). Use `"*"`-less listing via `list_only`.
    sheet: Option<String>,
    /// A1-style range such as `B2:D20` or a single cell `C3`.
    range: Option<String>,
    /// Only list sheet names and dimensions.
    #[serde(default)]
    list_only: bool,
    /// Treat the first row of the range as a header in the markdown preview.
    #[serde(default = "yes")]
    header: bool,
    /// Also return formulas (A1 -> "=...") for the selected range.
    #[serde(default)]
    formulas: bool,
}

fn yes() -> bool {
    true
}

pub fn read_spec() -> ToolSpec {
    crate::mk_spec(
        "xlsx_read",
        "Read a spreadsheet (.xlsx/.xlsm/.xlsb/.xls): sheet list, a sheet or an A1 range, with typed cells (number/string/bool/datetime/error) and optional formulas. Example: {\"path\":\"book.xlsx\",\"sheet\":\"Sales\",\"range\":\"A1:D20\"}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Spreadsheet file path."},
            "sheet":{"type":"string","description":"Sheet name; default first sheet."},
            "range":{"type":"string","description":"A1 range, e.g. B2:D20 or C3. Default: used range."},
            "list_only":{"type":"boolean","description":"Only list sheets and their sizes."},
            "header":{"type":"boolean","description":"Show first row as header in the preview (default true)."},
            "formulas":{"type":"boolean","description":"Also return formulas of the selected cells."}
        },"required":["path"]}),
        Effect::Read,
    )
}

/// Column letters -> zero-based index (`A` = 0).
fn col_index(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        n = n * 26 + (u32::from(c.to_ascii_uppercase() as u8 - b'A') + 1);
    }
    Some(n - 1)
}

fn col_letters(mut idx: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + u8::try_from(idx % 26).unwrap_or(0));
        if idx < 26 {
            break;
        }
        idx = idx / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// `B12` -> (row 11, col 1), zero-based.
fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    let row: u32 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col_index(letters)?))
}

/// `A1:C3` or `B2` -> ((r0,c0),(r1,c1)), normalized so start <= end.
type CellRange = ((u32, u32), (u32, u32));

pub(crate) fn parse_range(s: &str) -> Result<CellRange, ToolError> {
    let bad = || invalid(format!("bad range '{s}'; use A1:D20 or C3"));
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let a = parse_cell(a).ok_or_else(bad)?;
    let b = parse_cell(b).ok_or_else(bad)?;
    Ok(((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))))
}

fn iso_datetime(dt: &calamine::ExcelDateTime) -> String {
    if dt.is_duration() {
        return format!("{} days", dt.as_f64());
    }
    let (y, mo, d, h, mi, s, ms) = dt.to_ymd_hms_milli();
    if h == 0 && mi == 0 && s == 0 && ms == 0 {
        format!("{y:04}-{mo:02}-{d:02}")
    } else {
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
    }
}

/// Convert a cell to (JSON value, type name, display text).
fn cell_value(d: &Data) -> (Value, &'static str, String) {
    match d {
        Data::Empty => (Value::Null, "empty", String::new()),
        Data::Int(i) => (json!(i), "int", i.to_string()),
        Data::Float(f) => match serde_json::Number::from_f64(*f) {
            Some(n) => (Value::Number(n), "float", f.to_string()),
            None => (Value::Null, "float", f.to_string()),
        },
        Data::String(s) => (json!(s), "string", s.clone()),
        Data::Bool(b) => (json!(b), "bool", b.to_string()),
        Data::DateTime(dt) => {
            let s = iso_datetime(dt);
            let ty = if dt.is_duration() {
                "duration"
            } else {
                "datetime"
            };
            (json!(s), ty, s)
        }
        Data::DateTimeIso(s) => (json!(s), "datetime", s.clone()),
        Data::DurationIso(s) => (json!(s), "duration", s.clone()),
        Data::Error(e) => (json!(e.to_string()), "error", e.to_string()),
    }
}

/// Maximum number of non-empty cells streamed from one sheet.
const MAX_SCAN_CELLS: usize = 5_000_000;

type Book = calamine::Sheets<Cursor<Vec<u8>>>;

/// Stream the non-empty cells of a sheet (`pos`, value, formula) without ever
/// building calamine's dense `Range` (whose allocation is `rows * cols` of the
/// bounding box and can abort the process on a hostile sparse sheet).
/// `.xls` is bounded by the BIFF grid (65536 x 256) and `.ods` is refused.
fn scan_sheet<F>(wb: &mut Book, name: &str, formulas: bool, mut f: F) -> Result<(), ToolError>
where
    F: FnMut((u32, u32), Data, Option<String>),
{
    let too_big = || {
        exec(format!(
            "sheet '{name}' has more than {MAX_SCAN_CELLS} cells; refusing to load it"
        ))
    };
    let bad = |e: &dyn std::fmt::Display| exec(format!("cannot read sheet '{name}': {e}"));
    let mut seen = 0usize;
    match wb {
        calamine::Sheets::Xlsx(x) => {
            let mut r = x.worksheet_cells_reader(name).map_err(|e| bad(&e))?;
            loop {
                let (pos, v, fm) = if formulas {
                    match r.next_cell_with_formula() {
                        Ok(Some(c)) => (c.pos, Data::from(c.value), c.formula),
                        Ok(None) => break,
                        Err(e) => return Err(bad(&e)),
                    }
                } else {
                    match r.next_cell() {
                        Ok(Some(c)) => (c.get_position(), Data::from(c.get_value().clone()), None),
                        Ok(None) => break,
                        Err(e) => return Err(bad(&e)),
                    }
                };
                seen += 1;
                if seen > MAX_SCAN_CELLS {
                    return Err(too_big());
                }
                f(pos, v, fm);
            }
        }
        calamine::Sheets::Xlsb(x) => {
            let mut r = x.worksheet_cells_reader(name).map_err(|e| bad(&e))?;
            loop {
                match r.next_cell() {
                    Ok(Some(c)) => {
                        seen += 1;
                        if seen > MAX_SCAN_CELLS {
                            return Err(too_big());
                        }
                        f(c.get_position(), Data::from(c.get_value().clone()), None);
                    }
                    Ok(None) => break,
                    Err(e) => return Err(bad(&e)),
                }
            }
        }
        calamine::Sheets::Xls(x) => {
            let range = x.worksheet_range(name).map_err(|e| bad(&e))?;
            let (sr, sc) = range.start().unwrap_or((0, 0));
            for (r, c, v) in range.cells() {
                f((sr + to_u32(r), sc + to_u32(c)), v.clone(), None);
            }
            if formulas && let Ok(fr) = x.worksheet_formula(name) {
                let (sr, sc) = fr.start().unwrap_or((0, 0));
                for (r, c, v) in fr.cells() {
                    if !v.is_empty() {
                        f(
                            (sr + to_u32(r), sc + to_u32(c)),
                            Data::Empty,
                            Some(v.clone()),
                        );
                    }
                }
            }
        }
        calamine::Sheets::Ods(_) => {
            return Err(invalid(
                ".ods spreadsheets are not supported (unbounded sheet sizes); convert to .xlsx",
            ));
        }
    }
    Ok(())
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Used range (bounding box of non-empty cells) of a sheet.
fn used_range(wb: &mut Book, name: &str) -> Result<Option<CellRange>, ToolError> {
    let mut bb: Option<CellRange> = None;
    scan_sheet(wb, name, false, |(r, c), v, _| {
        if matches!(v, Data::Empty) {
            return;
        }
        bb = Some(match bb {
            None => ((r, c), (r, c)),
            Some(((r0, c0), (r1, c1))) => ((r0.min(r), c0.min(c)), (r1.max(r), c1.max(c))),
        });
    })?;
    Ok(bb)
}

fn size_of(bb: Option<CellRange>) -> (usize, usize) {
    bb.map_or((0, 0), |((r0, c0), (r1, c1))| {
        ((r1 - r0 + 1) as usize, (c1 - c0 + 1) as usize)
    })
}

pub fn run_read(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: ReadIn = parse(input)?;
    let bytes = read_limited(cfg, &i.path)?;
    let mut wb: Book = open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| exec(format!("cannot open spreadsheet {}: {e}", i.path)))?;
    let names = wb.sheet_names();
    if names.is_empty() {
        return Err(exec("workbook has no sheets"));
    }
    if i.list_only {
        let mut sheets_info = Vec::new();
        for n in &names {
            match used_range(&mut wb, n) {
                Ok(bb) => {
                    let (rows, cols) = size_of(bb);
                    sheets_info.push(json!({"name": n, "rows": rows, "cols": cols}));
                }
                Err(e) => sheets_info.push(json!({"name": n, "error": e.to_string()})),
            }
        }
        let mut md = format!("# {}\n\n", i.path);
        for s in &sheets_info {
            let name = s["name"].as_str().unwrap_or("");
            let _ = match s.get("error") {
                Some(e) => writeln!(md, "- {name} (unreadable: {e})"),
                None => writeln!(md, "- {name} ({} rows x {} cols)", s["rows"], s["cols"]),
            };
        }
        return Ok(Reply::new(
            cfg,
            &md,
            json!({"path": i.path, "sheets": sheets_info}),
        ));
    }
    let sheet = match &i.sheet {
        Some(s) if names.contains(s) => s.clone(),
        Some(s) => {
            return Err(invalid(format!(
                "no sheet '{s}'; sheets: {}",
                names.join(", ")
            )));
        }
        None => names[0].clone(),
    };
    let used = used_range(&mut wb, &sheet)?;
    let ((r0, c0), (r1, c1)) = match &i.range {
        Some(r) => parse_range(r)?,
        None => used.unwrap_or(((0, 0), (0, 0))),
    };
    let total_rows = (r1 - r0 + 1) as usize;
    let width = (c1 - c0 + 1) as usize;
    if width > 1000 {
        return Err(invalid("range is wider than 1000 columns"));
    }
    let shown_rows = total_rows.min(cfg.max_rows);
    let last_row = r0.saturating_add(to_u32(shown_rows)).saturating_sub(1);
    // Second pass keeps only the cells inside the visible window.
    let mut window: HashMap<(u32, u32), Data> = HashMap::new();
    let mut fmap = Map::new();
    scan_sheet(&mut wb, &sheet, i.formulas, |(r, c), v, fm| {
        if r < r0 || r > last_row || c < c0 || c > c1 {
            return;
        }
        if let Some(fm) = fm
            && !fm.is_empty()
            && fmap.len() < cfg.max_rows
        {
            let text = if fm.starts_with('=') {
                fm
            } else {
                format!("={fm}")
            };
            fmap.insert(format!("{}{}", col_letters(c), r + 1), json!(text));
        }
        if !matches!(v, Data::Empty) {
            window.insert((r, c), v);
        }
    })?;
    let (ur, uc) = size_of(used);
    let sheets_info: Vec<Value> = names
        .iter()
        .map(|n| {
            if *n == sheet {
                json!({"name": n, "rows": ur, "cols": uc})
            } else {
                json!({"name": n})
            }
        })
        .collect();
    let (mut cells, mut types, mut text_rows) = (Vec::new(), Vec::new(), Vec::new());
    for r in r0..r0 + to_u32(shown_rows) {
        let (mut crow, mut trow, mut xrow) = (Vec::new(), Vec::new(), Vec::new());
        for c in c0..=c1 {
            let d = window.get(&(r, c)).unwrap_or(&Data::Empty);
            let (v, t, x) = cell_value(d);
            let v = match v {
                Value::String(s) => Value::String(clip(&s, cfg.max_cell_chars)),
                o => o,
            };
            crow.push(v);
            trow.push(t);
            xrow.push(x);
        }
        cells.push(crow);
        types.push(trow);
        text_rows.push(xrow);
    }
    let mut data = json!({
        "path": i.path, "sheet": sheet, "sheets": sheets_info,
        "range": format!("{}{}:{}{}", col_letters(c0), r0 + 1, col_letters(c1), r1 + 1),
        "rows": total_rows, "cols": width, "truncated": total_rows > shown_rows,
        "cells": cells, "types": types,
    });
    if i.formulas {
        data["formulas"] = Value::Object(fmap);
    }
    let mut md = format!(
        "# {} / {} {}\n\n",
        i.path,
        data["sheet"].as_str().unwrap_or(""),
        data["range"].as_str().unwrap_or("")
    );
    md.push_str(&md_table(&text_rows, i.header, 40));
    if total_rows > shown_rows {
        let _ = write!(
            md,
            "\n_…{} more rows (limit {})_\n",
            total_rows - shown_rows,
            cfg.max_rows
        );
    }
    Ok(Reply::new(cfg, &md, data))
}

// ---------------------------------------------------------------- writing

#[derive(Deserialize)]
struct SheetIn {
    name: String,
    /// Rows of cells. A cell is null, bool, number, string, `{"formula":"=A1+B1"}`,
    /// `{"date":"2024-01-31"}` or `{"datetime":"2024-01-31T12:30:00"}`.
    rows: Vec<Vec<Value>>,
    /// Bold the first row and freeze it (default true).
    #[serde(default = "yes")]
    header: bool,
}

#[derive(Deserialize)]
struct WriteIn {
    path: String,
    sheets: Vec<SheetIn>,
    #[serde(default)]
    overwrite: bool,
}

pub fn write_spec() -> ToolSpec {
    crate::mk_spec(
        "xlsx_write",
        "Create an .xlsx with one or more sheets of typed cells. Cell: null, bool, number, string (always literal text), {\"formula\":\"=SUM(A1:A3)\"}, {\"date\":\"2024-01-31\"}, {\"datetime\":\"2024-01-31T12:30:00\"}. Fails if the file exists unless overwrite. Example: {\"path\":\"o.xlsx\",\"sheets\":[{\"name\":\"Data\",\"rows\":[[\"item\",\"qty\"],[\"a\",3],[\"sum\",{\"formula\":\"=SUM(B2:B2)\"}]]}]}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Output .xlsx path."},
            "sheets":{"type":"array","items":{"type":"object","properties":{
                "name":{"type":"string","description":"Sheet name (max 31 chars, no []:*?/\\)."},
                "rows":{"type":"array","items":{"type":"array"},"description":"Rows of cells."},
                "header":{"type":"boolean","description":"Bold + freeze first row (default true)."}
            },"required":["name","rows"]}},
            "overwrite":{"type":"boolean","description":"Replace an existing file."}
        },"required":["path","sheets"]}),
        Effect::Write,
    )
}

fn xerr(e: &rust_xlsxwriter::XlsxError) -> ToolError {
    invalid(format!("xlsx error: {e}"))
}

fn write_cell(
    ws: &mut rust_xlsxwriter::Worksheet,
    r: u32,
    c: u16,
    v: &Value,
    date_fmt: &Format,
    datetime_fmt: &Format,
    bold: Option<&Format>,
) -> Result<(), ToolError> {
    let res = match v {
        Value::Null => return Ok(()),
        Value::Bool(b) => ws.write_boolean(r, c, *b),
        Value::Number(n) => ws.write_number(r, c, n.as_f64().unwrap_or(0.0)),
        Value::String(s) => match bold {
            Some(f) => ws.write_string_with_format(r, c, s, f),
            None => ws.write_string(r, c, s),
        },
        Value::Object(o) => {
            if let Some(f) = o.get("formula").and_then(Value::as_str) {
                ws.write_formula(r, c, f)
            } else if let Some(d) = o.get("date").and_then(Value::as_str) {
                let dt = ExcelDateTime::parse_from_str(d).map_err(|e| xerr(&e))?;
                ws.write_datetime_with_format(r, c, &dt, date_fmt)
            } else if let Some(d) = o.get("datetime").and_then(Value::as_str) {
                let dt = ExcelDateTime::parse_from_str(d).map_err(|e| xerr(&e))?;
                ws.write_datetime_with_format(r, c, &dt, datetime_fmt)
            } else {
                return Err(invalid(
                    "object cell needs formula, date or datetime".to_string(),
                ));
            }
        }
        Value::Array(_) => return Err(invalid("a cell cannot be an array")),
    };
    res.map(|_| ()).map_err(|e| xerr(&e))
}

pub fn run_write(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: WriteIn = parse(input)?;
    if i.sheets.is_empty() {
        return Err(invalid("sheets must not be empty"));
    }
    let total_cells: usize = i
        .sheets
        .iter()
        .map(|s| s.rows.iter().map(Vec::len).sum::<usize>())
        .sum();
    if total_cells > MAX_WRITE_CELLS {
        return Err(invalid(format!(
            "{total_cells} cells exceeds the {MAX_WRITE_CELLS} cell limit"
        )));
    }
    let date_fmt = Format::new().set_num_format("yyyy-mm-dd");
    let datetime_fmt = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let bold = Format::new().set_bold();
    let mut wb = Workbook::new();
    let mut summary = Vec::new();
    for s in &i.sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&s.name).map_err(|e| xerr(&e))?;
        for (ri, row) in s.rows.iter().enumerate() {
            let r = u32::try_from(ri).map_err(|_| invalid("too many rows"))?;
            for (ci, v) in row.iter().enumerate() {
                let c = u16::try_from(ci).map_err(|_| invalid("too many columns"))?;
                let b = (s.header && ri == 0).then_some(&bold);
                write_cell(ws, r, c, v, &date_fmt, &datetime_fmt, b)?;
            }
        }
        if s.header && !s.rows.is_empty() {
            ws.set_freeze_panes(1, 0).map_err(|e| xerr(&e))?;
        }
        ws.autofit();
        let cols = s.rows.iter().map(Vec::len).max().unwrap_or(0);
        summary.push(json!({"name": s.name, "rows": s.rows.len(), "cols": cols}));
    }
    let bytes = wb.save_to_buffer().map_err(|e| xerr(&e))?;
    write_file(cfg, &i.path, &bytes, i.overwrite)?;
    let mut md = format!("Wrote {} ({} bytes):\n", i.path, bytes.len());
    for s in &summary {
        let _ = writeln!(
            md,
            "- {} ({} rows x {} cols)",
            s["name"].as_str().unwrap_or(""),
            s["rows"],
            s["cols"]
        );
    }
    Ok(Reply::new(
        cfg,
        &md,
        json!({"path": i.path, "bytes": bytes.len(), "sheets": summary}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name).to_str().unwrap().to_string();
        (dir, p)
    }

    #[test]
    fn a1_parsing() {
        assert_eq!(parse_cell("B12"), Some((11, 1)));
        assert_eq!(parse_cell("AA1"), Some((0, 26)));
        assert_eq!(parse_range("D4:B2").unwrap(), ((1, 1), (3, 3)));
        assert_eq!(parse_range("C3").unwrap(), ((2, 2), (2, 2)));
        assert!(parse_range("A0").is_err());
        assert!(parse_range("zz").is_err());
        assert_eq!(col_letters(27), "AB");
        assert_eq!(col_letters(0), "A");
    }

    #[test]
    fn write_then_read_typed_cells() {
        let (_d, p) = tmp_path("o.xlsx");
        let cfg = DocToolsConfig::default();
        run_write(
            &cfg,
            json!({"path": p, "sheets": [
                {"name":"Data","rows":[
                    ["item","qty","ok","when"],
                    ["apple",3,true,{"date":"2024-01-31"}],
                    ["=not a formula",2.5,false,{"datetime":"2024-01-31T12:30:00"}],
                    ["total",{"formula":"=SUM(B2:B3)"},null,null]
                ]},
                {"name":"Second","rows":[["x"]],"header":false}
            ]}),
        )
        .unwrap();

        let list = run_read(&cfg, json!({"path": p, "list_only": true})).unwrap();
        assert_eq!(list.data["sheets"].as_array().unwrap().len(), 2);

        let r = run_read(&cfg, json!({"path": p, "sheet": "Data", "formulas": true})).unwrap();
        assert_eq!(r.data["range"], "A1:D4");
        assert_eq!(r.data["cells"][1][0], "apple");
        assert_eq!(r.data["cells"][1][1], 3.0);
        assert_eq!(r.data["cells"][1][2], true);
        assert_eq!(r.data["types"][1][3], "datetime");
        assert_eq!(r.data["cells"][1][3], "2024-01-31");
        assert_eq!(r.data["cells"][2][3], "2024-01-31T12:30:00");
        assert_eq!(r.data["cells"][2][0], "=not a formula");
        assert_eq!(r.data["formulas"]["B4"], "=SUM(B2:B3)");
        assert!(r.markdown.contains("| item | qty | ok | when |"));

        let sub = run_read(&cfg, json!({"path": p, "range": "B2:C3"})).unwrap();
        assert_eq!(sub.data["rows"], 2);
        assert_eq!(sub.data["cols"], 2);
        assert_eq!(sub.data["cells"][1][0], 2.5);
    }

    #[test]
    fn row_limit_truncates() {
        let (_d, p) = tmp_path("big.xlsx");
        let cfg = DocToolsConfig {
            max_rows: 3,
            ..DocToolsConfig::default()
        };
        let rows: Vec<Value> = (0..10).map(|n| json!([n])).collect();
        run_write(
            &cfg,
            json!({"path": p, "sheets": [{"name":"S","rows": rows, "header": false}]}),
        )
        .unwrap();
        let r = run_read(&cfg, json!({"path": p})).unwrap();
        assert_eq!(r.data["truncated"], true);
        assert_eq!(r.data["cells"].as_array().unwrap().len(), 3);
        assert_eq!(r.data["rows"], 10);
    }

    #[test]
    fn errors() {
        let (_d, p) = tmp_path("o.xlsx");
        let cfg = DocToolsConfig::default();
        assert!(
            run_write(
                &cfg,
                json!({"path": p, "sheets": [{"name":"bad/name","rows":[["a"]]}]})
            )
            .is_err()
        );
        assert!(
            run_write(
                &cfg,
                json!({"path": p, "sheets": [{"name":"S","rows":[[{"nope":1}]]}]})
            )
            .is_err()
        );
        assert!(
            run_write(
                &cfg,
                json!({"path": p, "sheets": [{"name":"S","rows":[[[1]]]}]})
            )
            .is_err()
        );
        run_write(
            &cfg,
            json!({"path": p, "sheets": [{"name":"S","rows":[["a"]]}]}),
        )
        .unwrap();
        assert!(
            run_write(
                &cfg,
                json!({"path": p, "sheets": [{"name":"S","rows":[["a"]]}]})
            )
            .is_err()
        );
        assert!(run_read(&cfg, json!({"path": p, "sheet": "Nope"})).is_err());
        let junk = p.replace("o.xlsx", "junk.xlsx");
        std::fs::write(&junk, b"junk").unwrap();
        assert!(run_read(&cfg, json!({"path": junk})).is_err());
    }

    #[test]
    fn sparse_far_cell_does_not_allocate_bounding_box() {
        let (_d, p) = tmp_path("far.xlsx");
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.write_string(0, 0, "a").unwrap();
        ws.write_string(1_048_575, 16_383, "z").unwrap();
        wb.save(&p).unwrap();
        let cfg = DocToolsConfig::default();
        let l = run_read(&cfg, json!({"path": p, "list_only": true})).unwrap();
        assert_eq!(l.data["sheets"][0]["rows"], 1_048_576);
        // Default range is wider than 1000 columns: rejected, not allocated.
        assert!(run_read(&cfg, json!({"path": p})).is_err());
        let r = run_read(&cfg, json!({"path": p, "range": "A1:B2"})).unwrap();
        assert_eq!(r.data["cells"][0][0], "a");
        assert_eq!(r.data["sheets"][0]["cols"], 16_384);
    }
}
