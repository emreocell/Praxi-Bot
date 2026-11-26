//! `xlsx_append_rows` / `xlsx_update_cells`: edit an existing `.xlsx` in place.
//!
//! `xlsx_write` builds a new workbook, so using it on an existing file loses every other
//! sheet, style, column width and image. These tools instead rewrite only the target
//! worksheet part (and, when a date is written, append two cell formats to the style
//! table) and copy every other part of the package byte for byte. Strings are stored
//! inline (`t="inlineStr"`), so the shared string table is never touched.
//!
//! Limits, stated plainly: new rows are added after the last row element of the sheet
//! (formatted empty rows count); Excel tables (list objects) and conditional formats are
//! not extended; cached results of formulas are not recalculated, Excel does that when
//! the file is opened; a cell that belongs to a shared or array formula cannot be
//! overwritten.

use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::escape::escape;
use quick_xml::events::{BytesStart, Event};
use rust_xlsxwriter::ExcelDateTime;
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{Effect, ToolError, ToolSpec};

use crate::DocToolsConfig;
use crate::common::{Reply, exec, invalid, parse, read_limited, replace_file};
use crate::ooxml::{self, attr, xml_text};
use crate::xlsx::{col_letters, parse_cell, parse_range};

const MAX_EDIT_CELLS: usize = 1_000_000;
const MAX_ROW: u32 = 1_048_576;
const MAX_COL: u32 = 16_383;
const DATE_FORMAT_ID: u32 = 14;
const DATETIME_FORMAT_ID: u32 = 22;

#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Formula(String),
    Date(f64),
    DateTime(f64),
}

impl Cell {
    fn from_json(v: &Value) -> Result<Self, ToolError> {
        Ok(match v {
            Value::Null => Self::Empty,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => Self::Number(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => Self::Text(s.clone()),
            Value::Array(_) => return Err(invalid("a cell cannot be an array")),
            Value::Object(o) => {
                if let Some(f) = o.get("formula").and_then(Value::as_str) {
                    Self::Formula(f.trim_start_matches('=').to_owned())
                } else if let Some(d) = o.get("date").and_then(Value::as_str) {
                    Self::Date(serial(d)?)
                } else if let Some(d) = o.get("datetime").and_then(Value::as_str) {
                    Self::DateTime(serial(d)?)
                } else {
                    return Err(invalid("object cell needs formula, date or datetime"));
                }
            }
        })
    }

    fn is_date(&self) -> bool {
        matches!(self, Self::Date(_) | Self::DateTime(_))
    }
}

fn serial(text: &str) -> Result<f64, ToolError> {
    ExcelDateTime::parse_from_str(text)
        .map(|d| d.to_excel())
        .map_err(|e| invalid(format!("bad date '{text}': {e}")))
}

/// Cell formats appended to the style table for date cells.
#[derive(Clone, Copy)]
struct DateStyles {
    date: u32,
    datetime: u32,
}

type Edits = BTreeMap<u32, BTreeMap<u32, Cell>>;

fn a1(row0: u32, col0: u32) -> String {
    format!("{}{}", col_letters(col0), row0 + 1)
}

fn cell_xml(
    row0: u32,
    col0: u32,
    cell: &Cell,
    old_style: Option<&str>,
    dates: Option<DateStyles>,
) -> String {
    let r = a1(row0, col0);
    let style = |s: Option<String>| {
        s.or_else(|| {
            old_style
                .filter(|s| s.parse::<u32>().is_ok())
                .map(str::to_owned)
        })
        .map_or_else(String::new, |s| format!(" s=\"{s}\""))
    };
    match cell {
        Cell::Empty => format!("<c r=\"{r}\"{}/>", style(None)),
        Cell::Number(n) => format!("<c r=\"{r}\"{}><v>{n}</v></c>", style(None)),
        Cell::Bool(b) => format!(
            "<c r=\"{r}\"{} t=\"b\"><v>{}</v></c>",
            style(None),
            u8::from(*b)
        ),
        Cell::Text(t) => format!(
            "<c r=\"{r}\"{} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
            style(None),
            escape(xml_text(t).as_str())
        ),
        Cell::Formula(f) => format!(
            "<c r=\"{r}\"{}><f>{}</f></c>",
            style(None),
            escape(xml_text(f).as_str())
        ),
        Cell::Date(d) | Cell::DateTime(d) => {
            let id = dates.map(|s| {
                if matches!(cell, Cell::Date(_)) {
                    s.date
                } else {
                    s.datetime
                }
            });
            format!(
                "<c r=\"{r}\"{}><v>{d}</v></c>",
                style(id.map(|i| i.to_string()))
            )
        }
    }
}

fn xml_err(e: &impl std::fmt::Display) -> ToolError {
    exec(format!("invalid xml: {e}"))
}

fn raw(w: &mut Writer<Vec<u8>>, s: &str) {
    w.get_mut().extend_from_slice(s.as_bytes());
}

fn put(w: &mut Writer<Vec<u8>>, ev: Event<'_>) -> Result<(), ToolError> {
    w.write_event(ev)
        .map_err(|e| exec(format!("xml write: {e}")))
}

fn tag_name(e: &BytesStart<'_>) -> String {
    e.name().as_ref().to_owned()
}

/// `e` without its `spans` hint (it would be wrong once cells are added).
fn without_spans(e: &BytesStart<'_>) -> BytesStart<'static> {
    let mut out = BytesStart::new(tag_name(e));
    out.extend_attributes(
        e.attributes()
            .flatten()
            .filter(|a| a.key.as_ref() != "spans"),
    );
    out
}

struct SheetEditor<'a> {
    reader: Reader<&'a [u8]>,
    w: Writer<Vec<u8>>,
    pending: Edits,
    dates: Option<DateStyles>,
    dimension: Option<String>,
    overwrote_formula: bool,
    /// Index of the row element read last (1-based); rows without `r` number from it.
    last_row: u32,
    last_col: Option<u32>,
    row0: u32,
    /// Cells still to be placed in the row being copied.
    cells: Option<BTreeMap<u32, Cell>>,
}

impl SheetEditor<'_> {
    fn flush_rows(&mut self, before: Option<u32>) {
        while let Some((&row0, _)) = self.pending.first_key_value() {
            if before.is_some_and(|b| row0 >= b) {
                break;
            }
            let cells = self.pending.remove(&row0).unwrap_or_default();
            raw(&mut self.w, &format!("<row r=\"{}\">", row0 + 1));
            for (col0, cell) in &cells {
                raw(&mut self.w, &cell_xml(row0, *col0, cell, None, self.dates));
            }
            raw(&mut self.w, "</row>");
        }
    }

