//! Model catalog freshness: validation, snapshot diffing, staleness checks,
//! and documentation-table synchronization.
//!
//! All functions in this module are pure and offline: they never perform
//! network calls or read secrets.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::entry::{Auth, ModelInfo, ProviderEntry};
use crate::model_metadata::{
    CuratedModels, ModelKind, ProviderMetadata, embedded_metadata, inferred_metadata, safe_model_id,
};

/// Warn when the catalog review date is older than this many days.
pub const DEFAULT_STALE_AFTER_DAYS: u32 = 90;

/// Opening marker for the generated table in `docs/guide/model-catalog.md`.
pub const DOCS_TABLE_BEGIN: &str = "<!-- BEGIN:model-catalog-table -->";
/// Closing marker for the generated table in `docs/guide/model-catalog.md`.
pub const DOCS_TABLE_END: &str = "<!-- END:model-catalog-table -->";

const MIN_CONTEXT_WINDOW: u32 = 1_024;
const MAX_CONTEXT_WINDOW: u32 = 100_000_000;
const MAX_OUTPUT_TOKENS: u32 = 10_000_000;
const MAX_PRICE_PER_MTOK: f64 = 10_000.0;
const SECS_PER_DAY: u64 = 86_400;

// ------------------------------------------------------------------ ISO date

/// A calendar date (`YYYY-MM-DD`) in the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IsoDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl IsoDate {
    /// Parse a strict `YYYY-MM-DD` ISO date.
    ///
    /// # Errors
    /// Returns a human-readable error if the date is malformed or does not
    /// exist on the calendar (for example `2026-02-30`).
    pub fn parse(input: &str) -> Result<Self, String> {
        let trimmed = input.trim();
        let bytes = trimmed.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err("expected YYYY-MM-DD".into());
        }
        let year: i32 = trimmed[0..4]
            .parse()
            .map_err(|_| "year must be 4 digits".to_string())?;
        let month: u8 = trimmed[5..7]
            .parse()
            .map_err(|_| "month must be 2 digits".to_string())?;
        let day: u8 = trimmed[8..10]
            .parse()
            .map_err(|_| "day must be 2 digits".to_string())?;
        if !(2000..=9999).contains(&year) {
            return Err("year must be in 2000..=9999".into());
        }
        if !(1..=12).contains(&month) {
            return Err("month must be in 1..=12".into());
        }
        let max_day = days_in_month(year, month);
        if day < 1 || day > max_day {
            return Err(format!(
                "day must be in 1..={max_day} for {year:04}-{month:02}"
            ));
        }
        Ok(Self { year, month, day })
    }

    /// Convert a UTC [`SystemTime`] to its calendar date.
    #[must_use]
    pub fn from_system_time(now: SystemTime) -> Option<Self> {
        let secs = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
        let days = i64::try_from(secs / SECS_PER_DAY).ok()?;
        Some(civil_from_days(days))
    }

    /// Whole days elapsed from `earlier` to `self` (negative if `self < earlier`).
    #[must_use]
    pub fn days_since(self, earlier: Self) -> i64 {
        days_from_civil(self) - days_from_civil(earlier)
    }
}

impl std::fmt::Display for IsoDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Howard Hinnant's civil-to-days algorithm (days since 1970-01-01).
fn days_from_civil(date: IsoDate) -> i64 {
    let y = i64::from(date.year) - i64::from(date.month <= 2);
    let m = i64::from(date.month);
    let d = i64::from(date.day);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn civil_from_days(z: i64) -> IsoDate {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(m <= 2);
    IsoDate {
        year: year as i32,
        month: m as u8,
        day: d as u8,
    }
}

// ------------------------------------------------------------------ validation

/// Outcome of validating the curated metadata table and provider catalog entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub errors: Vec<String>,
    pub provider_count: usize,
    pub model_count: usize,
    pub deprecated_count: usize,
}

