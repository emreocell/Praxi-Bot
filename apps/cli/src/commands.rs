//! `bot secrets ...` and `bot providers ...`.

use provider_catalog::{
    OpenAiCompatConfig, ProviderConfig, ProviderEntry, ProviderSettings, config_path,
    test_connection,
};
use secret_store::{SecretStore, SecretValue};

use crate::{
    Result,
    wizard::{ProviderChoice, SetupEnv, configure_provider, entry_for},
};

/// `bot secrets list`: names only, never values.
///
/// # Errors
/// Store errors.
pub fn secrets_list(store: &dyn SecretStore, out: &mut dyn FnMut(&str)) -> Result<()> {
    let names: Vec<_> = store
        .list_names()?
        .into_iter()
        .filter(|n| !secret_store::is_host_credential(n))
        .collect();
    if names.is_empty() {
        out("(no secrets stored)");
    }
    for n in names {
        out(&n);
    }
    Ok(())
}

/// `bot secrets list --json`: a JSON array of secret names (never values).
///
/// # Errors
/// Store errors.
pub fn secrets_list_json(store: &dyn SecretStore) -> Result<serde_json::Value> {
    let names: Vec<_> = store
        .list_names()?
        .into_iter()
        .filter(|n| !secret_store::is_host_credential(n))
        .collect();
    Ok(serde_json::json!(names))
}

/// `bot secrets set`: the value comes from the environment or a hidden prompt,
/// never from argv.
///
/// # Errors
/// Invalid name, store errors.
pub fn secrets_set(store: &dyn SecretStore, name: &str, value: &SecretValue) -> Result<()> {
    if secret_store::is_host_credential(name) {
        return Err(
            "host credentials cannot be modified with `bot secrets`; use `bot token create` or `bot token revoke`".into(),
        );
    }
    store.set(name, value.expose())?;
    Ok(())
}

/// `bot secrets delete`.
///
/// # Errors
/// Store errors; unknown name.
pub fn secrets_delete(store: &dyn SecretStore, name: &str) -> Result<()> {
    if secret_store::is_host_credential(name) {
        return Err(
            "host credentials cannot be modified with `bot secrets`; use `bot token create` or `bot token revoke`".into(),
        );
    }
    if store.delete(name)? {
        Ok(())
    } else {
        Err(
            format!("secret `{name}` not found; run `bot secrets list` to see stored secret names")
                .into(),
        )
    }
}

/// `bot providers list`.
///
/// # Errors
/// Config errors.
pub fn providers_list(data_dir: &std::path::Path, out: &mut dyn FnMut(&str)) -> Result<()> {
    let cfg = ProviderConfig::load(&config_path(data_dir))?;
    if cfg.providers.is_empty() {
        out("(no providers configured; run `bot setup` or `bot providers add <id>`)");
    }
    let default_id = cfg.effective_default().map(|d| d.id.clone());
    for p in &cfg.providers {
        let mark = if default_id.as_deref() == Some(p.id.as_str()) {
            "*"
        } else {
            " "
        };
        let status = if p.enabled { "" } else { "(disabled)" };
        if p.cli.is_some() {
            out(&format!(
                "{mark} {:<20} kind=cli model={:<24} {}",
                p.id,
                p.default_model.as_deref().unwrap_or("default"),
                status,
            ));
        } else {
            out(&format!(
                "{mark} {:<20} model={:<24} key={} {}",
                p.id,
                p.default_model.as_deref().unwrap_or("-"),
                p.key.as_ref().map_or("-", |k| k.name()),
                status,
            ));
        }
    }
    Ok(())
}