    fn row_index(&mut self, e: &BytesStart<'_>) -> Result<u32, ToolError> {
        let r = match attr(e, "r") {
            Some(r) => r.trim().parse::<u32>().ok().filter(|r| *r >= 1),
            None => Some(self.last_row + 1),
        }
        .ok_or_else(|| exec("a row of the sheet has an invalid index"))?;
        self.last_row = r;
        Ok(r - 1)
    }

    fn flush_row_cells(&mut self) {
        let row0 = self.row0;
        for (col0, cell) in std::mem::take(&mut self.cells).unwrap_or_default() {
            raw(&mut self.w, &cell_xml(row0, col0, &cell, None, self.dates));
        }
    }

    fn row(&mut self, e: &BytesStart<'_>, empty: bool) -> Result<(), ToolError> {
        let row0 = self.row_index(e)?;
        self.flush_rows(Some(row0));
        let Some(cells) = self.pending.remove(&row0) else {
            let ev = if empty {
                Event::Empty(e.borrow())
            } else {
                Event::Start(e.borrow())
            };
            return put(&mut self.w, ev);
        };
        put(&mut self.w, Event::Start(without_spans(e)))?;
        self.row0 = row0;
        self.last_col = None;
        self.cells = Some(cells);
        if empty {
            self.flush_row_cells();
            raw(&mut self.w, &format!("</{}>", tag_name(e)));
        }
        Ok(())
    }

    fn cell(&mut self, e: &BytesStart<'_>, empty: bool) -> Result<(), ToolError> {
        let col0 = match attr(e, "r") {
            Some(r) => parse_cell(&r)
                .map(|(_, c)| c)
                .ok_or_else(|| exec("a cell of the sheet has an invalid reference"))?,
            None => self.last_col.map_or(0, |c| c + 1),
        };
        self.last_col = Some(col0);
        let mut cells = self.cells.take().unwrap_or_default();
        let before: Vec<u32> = cells.range(..col0).map(|(c, _)| *c).collect();
        for c in before {
            if let Some(cell) = cells.remove(&c) {
                raw(
                    &mut self.w,
                    &cell_xml(self.row0, c, &cell, None, self.dates),
                );
            }
        }
        if let Some(cell) = cells.remove(&col0) {
            if !empty {
                self.skip_cell()?;
            }
            let style = attr(e, "s");
            raw(
                &mut self.w,
                &cell_xml(self.row0, col0, &cell, style.as_deref(), self.dates),
            );
        } else {
            let ev = if empty {
                Event::Empty(e.borrow())
            } else {
                Event::Start(e.borrow())
            };
            put(&mut self.w, ev)?;
        }
        self.cells = Some(cells);
        Ok(())
    }