impl ValidationReport {
    /// Whether validation passed with zero errors.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Load every `*.toml` provider entry in `dir` as a flat list, preserving
/// duplicates so [`validate_catalog`] can report duplicate provider ids.
///
/// # Errors
/// Returns one error string per unreadable or malformed file (or unreadable directory).
pub fn load_catalog_entries_from_dir(dir: &Path) -> Result<Vec<ProviderEntry>, Vec<String>> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| {
        vec![format!(
            "cannot read catalog directory {}: {e}",
            dir.display()
        )]
    })?;
    let mut paths = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("cannot read {}: {e}", path.display()));
                continue;
            }
        };
        match toml::from_str::<ProviderEntry>(&content) {
            Ok(entry) => entries.push(entry),
            Err(e) => errors.push(format!("cannot parse {}: {e}", path.display())),
        }
    }
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// Validate the curated metadata table, the provider catalog TOML entries,
/// cross-references (aliases, provider/model coverage), and optionally the
/// documentation table in `docs_markdown`.
#[must_use]
pub fn validate_catalog(
    table: &CuratedModels,
    entries: &[ProviderEntry],
    docs_markdown: Option<&str>,
) -> ValidationReport {
    let mut errors = table.validation_errors();

    // Verify every alias resolves back to its declaring canonical provider.
    for provider in &table.providers {
        for alias in &provider.aliases {
            match table.find_provider(alias) {
                Some(resolved) if resolved.id == provider.id => {}
                Some(other) => errors.push(format!(
                    "alias `{alias}` on `{}` resolves ambiguously to `{}`",
                    provider.id, other.id
                )),
                None => errors.push(format!(
                    "alias `{alias}` on `{}` failed to resolve",
                    provider.id
                )),
            }
        }
    }

    validate_provider_entries(table, entries, &mut errors);

    if let Some(markdown) = docs_markdown
        && let Err(err) = verify_docs_table(table, markdown)
    {
        errors.push(err);
    }

    let model_count = table.providers.iter().map(|p| p.models.len()).sum();
    let deprecated_count = table
        .providers
        .iter()
        .flat_map(|p| &p.models)
        .filter(|m| m.deprecated)
        .count();

    ValidationReport {
        errors,
        provider_count: table.providers.len(),
        model_count,
        deprecated_count,
    }
}

fn validate_provider_entries(
    table: &CuratedModels,
    entries: &[ProviderEntry],
    errors: &mut Vec<String>,
) {
    if entries.is_empty() {
        errors.push("provider catalog has no TOML entries".into());
        return;
    }

    let mut seen_providers = BTreeSet::new();
    for entry in entries {
        if !safe_model_id(&entry.id) {
            errors.push(format!(
                "catalog TOML has invalid provider id `{}`",
                entry.id
            ));
        } else if !seen_providers.insert(entry.id.as_str()) {
            errors.push(format!(
                "catalog TOML has duplicate provider id `{}`",
                entry.id
            ));
        }

        if !has_http_scheme(&entry.base_url) {
            errors.push(format!(
                "catalog provider `{}` base_url must start with http:// or https://",
                entry.id
            ));
        }
        if let Some(ep) = &entry.models_endpoint
            && !has_http_scheme(ep)
        {
            errors.push(format!(
                "catalog provider `{}` models_endpoint must start with http:// or https://",
                entry.id
            ));
        }
        if let Auth::ApiKeyEnv(var) = &entry.auth
            && (var.is_empty() || !var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            errors.push(format!(
                "catalog provider `{}` has invalid ApiKeyEnv `{var}`",
                entry.id
            ));
        }

        let curated_provider = table.find_provider(&entry.id);
        if curated_provider.is_none() {
            errors.push(format!(
                "catalog provider `{}` does not resolve in model_metadata.json",
                entry.id
            ));
        }

        let mut seen_models = BTreeSet::new();
        for model in &entry.models {
            validate_model_info(&entry.id, model, &mut seen_models, curated_provider, errors);
        }
    }
}