/// `bot providers list --json`: one object per provider
/// (`id`, `default`, `enabled`, `kind` = `api` | `cli`, `model`, `key` = the
/// secret's *name*, never its value; `null` for CLI providers).
///
/// # Errors
/// Unreadable provider config.
pub fn providers_list_json(data_dir: &std::path::Path) -> Result<serde_json::Value> {
    let cfg = ProviderConfig::load(&config_path(data_dir))?;
    let default_id = cfg.effective_default().map(|d| d.id.clone());
    let rows: Vec<_> = cfg
        .providers
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "default": default_id.as_deref() == Some(p.id.as_str()),
                "enabled": p.enabled,
                "kind": if p.cli.is_some() { "cli" } else { "api" },
                "model": p.default_model,
                "key": if p.cli.is_some() { None } else { p.key.as_ref().map(|k| k.name().to_owned()) },
            })
        })
        .collect();
    Ok(serde_json::Value::Array(rows))
}

/// `bot providers add`.
///
/// # Errors
/// Unknown provider, store / config / connection errors.
pub fn providers_add(
    env: &SetupEnv<'_>,
    choice: &ProviderChoice,
    key: Option<&SecretValue>,
    model: Option<&str>,
    test: bool,
    out: &mut dyn FnMut(&str),
) -> Result<ProviderSettings> {
    if test {
        let entry = entry_for(choice, env.catalog)?;
        env.connector.test(&entry, key, None).map_err(|e| {
            format!(
                "provider connection test failed: {e}; verify your API key and network, or pass `--no-test` to save anyway"
            )
        })?;
        out("connection OK");
    }
    let (settings, cfg) = configure_provider(env, choice, key, model)?;
    let default = cfg.effective_default().map(|d| d.id.clone());
    out(&format!(
        "added {} (global default: {})",
        settings.id,
        default.as_deref().unwrap_or("-")
    ));
    Ok(settings)
}

/// The entry to use for an already configured provider.
///
/// # Errors
/// Unknown provider.
pub fn entry_of(settings: &ProviderSettings, env: &SetupEnv<'_>) -> Result<ProviderEntry> {
    if let Some(c) = &settings.custom {
        return Ok(c.clone());
    }
    env.catalog.get(&settings.id).cloned().ok_or_else(|| {
        format!(
            "provider `{}` is not in the catalog; re-add it with `bot providers add {} --base-url <URL>` if it is a custom provider",
            settings.id, settings.id
        )
        .into()
    })
}

/// `bot providers test [ID]` (default provider when omitted).
///
/// # Errors
/// Unknown provider, missing key, failed connection.
pub fn providers_test(
    env: &SetupEnv<'_>,
    id: Option<&str>,
    out: &mut dyn FnMut(&str),
) -> Result<()> {
    let cfg = ProviderConfig::load(&config_path(env.data_dir))?;
    let settings = match id {
        Some(i) => cfg.get(i).ok_or_else(|| {
            format!(
                "provider `{i}` is not configured; run `bot providers list` to see configured providers or `bot providers add {i}` to add it"
            )
        })?,
        None => cfg.effective_default().ok_or(
            "no providers configured yet; run `bot setup` or `bot providers add <id>` first",
        )?,
    };
    let entry = entry_of(settings, env)?;
    let provider = settings.build_provider(&entry, env.store, OpenAiCompatConfig::default())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let model = settings.model_ref();
    let report = rt.block_on(test_connection(&*provider, model.as_ref()))?;
    out(&format!(
        "{}: OK ({} models listed)",
        settings.id,
        report.models.len()
    ));
    Ok(())
}

/// `bot providers detect-local`: one JSON array of the local model servers found on loopback.
///
/// # Errors
/// The async runtime cannot start.
pub fn providers_detect_local(out: &mut dyn FnMut(&str)) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let found = rt.block_on(provider_catalog::detect_local_servers());
    out(&serde_json::to_string(&found)?);
    Ok(())
}