    /// Consume the content of an overwritten cell up to its end tag.
    fn skip_cell(&mut self) -> Result<(), ToolError> {
        let mut depth = 0usize;
        loop {
            match self.reader.read_event().map_err(|e| xml_err(&e))? {
                Event::Eof => return Err(exec("unexpected end of the sheet")),
                Event::Start(e) => {
                    self.note_formula(&e)?;
                    depth += 1;
                }
                Event::Empty(e) => self.note_formula(&e)?,
                Event::End(_) => {
                    if depth == 0 {
                        return Ok(());
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
    }

    fn note_formula(&mut self, e: &BytesStart<'_>) -> Result<(), ToolError> {
        if e.local_name().as_ref() != "f" {
            return Ok(());
        }
        if attr(e, "ref").is_some() {
            return Err(invalid(
                "a cell to overwrite belongs to a shared or array formula; change it in a spreadsheet program",
            ));
        }
        self.overwrote_formula = true;
        Ok(())
    }

    fn run(mut self) -> Result<(Vec<u8>, bool), ToolError> {
        let mut in_data = false;
        loop {
            let ev = self.reader.read_event().map_err(|e| xml_err(&e))?;
            match ev {
                Event::Eof => break,
                Event::Empty(e)
                    if e.local_name().as_ref() == "dimension" && self.dimension.is_some() =>
                {
                    let dim = self.dimension.take().unwrap_or_default();
                    raw(&mut self.w, &format!("<{} ref=\"{dim}\"/>", tag_name(&e)));
                }
                Event::Empty(e) if e.local_name().as_ref() == "sheetData" => {
                    let name = tag_name(&e);
                    raw(&mut self.w, &format!("<{name}>"));
                    self.flush_rows(None);
                    raw(&mut self.w, &format!("</{name}>"));
                }
                Event::Start(e) if e.local_name().as_ref() == "sheetData" => {
                    in_data = true;
                    put(&mut self.w, Event::Start(e))?;
                }
                Event::End(e) if e.local_name().as_ref() == "sheetData" => {
                    self.flush_rows(None);
                    in_data = false;
                    put(&mut self.w, Event::End(e))?;
                }
                Event::Start(e)
                    if in_data && self.cells.is_none() && e.local_name().as_ref() == "row" =>
                {
                    self.row(&e, false)?;
                }
                Event::Empty(e)
                    if in_data && self.cells.is_none() && e.local_name().as_ref() == "row" =>
                {
                    self.row(&e, true)?;
                }
                Event::End(e) if self.cells.is_some() && e.local_name().as_ref() == "row" => {
                    self.flush_row_cells();
                    put(&mut self.w, Event::End(e))?;
                }
                Event::Start(e) if self.cells.is_some() && e.local_name().as_ref() == "c" => {
                    self.cell(&e, false)?;
                }
                Event::Empty(e) if self.cells.is_some() && e.local_name().as_ref() == "c" => {
                    self.cell(&e, true)?;
                }
                other => put(&mut self.w, other)?,
            }
        }
        if !self.pending.is_empty() {
            return Err(exec("the sheet has no data section to add cells to"));
        }
        Ok((self.w.into_inner(), self.overwrote_formula))
    }
}

/// `xml` with `edits` applied; the flag tells whether an existing formula was overwritten.
fn edit_sheet(
    xml: &[u8],
    edits: Edits,
    dates: Option<DateStyles>,
    dimension: Option<String>,
) -> Result<(Vec<u8>, bool), ToolError> {
    SheetEditor {
        reader: Reader::from_reader(xml),
        w: Writer::new(Vec::new()),
        pending: edits,
        dates,
        dimension,
        overwrote_formula: false,
        last_row: 0,
        last_col: None,
        row0: 0,
        cells: None,
    }
    .run()
}

/// Number of the last row element of the sheet (0 when there is none) and its dimension ref.
fn sheet_extent(xml: &[u8]) -> Result<(u32, Option<String>), ToolError> {
    let mut reader = Reader::from_reader(xml);
    let (mut last, mut dim) = (0u32, None);
    loop {
        match reader.read_event().map_err(|e| xml_err(&e))? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "row" => {
                    last = match attr(&e, "r") {
                        Some(r) => r.trim().parse().unwrap_or(last + 1),
                        None => last + 1,
                    };
                }
                "dimension" => dim = attr(&e, "ref"),
                _ => {}
            },
            _ => {}
        }
    }
    Ok((last, dim))
}

/// The dimension ref covering the old one and every edited cell.
fn merged_dimension(old: Option<&str>, edits: &Edits) -> String {
    let mut bounds: Option<((u32, u32), (u32, u32))> = old.and_then(|d| parse_range(d).ok());
    for (row0, cols) in edits {
        for col0 in cols.keys() {
            bounds = Some(match bounds {
                None => ((*row0, *col0), (*row0, *col0)),
                Some(((r0, c0), (r1, c1))) => (
                    (r0.min(*row0), c0.min(*col0)),
                    (r1.max(*row0), c1.max(*col0)),
                ),
            });
        }
    }
    let ((r0, c0), (r1, c1)) = bounds.unwrap_or(((0, 0), (0, 0)));
    if (r0, c0) == (r1, c1) {
        a1(r0, c0)
    } else {
        format!("{}:{}", a1(r0, c0), a1(r1, c1))
    }
}

/// Append two date cell formats to the style table; returns the new XML and their indexes.
fn add_date_styles(xml: &[u8]) -> Result<(Vec<u8>, DateStyles), ToolError> {
    let unsupported =
        || invalid("this workbook's style table cannot take a date; write the date as text");
    let mut reader = Reader::from_reader(xml);
    let (mut in_xfs, mut count, mut found) = (false, 0u32, false);
    loop {
        match reader.read_event().map_err(|e| xml_err(&e))? {
            Event::Eof => break,
            Event::Start(e) => match e.local_name().as_ref() {
                "cellXfs" => (in_xfs, found) = (true, true),
                "xf" if in_xfs => count += 1,
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "cellXfs" => found = true,
                "xf" if in_xfs => count += 1,
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "cellXfs" => in_xfs = false,
            _ => {}
        }
    }
    if !found {
        return Err(unsupported());
    }
    let xf = |id: u32| {
        format!(
            "<xf numFmtId=\"{id}\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>"
        )
    };
    let counted = |e: &BytesStart<'_>| {
        let mut out = BytesStart::new(tag_name(e));
        out.extend_attributes(
            e.attributes()
                .flatten()
                .filter(|a| a.key.as_ref() != "count"),
        );
        out.push_attribute(("count", (count + 2).to_string().as_str()));
        out
    };
    let mut reader = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::new());
    loop {
        match reader.read_event().map_err(|e| xml_err(&e))? {
            Event::Eof => break,
            Event::Start(e) if e.local_name().as_ref() == "cellXfs" => {
                if tag_name(&e).contains(':') {
                    return Err(unsupported());
                }
                put(&mut w, Event::Start(counted(&e)))?;
            }
            Event::Empty(e) if e.local_name().as_ref() == "cellXfs" => {
                if tag_name(&e).contains(':') {
                    return Err(unsupported());
                }
                put(&mut w, Event::Start(counted(&e)))?;
                raw(
                    &mut w,
                    &format!(
                        "{}{}</{}>",
                        xf(DATE_FORMAT_ID),
                        xf(DATETIME_FORMAT_ID),
                        tag_name(&e)
                    ),
                );
            }
            Event::End(e) if e.local_name().as_ref() == "cellXfs" => {
                raw(
                    &mut w,
                    &format!("{}{}", xf(DATE_FORMAT_ID), xf(DATETIME_FORMAT_ID)),
                );
                put(&mut w, Event::End(e))?;
            }
            other => put(&mut w, other)?,
        }
    }
    Ok((
        w.into_inner(),
        DateStyles {
            date: count,
            datetime: count + 1,
        },
    ))
}

/// Name and package path of the worksheet to edit (the first sheet when `wanted` is `None`).
fn resolve_sheet(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    wanted: Option<&str>,
) -> Result<(String, String), ToolError> {
    let workbook = ooxml::read_part(zip, "xl/workbook.xml")?
        .ok_or_else(|| invalid("not a spreadsheet: xl/workbook.xml is missing"))?;
    let rels = ooxml::read_part(zip, "xl/_rels/workbook.xml.rels")?
        .ok_or_else(|| invalid("not a spreadsheet: workbook relationships are missing"))?;
    let mut targets = BTreeMap::new();
    let mut reader = Reader::from_reader(rels.as_slice());
    loop {
        match reader.read_event().map_err(|e| xml_err(&e))? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    targets.insert(id, target);
                }
            }
            _ => {}
        }
    }
    let mut sheets = Vec::new();
    let mut reader = Reader::from_reader(workbook.as_slice());
    loop {
        match reader.read_event().map_err(|e| xml_err(&e))? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "sheet" => {
                if let (Some(name), Some(id)) = (attr(&e, "name"), attr(&e, "id")) {
                    sheets.push((name, id));
                }
            }
            _ => {}
        }
    }
    let names: Vec<&str> = sheets.iter().map(|(n, _)| n.as_str()).collect();
    let (name, id) = match wanted {
        Some(w) => sheets
            .iter()
            .find(|(n, _)| n == w)
            .ok_or_else(|| invalid(format!("no sheet '{w}'; sheets: {}", names.join(", "))))?,
        None => sheets
            .first()
            .ok_or_else(|| exec("workbook has no sheets"))?,
    };
    let target = targets
        .get(id)
        .ok_or_else(|| exec(format!("sheet '{name}' has no worksheet part")))?;
    let part = match target.strip_prefix('/') {
        Some(abs) => abs.to_owned(),
        None => format!("xl/{target}"),
    };
    Ok((name.clone(), part))
}