fn validate_model_info<'a>(
    provider_id: &str,
    model: &'a ModelInfo,
    seen_models: &mut BTreeSet<&'a str>,
    curated_provider: Option<&ProviderMetadata>,
    errors: &mut Vec<String>,
) {
    if !safe_model_id(&model.id) {
        errors.push(format!(
            "catalog provider `{provider_id}` has invalid model id `{}`",
            model.id
        ));
    } else if !seen_models.insert(model.id.as_str()) {
        errors.push(format!(
            "catalog provider `{provider_id}` has duplicate model id `{}`",
            model.id
        ));
    }
    if model.id.ends_with(":free") {
        errors.push(format!(
            "catalog provider `{provider_id}` must not hard-wire unstable :free model `{}`",
            model.id
        ));
    }

    if let Some(cw) = model.context_window
        && !(MIN_CONTEXT_WINDOW..=MAX_CONTEXT_WINDOW).contains(&cw)
    {
        errors.push(format!(
            "catalog `{provider_id}/{}` context_window {cw} is outside sane range {MIN_CONTEXT_WINDOW}..={MAX_CONTEXT_WINDOW}",
            model.id
        ));
    }
    if let Some(max_out) = model.max_output_tokens {
        if !(1..=MAX_OUTPUT_TOKENS).contains(&max_out) {
            errors.push(format!(
                "catalog `{provider_id}/{}` max_output_tokens {max_out} is outside sane range 1..={MAX_OUTPUT_TOKENS}",
                model.id
            ));
        }
        if let Some(cw) = model.context_window
            && max_out > cw
        {
            errors.push(format!(
                "catalog `{provider_id}/{}` max_output_tokens ({max_out}) exceeds context_window ({cw})",
                model.id
            ));
        }
    }

    validate_model_pricing(provider_id, model, errors);

    if let Some(provider_meta) = curated_provider
        && !provider_meta.models.iter().any(|m| m.id == model.id)
    {
        errors.push(format!(
            "catalog `{provider_id}/{}` is missing from model_metadata.json",
            model.id
        ));
    }
}

fn validate_model_pricing(provider_id: &str, model: &ModelInfo, errors: &mut Vec<String>) {
    for (field, value) in [
        ("input_per_mtok", model.input_per_mtok),
        ("output_per_mtok", model.output_per_mtok),
        ("cached_per_mtok", model.cached_per_mtok),
        ("cache_write_per_mtok", model.cache_write_per_mtok),
    ] {
        if let Some(price) = value
            && (!price.is_finite() || price < 0.0 || price > MAX_PRICE_PER_MTOK)
        {
            errors.push(format!(
                "catalog `{provider_id}/{}` {field} ({price}) is outside sane range 0.0..={MAX_PRICE_PER_MTOK}",
                model.id
            ));
        }
    }

    if model.input_per_mtok.is_some() != model.output_per_mtok.is_some() {
        errors.push(format!(
            "catalog `{provider_id}/{}` must specify both input_per_mtok and output_per_mtok together",
            model.id
        ));
    }
    if let (Some(cached), Some(input)) = (model.cached_per_mtok, model.input_per_mtok)
        && cached.is_finite()
        && input.is_finite()
        && cached > input
    {
        errors.push(format!(
            "catalog `{provider_id}/{}` cached_per_mtok ({cached}) exceeds input_per_mtok ({input})",
            model.id
        ));
    }

    let has_any_price = model.input_per_mtok.is_some()
        || model.output_per_mtok.is_some()
        || model.cached_per_mtok.is_some()
        || model.cache_write_per_mtok.is_some();
    match &model.prices_as_of {
        Some(date) => {
            if let Err(err) = IsoDate::parse(date) {
                errors.push(format!(
                    "catalog `{provider_id}/{}` has invalid prices_as_of `{date}`: {err}",
                    model.id
                ));
            }
        }
        None if has_any_price => {
            errors.push(format!(
                "catalog `{provider_id}/{}` records prices without a prices_as_of date",
                model.id
            ));
        }
        None => {}
    }
}

