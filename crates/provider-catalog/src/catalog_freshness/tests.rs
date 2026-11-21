use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::entry::{Auth, CompatFlags, ModelCapabilities, ModelInfo, ProviderEntry, ProviderKind};
use crate::model_metadata::{CuratedModels, ModelKind, ModelMetadata};

fn system_time_for_date(date: &str) -> SystemTime {
    let parsed = IsoDate::parse(date).unwrap();
    let days = days_from_civil(parsed);
    assert!(days >= 0);
    UNIX_EPOCH + Duration::from_secs(u64::try_from(days).unwrap() * SECS_PER_DAY)
}

fn load_shipped_entries() -> Vec<ProviderEntry> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/catalog");
    load_catalog_entries_from_dir(&dir).expect("shipped catalog TOMLs parse")
}

fn load_shipped_docs() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide/model-catalog.md");
    std::fs::read_to_string(&path).expect("docs/guide/model-catalog.md is readable")
}

#[test]
fn committed_catalog_and_docs_pass_validation_without_drift() {
    let entries = load_shipped_entries();
    let docs = load_shipped_docs();
    let report = validate_catalog(embedded_metadata(), &entries, Some(&docs));
    assert!(
        report.is_ok(),
        "committed catalog validation failed: {:?}",
        report.errors
    );
    assert!(report.provider_count >= 10);
    assert!(report.model_count >= 25);
    assert!(report.deprecated_count >= 1);
}

#[test]
fn validator_catches_bad_dates_duplicate_display_names_aliases_and_deprecation_flags() {
    let base: CuratedModels =
        CuratedModels::from_json(include_str!("../model_metadata.json")).unwrap();
    let entries = load_shipped_entries();

    // Missing / invalid last_reviewed date.
    let mut bad_date = base.clone();
    bad_date.last_reviewed = "2026-02-30".into();
    assert!(!validate_catalog(&bad_date, &entries, None).is_ok());

    // Duplicate display name within a provider (case-insensitive).
    let mut dup_name = base.clone();
    dup_name.providers[0].models.push(ModelMetadata {
        id: "gemini-4-carbon-copy".into(),
        display_name: "  gemini 4 carbon  ".into(),
        kind: ModelKind::Chat,
        recommended: false,
        recommended_rank: None,
        deprecated: false,
        note: None,
    });
    let report = validate_catalog(&dup_name, &entries, None);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("duplicate display_name")),
        "{:?}",
        report.errors
    );

    // Self-alias / duplicate alias rejected.
    let mut self_alias = base.clone();
    let self_id = self_alias.providers[0].id.clone();
    self_alias.providers[0].aliases.push(self_id);
    assert!(!validate_catalog(&self_alias, &entries, None).is_ok());

    // Deprecated model missing an explanatory note is rejected.
    let mut dep_no_note = base.clone();
    dep_no_note.providers[0].models.push(ModelMetadata {
        id: "gemini-old-model".into(),
        display_name: "Gemini Old Model".into(),
        kind: ModelKind::Chat,
        recommended: false,
        recommended_rank: None,
        deprecated: true,
        note: None,
    });
    assert!(!validate_catalog(&dep_no_note, &entries, None).is_ok());

    // Note says retired/deprecated while deprecated=false is rejected.
    let mut unflagged_retired = base.clone();
    unflagged_retired.providers[0].models.push(ModelMetadata {
        id: "gemini-unflagged-retired".into(),
        display_name: "Gemini Unflagged Retired".into(),
        kind: ModelKind::Chat,
        recommended: false,
        recommended_rank: None,
        deprecated: false,
        note: Some("retired".into()),
    });
    assert!(!validate_catalog(&unflagged_retired, &entries, None).is_ok());
}