/// Reads `path` as an `.xlsx`, lets `plan` choose the edits from the sheet's current end and
/// writes the edited workbook back. Returns what changed for the reply.
struct Applied {
    sheet: String,
    cells: usize,
    first_row: u32,
    last_row: u32,
}

fn edit_workbook(
    cfg: &DocToolsConfig,
    path: &str,
    sheet: Option<&str>,
    plan: impl FnOnce(u32) -> Result<Edits, ToolError>,
) -> Result<Applied, ToolError> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext != "xlsx" {
        return Err(invalid(
            "only .xlsx files can be edited in place (not .xlsm, .xlsb or .xls)",
        ));
    }
    let bytes = read_limited(cfg, path)?;
    crate::check_zip_limits(&bytes)?;
    let mut zip = ooxml::open(&bytes)?;
    if zip.by_name("xl/vbaProject.bin").is_ok() {
        return Err(invalid("workbooks with macros are never edited"));
    }
    let (sheet_name, part) = resolve_sheet(&mut zip, sheet)?;
    let xml = ooxml::read_part(&mut zip, &part)?
        .ok_or_else(|| exec(format!("worksheet part {part} is missing")))?;
    let (last_row, old_dimension) = sheet_extent(&xml)?;
    let edits = plan(last_row)?;
    let cells: usize = edits.values().map(BTreeMap::len).sum();
    if cells == 0 {
        return Err(invalid("nothing to write"));
    }
    if cells > MAX_EDIT_CELLS {
        return Err(invalid(format!(
            "{cells} cells exceeds the {MAX_EDIT_CELLS} cell limit"
        )));
    }
    let (first_row, last) = (
        edits.keys().next().copied().unwrap_or(0) + 1,
        edits.keys().next_back().copied().unwrap_or(0) + 1,
    );
    let dimension = old_dimension
        .as_ref()
        .map(|_| merged_dimension(old_dimension.as_deref(), &edits));
    let mut replace = ooxml::XmlHashMap::new();
    let mut dates = None;
    if edits.values().flat_map(BTreeMap::values).any(Cell::is_date) {
        let styles = ooxml::read_part(&mut zip, "xl/styles.xml")?
            .ok_or_else(|| invalid("this workbook has no style table; write the date as text"))?;
        let (new_styles, ids) = add_date_styles(&styles)?;
        replace.insert("xl/styles.xml".to_owned(), new_styles);
        dates = Some(ids);
    }
    let (new_xml, overwrote_formula) = edit_sheet(&xml, edits, dates, dimension)?;
    replace.insert(part, new_xml);
    let mut remove = Vec::new();
    if overwrote_formula && zip.by_name("xl/calcChain.xml").is_ok() {
        // The calculation chain lists formula cells; a stale entry makes Excel "repair" the file.
        remove.push("xl/calcChain.xml");
        for (name, skip) in [
            ("[Content_Types].xml", "calcChain.xml"),
            ("xl/_rels/workbook.xml.rels", "calcChain.xml"),
        ] {
            if let Some(part) = ooxml::read_part(&mut zip, name)? {
                let attr_name = if name.starts_with('[') {
                    "PartName"
                } else {
                    "Target"
                };
                let cleaned = ooxml::remove_empty_elements(&part, |e| {
                    attr(e, attr_name).is_some_and(|v| v.ends_with(skip))
                })?;
                replace.insert(name.to_owned(), cleaned);
            }
        }
    }
    drop(zip);
    let out = ooxml::rewrite(&bytes, &replace, &remove)?;
    replace_file(cfg, path, &out)?;
    Ok(Applied {
        sheet: sheet_name,
        cells,
        first_row,
        last_row: last,
    })
}