/// `bot providers default ID [--model M]`.
///
/// # Errors
/// Unknown provider, config errors.
pub fn providers_default(
    data_dir: &std::path::Path,
    id: &str,
    model: Option<&str>,
    out: &mut dyn FnMut(&str),
) -> Result<()> {
    let path = config_path(data_dir);
    let _guard = ProviderConfig::lock(&path)?;
    let mut cfg = ProviderConfig::load(&path)?;
    cfg.set_default(id).map_err(|e| {
        format!(
            "cannot set default provider `{id}`: {e}; run `bot providers list` to see configured providers"
        )
    })?;
    if let Some(m) = model {
        cfg.set_model(id, m)?;
    }
    cfg.save(&path)?;
    out(&format!("global default provider: {id}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::MemoryStore;

    fn collect(f: impl FnOnce(&mut dyn FnMut(&str)) -> Result<()>) -> Vec<String> {
        let mut v = Vec::new();
        f(&mut |s: &str| v.push(s.to_string())).unwrap();
        v
    }

    #[test]
    fn secrets_commands_never_print_values() {
        let s = MemoryStore::new();
        secrets_set(&s, "a.b", &SecretValue::new("topsecretvalue")).unwrap();
        let lines = collect(|o| secrets_list(&s, o));
        assert_eq!(lines, ["a.b"]);
        secrets_delete(&s, "a.b").unwrap();
        assert!(secrets_delete(&s, "a.b").is_err());
        assert_eq!(collect(|o| secrets_list(&s, o)), ["(no secrets stored)"]);
    }

    #[test]
    fn json_shapes_of_secrets_and_providers_list() {
        let s = MemoryStore::new();
        secrets_set(&s, "a.b", &SecretValue::new("topsecretvalue")).unwrap();
        let v = secrets_list_json(&s).unwrap();
        assert_eq!(v, serde_json::json!(["a.b"]));
        assert!(!v.to_string().contains("topsecretvalue"));

        let d = tempfile::tempdir().unwrap();
        let mut cfg = ProviderConfig::default();
        let mut a = ProviderSettings::new("a", 1);
        a.default_model = Some("m1".into());
        cfg.upsert(a);
        let mut cli = ProviderSettings::new("cli:claude", 2);
        cli.cli = Some(provider_catalog::config::CliSettings::Cli {
            engine: provider_catalog::config::CliEngine::Claude,
            command: "claude".into(),
        });
        cfg.upsert(cli);
        cfg.save(&config_path(d.path())).unwrap();
        let v = providers_list_json(d.path()).unwrap();
        assert_eq!(v[0]["id"], "a");
        assert_eq!(v[0]["default"], true);
        assert_eq!(v[0]["kind"], "api");
        assert_eq!(v[0]["model"], "m1");
        assert_eq!(v[1]["kind"], "cli");
        assert_eq!(v[1]["default"], false);
        assert!(v[1]["key"].is_null() && v[1]["model"].is_null());
        let mut keys: Vec<_> = v[0].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["default", "enabled", "id", "key", "kind", "model"]);
    }

    #[test]
    fn providers_default_and_list() {
        let d = tempfile::tempdir().unwrap();
        let path = config_path(d.path());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(ProviderSettings::new("a", 1));
        cfg.upsert(ProviderSettings::new("b", 2));
        cfg.save(&path).unwrap();
        let lines = collect(|o| providers_list(d.path(), o));
        assert!(lines[0].starts_with('*') && lines[0].contains(" a "));
        collect(|o| providers_default(d.path(), "b", Some("mm"), o));
        let lines = collect(|o| providers_list(d.path(), o));
        assert!(lines[1].starts_with('*') && lines[1].contains("mm"));
        let mut sink = |_: &str| {};
        assert!(providers_default(d.path(), "zzz", None, &mut sink).is_err());
    }

    #[test]
    fn providers_list_omits_key_for_cli_rows() {
        let d = tempfile::tempdir().unwrap();
        let path = config_path(d.path());
        let mut cfg = ProviderConfig::default();
        let mut cli = ProviderSettings::new("cli:claude", 1);
        cli.cli = Some(provider_catalog::config::CliSettings::Cli {
            engine: provider_catalog::config::CliEngine::Claude,
            command: "claude".into(),
        });
        cfg.upsert(cli);
        cfg.save(&path).unwrap();
        let lines = collect(|o| providers_list(d.path(), o));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("cli:claude"), "{}", lines[0]);
        assert!(!lines[0].contains("key="), "{}", lines[0]);
    }
}
