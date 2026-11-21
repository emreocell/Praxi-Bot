//! `bot catalog check|diff|render-docs`: offline model catalog validation,
//! snapshot diffing, and staleness inspection.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::Subcommand;
use provider_catalog::{
    Catalog, CuratedModels, DEFAULT_STALE_AFTER_DAYS, OpenAiCompatConfig, ProviderConfig,
    ProviderEntry, SnapshotModel, check_staleness, config_path, diff_provider, embedded_metadata,
    format_check_report, format_diff_report, load_catalog_entries_from_dir, parse_snapshot,
    render_docs_table, validate_catalog,
};
use secret_store::SecretStore;
use serde_json::json;

use crate::{Result, commands, wizard::SetupEnv};

/// Subcommands under `bot catalog`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CatalogCmd {
    /// Validate the curated model metadata and provider catalog entries, and report freshness.
    Check {
        /// Path to a `model_metadata.json` file (defaults to the embedded catalog table).
        #[arg(long)]
        metadata: Option<PathBuf>,
        /// Optional path to `docs/guide/model-catalog.md` to verify the table stays in sync.
        #[arg(long)]
        docs: Option<PathBuf>,
        /// Warn when the catalog review date is older than this many days.
        #[arg(long, default_value_t = DEFAULT_STALE_AFTER_DAYS)]
        max_age_days: u32,
        /// Exit non-zero if the catalog is stale (validation errors always exit non-zero).
        #[arg(long)]
        strict: bool,
    },
    /// Compare the committed catalog for a provider against a JSON/TOML model-list snapshot.
    ///
    /// Read-only: never modifies catalog files and never sends the catalog anywhere.
    /// Pass a snapshot file for offline diffing, or `--live` to query a configured
    /// provider with your own stored key.
    Diff {
        /// Provider ID or alias (for example `anthropic`, `gemini`, `gemini-openai-compat`).
        provider: String,
        /// Path to a JSON or TOML snapshot of the provider's model list (offline).
        #[arg(value_name = "SNAPSHOT", conflicts_with = "live")]
        snapshot: Option<PathBuf>,
        /// Opt-in: fetch the live model list from your configured provider using its stored key.
        #[arg(long, conflicts_with = "snapshot")]
        live: bool,
        /// Path to a `model_metadata.json` file (defaults to the embedded catalog table).
        #[arg(long)]
        metadata: Option<PathBuf>,
    },
    /// Print the Markdown provider table generated from `model_metadata.json`.
    RenderDocs {
        /// Path to a `model_metadata.json` file (defaults to the embedded catalog table).
        #[arg(long)]
        metadata: Option<PathBuf>,
    },
}

fn load_metadata(path: Option<&Path>) -> Result<CuratedModels> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read metadata file {}: {e}", p.display()))?;
            CuratedModels::from_json(&text).map_err(Into::into)
        }
        None => Ok(embedded_metadata().clone()),
    }
}

fn load_entries(
    catalog_dir: Option<&Path>,
    builtin_sources: &[&str],
) -> Result<Vec<ProviderEntry>> {
    if let Some(dir) = catalog_dir {
        return load_catalog_entries_from_dir(dir).map_err(|errs| errs.join("; ").into());
    }
    let mut entries = Vec::with_capacity(builtin_sources.len());
    for src in builtin_sources {
        entries.push(toml::from_str::<ProviderEntry>(src)?);
    }
    Ok(entries)
}

/// Run offline `bot catalog check`, `bot catalog diff <provider> <snapshot>`, or
/// `bot catalog render-docs`. Never touches the secret store or the network.
///
/// # Errors
/// Returns an error when catalog validation fails, input files are unreadable/malformed,
/// or `--strict` is set and the catalog is stale.
pub fn run_offline(
    action: &CatalogCmd,
    catalog_dir: Option<&Path>,
    builtin_sources: &[&str],
    json_out: bool,
    now: SystemTime,
    out: &mut dyn Write,
) -> Result<()> {
    match action {
        CatalogCmd::Check {
            metadata,
            docs,
            max_age_days,
            strict,
        } => {
            let table = load_metadata(metadata.as_deref())?;
            let entries = load_entries(catalog_dir, builtin_sources)?;
            let docs_text = match docs {
                Some(path) => Some(
                    std::fs::read_to_string(path)
                        .map_err(|e| format!("cannot read docs file {}: {e}", path.display()))?,
                ),
                None => None,
            };
            let validation = validate_catalog(&table, &entries, docs_text.as_deref());
            let staleness = check_staleness(&table, now, *max_age_days).ok();

            if json_out {
                let payload = json!({
                    "valid": validation.is_ok(),
                    "validation": validation,
                    "staleness": staleness,
                });
                writeln!(out, "{}", serde_json::to_string_pretty(&payload)?)?;
            } else {
                write!(
                    out,
                    "{}",
                    format_check_report(&validation, staleness.as_ref())
                )?;
            }

            if !validation.is_ok() {
                return Err(format!(
                    "model catalog validation failed with {} error(s)",
                    validation.errors.len()
                )
                .into());
            }
            if *strict && staleness.as_ref().is_some_and(|s| s.stale) {
                return Err(
                    format!("model catalog is stale (older than {max_age_days} days)").into(),
                );
            }
            Ok(())
        }
        CatalogCmd::Diff {
            provider,
            snapshot,
            live,
            metadata,
        } => {
            if *live {
                return Err("live diff must be dispatched through run_live_diff".into());
            }
            let Some(snapshot_path) = snapshot.as_deref() else {
                return Err(
                    "pass a JSON/TOML snapshot file (`bot catalog diff <provider> <file>`) or `--live`"
                        .into(),
                );
            };
            let table = load_metadata(metadata.as_deref())?;
            let entries = load_entries(catalog_dir, builtin_sources)?;
            let catalog_entry = entries.iter().find(|e| e.id == *provider);
            let raw = std::fs::read_to_string(snapshot_path).map_err(|e| {
                format!("cannot read snapshot file {}: {e}", snapshot_path.display())
            })?;
            let models = parse_snapshot(&raw, Some(snapshot_path))?;
            let diff = diff_provider(&table, catalog_entry, provider, &models)?;
            emit_diff(&diff, json_out, out)
        }
        CatalogCmd::RenderDocs { metadata } => {
            let table = load_metadata(metadata.as_deref())?;
            writeln!(out, "{}", render_docs_table(&table))?;
            Ok(())
        }
    }
}