fn check_position(row0: u32, col0: u32) -> Result<(), ToolError> {
    if row0 >= MAX_ROW || col0 > MAX_COL {
        return Err(invalid(
            "a cell is outside the spreadsheet grid (1048576 rows, 16384 columns)",
        ));
    }
    Ok(())
}

fn reply(cfg: &DocToolsConfig, path: &str, what: &str, a: &Applied) -> Reply {
    let range = if a.first_row == a.last_row {
        format!("row {}", a.first_row)
    } else {
        format!("rows {}-{}", a.first_row, a.last_row)
    };
    let md = format!(
        "{what} {} cell(s) in {path}, sheet '{}', {range}. Other sheets and formatting were kept.\n",
        a.cells, a.sheet
    );
    Reply::new(
        cfg,
        &md,
        json!({"path": path, "sheet": a.sheet, "cells": a.cells, "first_row": a.first_row, "last_row": a.last_row}),
    )
}

// ---------------------------------------------------------------- xlsx_append_rows

#[derive(Deserialize)]
struct AppendIn {
    path: String,
    sheet: Option<String>,
    rows: Vec<Vec<Value>>,
}

pub fn append_spec() -> ToolSpec {
    crate::mk_spec(
        "xlsx_append_rows",
        "Add rows after the last row of an existing .xlsx sheet, keeping every other sheet, style and column width as they are. Cell: null, bool, number, string (always literal text), {\"formula\":\"=SUM(A1:A3)\"}, {\"date\":\"2024-01-31\"}, {\"datetime\":\"2024-01-31T12:30:00\"}. Does not extend Excel tables. Example: {\"path\":\"book.xlsx\",\"sheet\":\"Sales\",\"rows\":[[\"apple\",3],[\"pear\",5]]}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Existing .xlsx file."},
            "sheet":{"type":"string","description":"Sheet name; default first sheet."},
            "rows":{"type":"array","items":{"type":"array"},"description":"Rows of cells, starting in column A."}
        },"required":["path","rows"]}),
        Effect::Write,
    )
}