fn has_http_scheme(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

// ------------------------------------------------------------------ docs table

/// Render the canonical provider/recommendation summary table for
/// `docs/guide/model-catalog.md` directly from `model_metadata.json`.
#[must_use]
pub fn render_docs_table(table: &CuratedModels) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Last reviewed on {}:\n", table.last_reviewed);
    out.push_str("| Provider | Aliases | Last reviewed | Candidate recommendations | Sources |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    for provider in &table.providers {
        let aliases = if provider.aliases.is_empty() {
            "—".to_string()
        } else {
            provider
                .aliases
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let reviewed = table.effective_last_reviewed(provider);
        let mut recommended: Vec<_> = provider.models.iter().filter(|m| m.recommended).collect();
        recommended.sort_by_key(|m| m.recommended_rank.unwrap_or(u8::MAX));
        let rec_cell = if recommended.is_empty() {
            "None (depends on installed local models)".to_string()
        } else {
            recommended
                .iter()
                .map(|m| format!("{} (`{}`)", m.display_name, m.id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let sources = provider
            .sources
            .iter()
            .map(|url| format!("<{url}>"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "| `{}` | {aliases} | {reviewed} | {rec_cell} | {sources} |",
            provider.id
        );
    }
    out.trim_end().to_string()
}

/// Verify that the marker-delimited table in `docs_markdown` matches
/// [`render_docs_table`] for `table`.
///
/// # Errors
/// Returns a descriptive drift message if the markers are missing or the table
/// content disagrees with `model_metadata.json`.
pub fn verify_docs_table(table: &CuratedModels, docs_markdown: &str) -> Result<(), String> {
    let Some(start) = docs_markdown.find(DOCS_TABLE_BEGIN) else {
        return Err(format!(
            "docs/guide/model-catalog.md is missing opening marker `{DOCS_TABLE_BEGIN}`"
        ));
    };
    let after_start = start + DOCS_TABLE_BEGIN.len();
    let Some(rel_end) = docs_markdown[after_start..].find(DOCS_TABLE_END) else {
        return Err(format!(
            "docs/guide/model-catalog.md is missing closing marker `{DOCS_TABLE_END}`"
        ));
    };
    let actual = docs_markdown[after_start..after_start + rel_end]
        .replace("\r\n", "\n")
        .trim()
        .to_string();
    let expected = render_docs_table(table);
    if actual != expected {
        return Err(
            "docs/guide/model-catalog.md table disagrees with model_metadata.json; run `bot catalog render-docs` and update the table between the markers"
                .into(),
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ staleness

/// Freshness summary for one curated provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFreshness {
    pub id: String,
    pub aliases: Vec<String>,
    pub last_reviewed: String,
    pub age_days: i64,
    pub stale: bool,
    pub model_count: usize,
    pub deprecated_count: usize,
}

/// Overall freshness summary for a curated metadata table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalenessReport {
    pub last_reviewed: String,
    pub oldest_reviewed: String,
    pub age_days: i64,
    pub max_age_days: u32,
    pub stale: bool,
    pub providers: Vec<ProviderFreshness>,
}

/// Evaluate staleness of the embedded curated table against `now`.
///
/// # Errors
/// Returns an error if `now` is before the Unix epoch or a review date is invalid.
pub fn check_staleness_embedded(
    now: SystemTime,
    max_age_days: u32,
) -> Result<StalenessReport, String> {
    check_staleness(embedded_metadata(), now, max_age_days)
}

/// Evaluate staleness of `table` against `now` with threshold `max_age_days`.
///
/// A table is considered stale when its age is strictly greater than
/// `max_age_days` (e.g. older than 90 days -> 91+ days).
///
/// # Errors
/// Returns an error if `now` is before the Unix epoch or a review date is invalid.
pub fn check_staleness(
    table: &CuratedModels,
    now: SystemTime,
    max_age_days: u32,
) -> Result<StalenessReport, String> {
    let today = IsoDate::from_system_time(now)
        .ok_or_else(|| "system clock is before 1970-01-01".to_string())?;
    let top_date = IsoDate::parse(&table.last_reviewed)
        .map_err(|e| format!("invalid last_reviewed date `{}`: {e}", table.last_reviewed))?;
    let threshold = i64::from(max_age_days);

    let mut oldest_date = top_date;
    let mut providers = Vec::with_capacity(table.providers.len());
    for provider in &table.providers {
        let date_str = table.effective_last_reviewed(provider);
        let date = IsoDate::parse(date_str).map_err(|e| {
            format!(
                "provider `{}` has invalid last_reviewed date `{date_str}`: {e}",
                provider.id
            )
        })?;
        if date < oldest_date {
            oldest_date = date;
        }
        let age_days = today.days_since(date);
        let deprecated_count = provider.models.iter().filter(|m| m.deprecated).count();
        providers.push(ProviderFreshness {
            id: provider.id.clone(),
            aliases: provider.aliases.clone(),
            last_reviewed: date.to_string(),
            age_days,
            stale: age_days > threshold,
            model_count: provider.models.len(),
            deprecated_count,
        });
    }

    let age_days = today.days_since(oldest_date);
    Ok(StalenessReport {
        last_reviewed: top_date.to_string(),
        oldest_reviewed: oldest_date.to_string(),
        age_days,
        max_age_days,
        stale: age_days > threshold,
        providers,
    })
}

/// Format a combined validation + staleness report for terminal or CI summary output.
#[must_use]
pub fn format_check_report(
    validation: &ValidationReport,
    staleness: Option<&StalenessReport>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Catalog validation: {} ({} provider(s), {} curated model(s), {} deprecated)",
        if validation.is_ok() { "OK" } else { "FAILED" },
        validation.provider_count,
        validation.model_count,
        validation.deprecated_count,
    );
    for err in &validation.errors {
        let _ = writeln!(out, "  ERROR: {err}");
    }

    if let Some(report) = staleness {
        let status = if report.stale {
            format!(
                "STALE (older than {} days — review recommended)",
                report.max_age_days
            )
        } else {
            "FRESH".to_string()
        };
        let _ = writeln!(
            out,
            "Catalog freshness:  {status} (last_reviewed={}, oldest={}, age={} day(s))",
            report.last_reviewed, report.oldest_reviewed, report.age_days
        );
        out.push_str("\nProviders:\n");
        for p in &report.providers {
            let state = if p.stale { "stale" } else { "fresh" };
            let aliases = if p.aliases.is_empty() {
                String::new()
            } else {
                format!(" [aliases: {}]", p.aliases.join(", "))
            };
            let _ = writeln!(
                out,
                "  - {:<14} reviewed {} ({:>3} d, {:<5})  models={:<2} deprecated={}{aliases}",
                p.id, p.last_reviewed, p.age_days, state, p.model_count, p.deprecated_count
            );
        }
    }
    out
}

// ------------------------------------------------------------------ diff tool

/// One model entry parsed from a JSON or TOML live-listing snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotModel {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub input_per_mtok: Option<f64>,
    #[serde(default)]
    pub output_per_mtok: Option<f64>,
    #[serde(default)]
    pub deprecated: Option<bool>,
}

impl SnapshotModel {
    /// Construct a minimal snapshot model from an id only (e.g. live `/v1/models` id list).
    #[must_use]
    pub fn from_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            input_per_mtok: None,
            output_per_mtok: None,
            deprecated: None,
        }
    }
}

/// Parse a provider model listing snapshot from JSON or TOML text.
///
/// Supported shapes:
/// - JSON array of model id strings: `["claude-sonnet-5-5", ...]`
/// - JSON array of model objects: `[{"id": "...", ...}]`
/// - `OpenAI` / Anthropic / `OpenRouter` JSON object: `{"data": [...]}`
/// - Gemini / Ollama JSON object: `{"models": [...]}` (strips `models/` prefix)
/// - TOML table with `models = ["..."]` or `[[models]]` array of tables
///
/// # Errors
/// Returns a descriptive error if the content cannot be parsed as a supported
/// JSON or TOML model listing.
pub fn parse_snapshot(
    content: &str,
    path_hint: Option<&Path>,
) -> Result<Vec<SnapshotModel>, String> {
    let ext = path_hint
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    let prefer_toml = ext.as_deref() == Some("toml");
    let prefer_json = ext.as_deref() == Some("json");

    if prefer_toml {
        return parse_toml_snapshot(content);
    }
    if prefer_json {
        return parse_json_snapshot(content);
    }

    match parse_json_snapshot(content) {
        Ok(models) => Ok(models),
        Err(json_err) => parse_toml_snapshot(content).map_err(|toml_err| {
            format!("could not parse snapshot as JSON ({json_err}) or TOML ({toml_err})")
        }),
    }
}

fn parse_json_snapshot(content: &str) -> Result<Vec<SnapshotModel>, String> {
    let value: JsonValue =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON snapshot: {e}"))?;
    extract_models_from_json(&value)
}

fn parse_toml_snapshot(content: &str) -> Result<Vec<SnapshotModel>, String> {
    let value: toml::Value =
        toml::from_str(content).map_err(|e| format!("invalid TOML snapshot: {e}"))?;
    let json =
        serde_json::to_value(value).map_err(|e| format!("cannot convert TOML snapshot: {e}"))?;
    extract_models_from_json(&json)
}

fn extract_models_from_json(value: &JsonValue) -> Result<Vec<SnapshotModel>, String> {
    let items = match value {
        JsonValue::Array(arr) => arr,
        JsonValue::Object(map) => {
            if let Some(JsonValue::Array(arr)) = map.get("data") {
                arr
            } else if let Some(JsonValue::Array(arr)) = map.get("models") {
                arr
            } else {
                return Err("snapshot object must contain a `data` or `models` array".into());
            }
        }
        _ => return Err("snapshot must be a JSON array or object with `data`/`models`".into()),
    };

    let mut out = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for (idx, item) in items.iter().enumerate() {
        let model = match item {
            JsonValue::String(id) => SnapshotModel::from_id(normalize_snapshot_id(id)),
            JsonValue::Object(obj) => {
                let raw_id = obj
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .or_else(|| obj.get("name").and_then(JsonValue::as_str))
                    .ok_or_else(|| {
                        format!("snapshot entry #{idx} is missing string `id` or `name`")
                    })?;
                let id = normalize_snapshot_id(raw_id);
                let display_name = obj
                    .get("display_name")
                    .or_else(|| obj.get("displayName"))
                    .and_then(JsonValue::as_str)
                    .or_else(|| {
                        // OpenRouter uses `name` as the display name when `id` is also set.
                        obj.get("id")
                            .and_then(JsonValue::as_str)
                            .and_then(|_| obj.get("name").and_then(JsonValue::as_str))
                    })
                    .map(ToOwned::to_owned);
                let context_window = extract_u32(
                    obj,
                    &["context_window", "context_length", "inputTokenLimit"],
                );
                let max_output_tokens =
                    extract_u32(obj, &["max_output_tokens", "outputTokenLimit"]);
                let input_per_mtok = obj.get("input_per_mtok").and_then(JsonValue::as_f64);
                let output_per_mtok = obj.get("output_per_mtok").and_then(JsonValue::as_f64);
                let deprecated = obj.get("deprecated").and_then(JsonValue::as_bool);
                SnapshotModel {
                    id,
                    display_name,
                    context_window,
                    max_output_tokens,
                    input_per_mtok,
                    output_per_mtok,
                    deprecated,
                }
            }
            _ => {
                return Err(format!(
                    "snapshot entry #{idx} must be a model id string or object"
                ));
            }
        };
        if safe_model_id(&model.id) && seen.insert(model.id.clone()) {
            out.push(model);
        }
    }
    Ok(out)
}

fn normalize_snapshot_id(raw: &str) -> String {
    raw.trim()
        .strip_prefix("models/")
        .unwrap_or_else(|| raw.trim())
        .to_string()
}

fn extract_u32(map: &serde_json::Map<String, JsonValue>, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(JsonValue::as_u64))
        .and_then(|v| u32::try_from(v).ok())
}

/// A model present in the live snapshot but missing from the committed table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedModel {
    pub id: String,
    pub inferred_display_name: String,
    pub inferred_kind: ModelKind,
    pub is_free: bool,
}