#[test]
fn validator_catches_unsane_context_windows_prices_and_broken_cross_references() {
    let base = embedded_metadata();
    let mut entries = load_shipped_entries();

    // Context window too small.
    let mut bad_cw = entries.clone();
    bad_cw
        .iter_mut()
        .find(|e| e.id == "anthropic")
        .unwrap()
        .models[0]
        .context_window = Some(64);
    assert!(!validate_catalog(base, &bad_cw, None).is_ok());

    // max_output_tokens > context_window.
    let mut bad_out = entries.clone();
    let m = &mut bad_out
        .iter_mut()
        .find(|e| e.id == "anthropic")
        .unwrap()
        .models[0];
    m.context_window = Some(4096);
    m.max_output_tokens = Some(8192);
    assert!(!validate_catalog(base, &bad_out, None).is_ok());

    // Negative price or cached > input price.
    let mut bad_price = entries.clone();
    bad_price
        .iter_mut()
        .find(|e| e.id == "anthropic")
        .unwrap()
        .models[0]
        .input_per_mtok = Some(-1.0);
    assert!(!validate_catalog(base, &bad_price, None).is_ok());

    let mut bad_cache = entries.clone();
    let m = &mut bad_cache
        .iter_mut()
        .find(|e| e.id == "anthropic")
        .unwrap()
        .models[0];
    m.cached_per_mtok = Some(m.input_per_mtok.unwrap() + 1.0);
    assert!(!validate_catalog(base, &bad_cache, None).is_ok());

    // Prices present without prices_as_of.
    let mut missing_date = entries.clone();
    missing_date
        .iter_mut()
        .find(|e| e.id == "anthropic")
        .unwrap()
        .models[0]
        .prices_as_of = None;
    assert!(!validate_catalog(base, &missing_date, None).is_ok());

    // Model in TOML missing from model_metadata.json.
    entries
        .iter_mut()
        .find(|e| e.id == "anthropic")
        .unwrap()
        .models
        .push(ModelInfo {
            id: "claude-ghost-9".into(),
            context_window: Some(200_000),
            ..ModelInfo::default()
        });
    assert!(!validate_catalog(base, &entries, None).is_ok());

    // Unknown TOML provider fails alias/id resolution.
    let mut unknown_provider = load_shipped_entries();
    unknown_provider.push(ProviderEntry {
        id: "unregistered-provider".into(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.example.com/v1".into(),
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: true,
            vision: false,
            streaming: true,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
        models_endpoint: None,
        models: Vec::new(),
    });
    assert!(!validate_catalog(base, &unknown_provider, None).is_ok());
}

#[test]
fn docs_sync_check_fails_when_table_drifts_from_json() {
    let docs = load_shipped_docs();
    assert_eq!(verify_docs_table(embedded_metadata(), &docs), Ok(()));

    let drifted = docs.replace("2026-11-15", "2026-01-01");
    assert!(verify_docs_table(embedded_metadata(), &drifted).is_err());

    let no_markers = "# Model picker metadata\nno table markers here\n";
    assert!(verify_docs_table(embedded_metadata(), no_markers).is_err());
}

#[test]
fn staleness_boundary_is_strictly_older_than_90_days() {
    let mut table = CuratedModels::from_json(include_str!("../model_metadata.json")).unwrap();
    table.last_reviewed = "2026-01-01".into();

    // Exactly 90 days after 2026-01-01 is 2026-04-01 -> still fresh.
    let at_90 = check_staleness(&table, system_time_for_date("2026-04-01"), 90).unwrap();
    assert_eq!(at_90.age_days, 90);
    assert!(!at_90.stale);

    // 91 days after 2026-01-01 is 2026-04-02 -> stale.
    let at_91 = check_staleness(&table, system_time_for_date("2026-04-02"), 90).unwrap();
    assert_eq!(at_91.age_days, 91);
    assert!(at_91.stale);

    // An older per-provider review date makes the overall report stale.
    table.last_reviewed = "2026-04-01".into();
    table.providers[0].last_reviewed = Some("2025-12-01".into());
    let per_provider = check_staleness(&table, system_time_for_date("2026-04-02"), 90).unwrap();
    assert!(per_provider.stale);
    assert_eq!(per_provider.oldest_reviewed, "2025-12-01");
    assert!(per_provider.providers[0].stale);
    assert!(!per_provider.providers[1].stale);
}

#[test]
fn diff_parses_json_and_toml_snapshots_and_reports_added_removed_changed_and_renames() {
    let entries = load_shipped_entries();
    let anthropic_entry = entries.iter().find(|e| e.id == "anthropic");

    // JSON OpenAI/Anthropic style snapshot with added, removed, changed, and likely rename.
    let json_snapshot = r#"{
      "data": [
        { "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5 (Updated)", "context_window": 2000000, "input_per_mtok": 1.5 },
        { "id": "claude-opus-5-5" },
        { "id": "claude-haiku-5-6", "display_name": "Claude Haiku 5.6" },
        { "id": "claude-fable-5-2" }
      ]
    }"#;
    let parsed = parse_snapshot(json_snapshot, Some(Path::new("anthropic.json"))).unwrap();
    let diff = diff_provider(embedded_metadata(), anthropic_entry, "anthropic", &parsed).unwrap();

    assert_eq!(diff.resolved_provider, "anthropic");
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.added[0].id, "claude-haiku-5-6");
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.removed[0].id, "claude-haiku-5-5");
    assert!(diff.removed[0].recommended);

    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].id, "claude-sonnet-5-5");
    let fields: Vec<&str> = diff.changed[0]
        .changes
        .iter()
        .map(|c| c.field.as_str())
        .collect();
    assert_eq!(fields, ["display_name", "context_window", "input_per_mtok"]);

    assert_eq!(diff.likely_renames.len(), 1);
    assert_eq!(diff.likely_renames[0].from, "claude-haiku-5-5");
    assert_eq!(diff.likely_renames[0].to, "claude-haiku-5-6");

    // TOML snapshot + alias resolution (gemini-openai-compat -> gemini).
    let toml_snapshot = r#"
[[models]]
id = "models/gemini-4-carbon"
display_name = "Gemini 4 Carbon"

[[models]]
id = "gemini-4-neon"
"#;
    let parsed_toml = parse_snapshot(toml_snapshot, Some(Path::new("gemini.toml"))).unwrap();
    let gemini_diff = diff_provider(
        embedded_metadata(),
        None,
        "gemini-openai-compat",
        &parsed_toml,
    )
    .unwrap();
    assert_eq!(gemini_diff.resolved_provider, "gemini");
    assert!(gemini_diff.added.iter().any(|m| m.id == "gemini-4-neon"));
    assert!(
        gemini_diff
            .removed
            .iter()
            .any(|m| m.id == "gemini-4-argon" && m.recommended)
    );

    // Unknown provider returns an error.
    assert!(diff_provider(embedded_metadata(), None, "no-such-provider", &[]).is_err());

    // OpenRouter :free model is surfaced dynamically without hard-wiring.
    let or_snap = parse_snapshot(r#"["vendor/ephemeral:free"]"#, None).unwrap();
    let or_diff = diff_provider(embedded_metadata(), None, "openrouter", &or_snap).unwrap();
    assert!(
        or_diff
            .added
            .iter()
            .any(|m| m.id == "vendor/ephemeral:free" && m.is_free)
    );
}