pub fn run_append(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: AppendIn = parse(input)?;
    if i.rows.iter().all(Vec::is_empty) {
        return Err(invalid("rows must hold at least one cell"));
    }
    let applied = edit_workbook(cfg, &i.path, i.sheet.as_deref(), |last_row| {
        let mut edits = Edits::new();
        for (n, row) in i.rows.iter().enumerate() {
            let row0 =
                last_row.saturating_add(u32::try_from(n).map_err(|_| invalid("too many rows"))?);
            for (c, v) in row.iter().enumerate() {
                let col0 = u32::try_from(c).map_err(|_| invalid("too many columns"))?;
                check_position(row0, col0)?;
                let cell = Cell::from_json(v)?;
                if cell != Cell::Empty {
                    edits.entry(row0).or_default().insert(col0, cell);
                }
            }
        }
        Ok(edits)
    })?;
    Ok(reply(cfg, &i.path, "Appended", &applied))
}

// ---------------------------------------------------------------- xlsx_update_cells

#[derive(Deserialize)]
struct UpdateIn {
    path: String,
    sheet: Option<String>,
    cells: BTreeMap<String, Value>,
}

pub fn update_spec() -> ToolSpec {
    crate::mk_spec(
        "xlsx_update_cells",
        "Set individual cells of an existing .xlsx sheet by A1 reference, keeping every other cell, sheet and style as they are (a cell keeps its own formatting). Cell: null clears it, bool, number, string (always literal text), {\"formula\":\"=A1+B1\"}, {\"date\":\"2024-01-31\"}, {\"datetime\":\"2024-01-31T12:30:00\"}. Example: {\"path\":\"book.xlsx\",\"sheet\":\"Sales\",\"cells\":{\"B2\":12,\"C2\":{\"formula\":\"=B2*2\"}}}",
        json!({"type":"object","properties":{
            "path":{"type":"string","description":"Existing .xlsx file."},
            "sheet":{"type":"string","description":"Sheet name; default first sheet."},
            "cells":{"type":"object","description":"A1 reference to new cell value, e.g. {\"B2\": 12}."}
        },"required":["path","cells"]}),
        Effect::Write,
    )
}