/// Opt-in live `bot catalog diff <provider> --live`: fetches the model list
/// from the user's configured provider (read-only, never writes catalog files).
///
/// # Errors
/// Returns an error if the provider is not configured, the connection fails,
/// or the provider id does not resolve in the catalog metadata.
pub fn run_live_diff(
    provider: &str,
    metadata: Option<&Path>,
    data_dir: &Path,
    catalog: &Catalog,
    store: &dyn SecretStore,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let table = load_metadata(metadata)?;
    let cfg = ProviderConfig::load(&config_path(data_dir))?;
    let settings = cfg.get(provider).ok_or_else(|| {
        format!(
            "provider `{provider}` is not configured; pass a snapshot file for offline diffing, or configure it with `bot providers add {provider}`"
        )
    })?;
    let env = SetupEnv {
        catalog,
        store,
        data_dir,
        connector: &crate::wizard::NetConnector,
    };
    let entry = commands::entry_of(settings, &env)?;
    let built = settings.build_provider(&entry, store, OpenAiCompatConfig::default())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let ids = rt.block_on(built.list_models())?;
    let snapshot: Vec<SnapshotModel> = ids.into_iter().map(SnapshotModel::from_id).collect();
    let diff = diff_provider(&table, Some(&entry), provider, &snapshot)?;
    emit_diff(&diff, json_out, out)
}

fn emit_diff(
    diff: &provider_catalog::CatalogDiff,
    json_out: bool,
    out: &mut dyn Write,
) -> Result<()> {
    if json_out {
        writeln!(out, "{}", serde_json::to_string_pretty(diff)?)?;
    } else {
        write!(out, "{}", format_diff_report(diff))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn builtin() -> &'static [&'static str] {
        crate::BUILTIN_CATALOG
    }

    fn time_for(days_since_epoch: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(days_since_epoch * 86_400)
    }

    #[test]
    fn catalog_check_warns_when_stale_without_failing_unless_strict() {
        // 2026-11-15 is 20772 days after 1970-01-01; +100 days = stale (> 90 days).
        let stale_now = time_for(20_772 + 100);
        let cmd = CatalogCmd::Check {
            metadata: None,
            docs: None,
            max_age_days: 90,
            strict: false,
        };
        let mut buf = Vec::new();
        run_offline(&cmd, None, builtin(), false, stale_now, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("Catalog validation: OK"), "{text}");
        assert!(text.contains("STALE"), "{text}");

        // With --strict, staleness returns an error.
        let strict_cmd = CatalogCmd::Check {
            metadata: None,
            docs: None,
            max_age_days: 90,
            strict: true,
        };
        let mut buf = Vec::new();
        assert!(run_offline(&strict_cmd, None, builtin(), false, stale_now, &mut buf).is_err());
    }

    #[test]
    fn catalog_check_fails_on_malformed_fixture_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let bad_meta = dir.path().join("bad.json");
        std::fs::write(
            &bad_meta,
            r#"{"last_reviewed":"not-a-date","providers":[]}"#,
        )
        .unwrap();

        let cmd = CatalogCmd::Check {
            metadata: Some(bad_meta),
            docs: None,
            max_age_days: 90,
            strict: false,
        };
        let mut buf = Vec::new();
        assert!(run_offline(&cmd, None, builtin(), false, time_for(20_780), &mut buf).is_err());
    }

    #[test]
    fn catalog_diff_offline_supports_json_and_toml_snapshots_and_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let snap_json = dir.path().join("anthropic.json");
        std::fs::write(
            &snap_json,
            r#"{"data":[{"id":"claude-sonnet-5-5","context_window":2000000},{"id":"claude-new-6-0"}]}"#,
        )
        .unwrap();

        let cmd = CatalogCmd::Diff {
            provider: "anthropic".into(),
            snapshot: Some(snap_json),
            live: false,
            metadata: None,
        };
        let mut buf = Vec::new();
        run_offline(&cmd, None, builtin(), true, time_for(20_780), &mut buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["resolved_provider"], "anthropic");
        assert_eq!(parsed["added"][0]["id"], "claude-new-6-0");
        assert!(
            parsed["changed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "claude-sonnet-5-5")
        );

        let snap_toml = dir.path().join("deepseek.toml");
        std::fs::write(
            &snap_toml,
            "models = [\"deepseek-flash\", \"deepseek-v5\"]\n",
        )
        .unwrap();
        let toml_cmd = CatalogCmd::Diff {
            provider: "deepseek".into(),
            snapshot: Some(snap_toml),
            live: false,
            metadata: None,
        };
        let mut text_buf = Vec::new();
        run_offline(
            &toml_cmd,
            None,
            builtin(),
            false,
            time_for(20_780),
            &mut text_buf,
        )
        .unwrap();
        let text = String::from_utf8(text_buf).unwrap();
        assert!(text.contains("+ deepseek-v5"), "{text}");
        assert!(text.contains("- deepseek-v4-pro"), "{text}");
    }
}