/// A model present in the committed table but absent from the live snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedModel {
    pub id: String,
    pub display_name: String,
    pub recommended: bool,
    pub deprecated: bool,
}

/// One changed field between committed catalog metadata/limits and a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub committed: String,
    pub snapshot: String,
}

/// A model present in both committed catalog and snapshot with differing fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedModel {
    pub id: String,
    pub changes: Vec<FieldChange>,
}

/// Heuristic pairing between a removed id and a newly added id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LikelyRename {
    pub from: String,
    pub to: String,
    pub score: f64,
}

/// Result of diffing one provider's committed catalog against a live snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogDiff {
    pub provider: String,
    pub resolved_provider: String,
    pub committed_count: usize,
    pub snapshot_count: usize,
    pub added: Vec<AddedModel>,
    pub removed: Vec<RemovedModel>,
    pub changed: Vec<ChangedModel>,
    pub likely_renames: Vec<LikelyRename>,
}

impl CatalogDiff {
    /// Whether the snapshot agrees completely with the committed catalog.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Compare the committed catalog for `provider` (id or alias) against `snapshot`.
///
/// # Errors
/// Returns an error if `provider` does not resolve in `table`.
pub fn diff_provider(
    table: &CuratedModels,
    catalog_entry: Option<&ProviderEntry>,
    provider: &str,
    snapshot: &[SnapshotModel],
) -> Result<CatalogDiff, String> {
    let curated = table.find_provider(provider).ok_or_else(|| {
        let known: Vec<&str> = table.providers.iter().map(|p| p.id.as_str()).collect();
        format!(
            "unknown provider `{provider}`; known providers: {}",
            known.join(", ")
        )
    })?;

    let committed_meta: BTreeMap<&str, &crate::model_metadata::ModelMetadata> =
        curated.models.iter().map(|m| (m.id.as_str(), m)).collect();
    let committed_info: BTreeMap<&str, &ModelInfo> = catalog_entry
        .map(|entry| {
            entry
                .models
                .iter()
                .map(|m| (m.id.as_str(), m))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut snapshot_map: BTreeMap<&str, &SnapshotModel> = BTreeMap::new();
    for item in snapshot {
        if safe_model_id(&item.id) {
            snapshot_map.entry(item.id.as_str()).or_insert(item);
        }
    }

    let mut added = Vec::new();
    let mut changed = Vec::new();
    for (id, live) in &snapshot_map {
        if let Some(meta) = committed_meta.get(id) {
            let info = committed_info.get(id).copied();
            let field_changes = compare_model_fields(meta, info, live);
            if !field_changes.is_empty() {
                changed.push(ChangedModel {
                    id: (*id).to_string(),
                    changes: field_changes,
                });
            }
        } else {
            let inferred = inferred_metadata(id);
            added.push(AddedModel {
                id: (*id).to_string(),
                inferred_display_name: live.display_name.clone().unwrap_or(inferred.display_name),
                inferred_kind: inferred.kind,
                is_free: id.ends_with(":free"),
            });
        }
    }

    let mut removed = Vec::new();
    for (id, meta) in &committed_meta {
        if !snapshot_map.contains_key(id) {
            removed.push(RemovedModel {
                id: (*id).to_string(),
                display_name: meta.display_name.clone(),
                recommended: meta.recommended,
                deprecated: meta.deprecated,
            });
        }
    }

    let likely_renames = detect_likely_renames(&removed, &added);

    Ok(CatalogDiff {
        provider: provider.to_string(),
        resolved_provider: curated.id.clone(),
        committed_count: committed_meta.len(),
        snapshot_count: snapshot_map.len(),
        added,
        removed,
        changed,
        likely_renames,
    })
}

fn compare_model_fields(
    meta: &crate::model_metadata::ModelMetadata,
    info: Option<&ModelInfo>,
    live: &SnapshotModel,
) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    if let Some(live_name) = &live.display_name
        && !live_name.trim().is_empty()
        && live_name.trim() != meta.display_name.trim()
    {
        changes.push(FieldChange {
            field: "display_name".into(),
            committed: meta.display_name.clone(),
            snapshot: live_name.trim().to_string(),
        });
    }
    if let Some(live_dep) = live.deprecated
        && live_dep != meta.deprecated
    {
        changes.push(FieldChange {
            field: "deprecated".into(),
            committed: meta.deprecated.to_string(),
            snapshot: live_dep.to_string(),
        });
    }
    if let Some(live_cw) = live.context_window
        && let Some(committed_cw) = info.and_then(|i| i.context_window)
        && live_cw != committed_cw
    {
        changes.push(FieldChange {
            field: "context_window".into(),
            committed: committed_cw.to_string(),
            snapshot: live_cw.to_string(),
        });
    }
    if let Some(live_max_out) = live.max_output_tokens
        && let Some(committed_max_out) = info.and_then(|i| i.max_output_tokens)
        && live_max_out != committed_max_out
    {
        changes.push(FieldChange {
            field: "max_output_tokens".into(),
            committed: committed_max_out.to_string(),
            snapshot: live_max_out.to_string(),
        });
    }
    if let Some(live_in) = live.input_per_mtok
        && let Some(committed_in) = info.and_then(|i| i.input_per_mtok)
        && (live_in - committed_in).abs() > 1e-9
    {
        changes.push(FieldChange {
            field: "input_per_mtok".into(),
            committed: format!("{committed_in}"),
            snapshot: format!("{live_in}"),
        });
    }
    if let Some(live_out) = live.output_per_mtok
        && let Some(committed_out) = info.and_then(|i| i.output_per_mtok)
        && (live_out - committed_out).abs() > 1e-9
    {
        changes.push(FieldChange {
            field: "output_per_mtok".into(),
            committed: format!("{committed_out}"),
            snapshot: format!("{live_out}"),
        });
    }
    changes
}

fn detect_likely_renames(removed: &[RemovedModel], added: &[AddedModel]) -> Vec<LikelyRename> {
    let mut candidates = Vec::new();
    for rem in removed {
        let rem_tokens = id_tokens(&rem.id);
        if rem_tokens.is_empty() {
            continue;
        }
        let mut best: Option<(&str, f64)> = None;
        for add in added {
            let add_tokens = id_tokens(&add.id);
            let score = token_similarity(&rem_tokens, &add_tokens);
            if score >= 0.5 && best.is_none_or(|(_, s)| score > s) {
                best = Some((add.id.as_str(), score));
            }
        }
        if let Some((to, score)) = best {
            candidates.push(LikelyRename {
                from: rem.id.clone(),
                to: to.to_string(),
                score,
            });
        }
    }
    candidates
}

fn id_tokens(id: &str) -> BTreeSet<String> {
    id.to_ascii_lowercase()
        .split(['-', '_', '/', ':', '.'])
        .filter(|tok| !tok.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[allow(clippy::cast_precision_loss)]
fn token_similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    inter as f64 / union as f64
}

/// Format a [`CatalogDiff`] as a readable report for terminal output.
#[must_use]
pub fn format_diff_report(diff: &CatalogDiff) -> String {
    let mut out = String::new();
    let alias_note = if diff.provider == diff.resolved_provider {
        String::new()
    } else {
        format!(" (resolved to `{}`)", diff.resolved_provider)
    };
    let _ = writeln!(
        out,
        "Provider `{}`{alias_note}: {} committed, {} in snapshot",
        diff.provider, diff.committed_count, diff.snapshot_count
    );
    if diff.is_empty() {
        out.push_str("No differences found.\n");
        return out;
    }

    if !diff.added.is_empty() {
        let _ = writeln!(out, "\nAdded ({}):", diff.added.len());
        for m in &diff.added {
            let free = if m.is_free {
                " [free — do not pin]"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  + {} ({}, kind={}){free}",
                m.id,
                m.inferred_display_name,
                m.inferred_kind.as_str()
            );
        }
    }

    if !diff.removed.is_empty() {
        let _ = writeln!(out, "\nRemoved ({}):", diff.removed.len());
        for m in &diff.removed {
            let mut flags = Vec::new();
            if m.recommended {
                flags.push("recommended");
            }
            if m.deprecated {
                flags.push("deprecated");
            }
            let suffix = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", flags.join(", "))
            };
            let _ = writeln!(out, "  - {} ({}){suffix}", m.id, m.display_name);
        }
    }

    if !diff.changed.is_empty() {
        let _ = writeln!(out, "\nChanged ({}):", diff.changed.len());
        for m in &diff.changed {
            let _ = writeln!(out, "  ~ {}", m.id);
            for c in &m.changes {
                let _ = writeln!(out, "      {}: {} -> {}", c.field, c.committed, c.snapshot);
            }
        }
    }

    if !diff.likely_renames.is_empty() {
        let _ = writeln!(out, "\nLikely renames ({}):", diff.likely_renames.len());
        for r in &diff.likely_renames {
            let _ = writeln!(out, "  ? {} -> {} (score {:.2})", r.from, r.to, r.score);
        }
    }
    out
}

#[cfg(test)]
mod tests;