pub fn run_update(cfg: &DocToolsConfig, input: Value) -> Result<Reply, ToolError> {
    let i: UpdateIn = parse(input)?;
    let applied = edit_workbook(cfg, &i.path, i.sheet.as_deref(), |_| {
        let mut edits = Edits::new();
        for (key, v) in &i.cells {
            let (row0, col0) = parse_cell(key)
                .ok_or_else(|| invalid(format!("bad cell reference '{key}'; use A1 style")))?;
            check_position(row0, col0)?;
            edits
                .entry(row0)
                .or_default()
                .insert(col0, Cell::from_json(v)?);
        }
        Ok(edits)
    })?;
    Ok(reply(cfg, &i.path, "Updated", &applied))
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use rust_xlsxwriter::{Format, Workbook};

    use super::*;
    use crate::xlsx::run_read;

    /// Two sheets: a formatted `Data` sheet (bold header, a formula, a date) and an `Other` sheet.
    fn fixture(dir: &std::path::Path) -> String {
        let path = dir.join("book.xlsx").to_str().unwrap().to_owned();
        let mut wb = Workbook::new();
        let bold = Format::new().set_bold();
        let date = Format::new().set_num_format("yyyy-mm-dd");
        let ws = wb.add_worksheet().set_name("Data").unwrap();
        ws.write_string_with_format(0, 0, "item", &bold).unwrap();
        ws.write_string_with_format(0, 1, "qty", &bold).unwrap();
        ws.write_string(1, 0, "apple").unwrap();
        ws.write_number(1, 1, 3).unwrap();
        ws.write_string(2, 0, "total").unwrap();
        ws.write_formula(2, 1, "=SUM(B2:B2)").unwrap();
        ws.write_datetime_with_format(
            3,
            0,
            ExcelDateTime::parse_from_str("2024-01-31").unwrap(),
            &date,
        )
        .unwrap();
        ws.set_column_width(0, 30).unwrap();
        let other = wb.add_worksheet().set_name("Other").unwrap();
        other.write_string(0, 0, "keep me").unwrap();
        wb.save(&path).unwrap();
        path
    }

    fn part(path: &str, name: &str) -> String {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut out = String::new();
        zip.by_name(name).unwrap().read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn append_rows_keeps_other_sheets_and_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        let cfg = DocToolsConfig::default();
        let (styles_before, other_before) = (
            part(&p, "xl/styles.xml"),
            part(&p, "xl/worksheets/sheet2.xml"),
        );
        let r = run_append(
            &cfg,
            json!({"path": p, "sheet": "Data", "rows": [
                ["pear", 5, true],
                [null, {"formula": "=B5*2"}, {"date": "2025-02-03"}],
                ["=not a formula", 1.5]
            ]}),
        )
        .unwrap();
        assert_eq!(r.data["first_row"], 5);
        assert_eq!(r.data["last_row"], 7);

        let read = run_read(&cfg, json!({"path": p, "sheet": "Data", "formulas": true})).unwrap();
        assert_eq!(read.data["range"], "A1:C7");
        assert_eq!(read.data["cells"][0][0], "item", "old header kept");
        assert_eq!(read.data["cells"][2][0], "total");
        assert_eq!(read.data["cells"][4][0], "pear");
        assert_eq!(read.data["cells"][4][1], 5.0);
        assert_eq!(read.data["cells"][4][2], true);
        assert_eq!(read.data["formulas"]["B6"], "=B5*2");
        assert_eq!(read.data["types"][5][2], "datetime");
        assert_eq!(read.data["cells"][5][2], "2025-02-03");
        assert_eq!(
            read.data["cells"][6][0], "=not a formula",
            "strings stay literal text"
        );

        // The other sheet is byte-identical; the old date still reads as a date; the bold header
        // keeps its style index; the style table only grew.
        assert_eq!(part(&p, "xl/worksheets/sheet2.xml"), other_before);
        let other = run_read(&cfg, json!({"path": p, "sheet": "Other"})).unwrap();
        assert_eq!(other.data["cells"][0][0], "keep me");
        assert_eq!(read.data["types"][3][0], "datetime");
        let sheet1 = part(&p, "xl/worksheets/sheet1.xml");
        assert!(sheet1.contains("<c r=\"A1\" s=\"1\""), "{sheet1}");
        assert!(sheet1.contains("<dimension ref=\"A1:C7\"/>"), "{sheet1}");
        let styles = part(&p, "xl/styles.xml");
        assert!(styles.len() > styles_before.len() && styles.contains("numFmtId=\"22\""));
        assert!(part(&p, "xl/workbook.xml").contains("Other"));
    }

    #[test]
    fn update_cells_overwrites_in_place_and_adds_new_cells() {
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        let cfg = DocToolsConfig::default();
        let r = run_update(
            &cfg,
            json!({"path": p, "sheet": "Data", "cells": {
                "A1": "Item name",
                "B2": 12,
                "B3": {"formula": "=SUM(B2:B2)+1"},
                "A4": null,
                "D1": "new col",
                "C9": {"date": "2024-12-24"}
            }}),
        )
        .unwrap();
        assert_eq!(r.data["cells"], 6);
        let read = run_read(&cfg, json!({"path": p, "sheet": "Data", "formulas": true})).unwrap();
        assert_eq!(read.data["cells"][0][0], "Item name");
        assert_eq!(read.data["cells"][0][1], "qty", "untouched cell kept");
        assert_eq!(read.data["cells"][1][1], 12.0);
        assert_eq!(read.data["formulas"]["B3"], "=SUM(B2:B2)+1");
        assert_eq!(read.data["cells"][0][3], "new col");
        assert_eq!(read.data["cells"][8][2], "2024-12-24");
        assert_eq!(read.data["cells"][3][0], Value::Null, "cleared");
        // An overwritten cell keeps its own formatting.
        assert!(
            part(&p, "xl/worksheets/sheet1.xml").contains("<c r=\"A1\" s=\"1\" t=\"inlineStr\">")
        );
        let other = run_read(&cfg, json!({"path": p, "sheet": "Other"})).unwrap();
        assert_eq!(other.data["cells"][0][0], "keep me");
    }

    #[test]
    fn edits_default_to_the_first_sheet_and_reject_bad_requests() {
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        let cfg = DocToolsConfig::default();
        run_append(&cfg, json!({"path": p, "rows": [["x"]]})).unwrap();
        let read = run_read(&cfg, json!({"path": p, "sheet": "Data"})).unwrap();
        assert_eq!(read.data["cells"][4][0], "x");
        for bad in [
            json!({"path": p, "sheet": "Nope", "rows": [["x"]]}),
            json!({"path": p, "rows": []}),
            json!({"path": p, "rows": [[[1]]]}),
            json!({"path": p, "rows": [[{"nope": 1}]]}),
        ] {
            assert!(run_append(&cfg, bad.clone()).is_err(), "{bad}");
        }
        assert!(run_update(&cfg, json!({"path": p, "cells": {"zz": 1}})).is_err());
        assert!(run_update(&cfg, json!({"path": p, "cells": {}})).is_err());
        let txt = dir.path().join("a.csv");
        std::fs::write(&txt, "a,b").unwrap();
        assert!(run_append(&cfg, json!({"path": txt, "rows": [["x"]]})).is_err());
    }

    #[test]
    fn an_empty_sheet_and_the_write_guard_are_handled() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.xlsx").to_str().unwrap().to_owned();
        let mut wb = Workbook::new();
        wb.add_worksheet();
        wb.save(&p).unwrap();
        let cfg = DocToolsConfig::default();
        run_append(&cfg, json!({"path": p, "rows": [["a", 1]]})).unwrap();
        let read = run_read(&cfg, json!({"path": p})).unwrap();
        assert_eq!(read.data["cells"][0][1], 1.0);

        let guarded = DocToolsConfig {
            write_guard: Some(crate::WriteGuard::new(|_| Err("not here".into()))),
            ..DocToolsConfig::default()
        };
        let before = std::fs::read(&p).unwrap();
        let err = run_update(&guarded, json!({"path": p, "cells": {"A1": "x"}})).unwrap_err();
        assert!(err.to_string().contains("not here"), "{err}");
        assert_eq!(
            std::fs::read(&p).unwrap(),
            before,
            "a refused edit leaves the file alone"
        );
    }

    #[test]
    fn overwriting_a_shared_formula_master_is_refused() {
        let xml = br#"<worksheet xmlns="x"><sheetData><row r="1"><c r="A1"><f t="shared" ref="A1:A2" si="0">B1</f><v>1</v></c></row></sheetData></worksheet>"#;
        let mut edits = Edits::new();
        edits.entry(0).or_default().insert(0, Cell::Number(2.0));
        let err = edit_sheet(xml, edits, None, None).unwrap_err();
        assert!(err.to_string().contains("shared or array formula"), "{err}");
    }

    #[test]
    fn sheet_xml_edits_keep_unrelated_markup() {
        let xml = br#"<worksheet xmlns="x"><dimension ref="A1:B2"/><sheetData><row r="1" spans="1:2" ht="20"><c r="A1" s="3"><v>1</v></c><c r="B1"/></row><row r="2"/></sheetData><mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#;
        let mut edits = Edits::new();
        edits
            .entry(0)
            .or_default()
            .insert(1, Cell::Text("a<b".into()));
        edits.entry(1).or_default().insert(0, Cell::Bool(true));
        edits.entry(3).or_default().insert(2, Cell::Number(7.5));
        let (out, overwrote) = edit_sheet(xml, edits, None, Some("A1:C4".into())).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(!overwrote);
        assert_eq!(
            out,
            "<worksheet xmlns=\"x\"><dimension ref=\"A1:C4\"/><sheetData><row r=\"1\" ht=\"20\"><c r=\"A1\" s=\"3\"><v>1</v></c><c r=\"B1\" t=\"inlineStr\"><is><t xml:space=\"preserve\">a&lt;b</t></is></c></row><row r=\"2\"><c r=\"A2\" t=\"b\"><v>1</v></c></row><row r=\"4\"><c r=\"C4\"><v>7.5</v></c></row></sheetData><mergeCells count=\"1\"><mergeCell ref=\"A1:B1\"/></mergeCells></worksheet>"
        );
    }

    #[test]
    fn overwriting_a_formula_drops_the_stale_calculation_chain() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let p = fixture(dir.path());
        // Give the fixture a calculation chain like Excel writes.
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
        let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for n in 0..zip.len() {
            let mut f = zip.by_index(n).unwrap();
            let name = f.name().unwrap().into_owned();
            let mut body = String::new();
            f.read_to_string(&mut body).unwrap();
            let body = match name.as_str() {
                "[Content_Types].xml" => body.replace(
                    "</Types>",
                    "<Override PartName=\"/xl/calcChain.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml\"/></Types>",
                ),
                "xl/_rels/workbook.xml.rels" => body.replace(
                    "</Relationships>",
                    "<Relationship Id=\"rId99\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain\" Target=\"calcChain.xml\"/></Relationships>",
                ),
                _ => body,
            };
            out.start_file(name, opts).unwrap();
            out.write_all(body.as_bytes()).unwrap();
        }
        out.start_file("xl/calcChain.xml", opts).unwrap();
        out.write_all(b"<calcChain xmlns=\"x\"><c r=\"B3\" i=\"1\"/></calcChain>")
            .unwrap();
        std::fs::write(&p, out.finish().unwrap().into_inner()).unwrap();

        let cfg = DocToolsConfig::default();
        // Overwriting a plain value leaves the chain alone.
        run_update(
            &cfg,
            json!({"path": p, "sheet": "Data", "cells": {"A2": "pear"}}),
        )
        .unwrap();
        assert!(
            zip::ZipArchive::new(std::fs::File::open(&p).unwrap())
                .unwrap()
                .by_name("xl/calcChain.xml")
                .is_ok()
        );
        // Overwriting the formula cell B3 removes the chain and every reference to it.
        run_update(
            &cfg,
            json!({"path": p, "sheet": "Data", "cells": {"B3": 9}}),
        )
        .unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
        assert!(zip.by_name("xl/calcChain.xml").is_err());
        drop(zip);
        assert!(!part(&p, "[Content_Types].xml").contains("calcChain"));
        assert!(!part(&p, "xl/_rels/workbook.xml.rels").contains("calcChain"));
        let read = run_read(&cfg, json!({"path": p, "sheet": "Data"})).unwrap();
        assert_eq!(read.data["cells"][2][1], 9.0);
    }
}
