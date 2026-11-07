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
    let names = store.list_names()?;
    if names.is_empty() {
        out("(no secrets stored)");
    }
    for n in names {
        out(&n);
    }
    Ok(())
}

/// `bot secrets set`: the value comes from the environment or a hidden prompt,
/// never from argv.
///
/// # Errors
/// Invalid name, store errors.
pub fn secrets_set(store: &dyn SecretStore, name: &str, value: &SecretValue) -> Result<()> {
    store.set(name, value.expose())?;
    Ok(())
}

/// `bot secrets delete`.
///
/// # Errors
/// Store errors; unknown name.
pub fn secrets_delete(store: &dyn SecretStore, name: &str) -> Result<()> {
    if store.delete(name)? {
        Ok(())
    } else {
        Err(format!("secret {name:?} not found").into())
    }
}

/// `bot providers list`.
///
/// # Errors
/// Config errors.
pub fn providers_list(data_dir: &std::path::Path, out: &mut dyn FnMut(&str)) -> Result<()> {
    let cfg = ProviderConfig::load(&config_path(data_dir))?;
    if cfg.providers.is_empty() {
        out("(no providers configured; run `bot setup`)");
    }
    let default_id = cfg.effective_default().map(|d| d.id.clone());
    for p in &cfg.providers {
        let mark = if default_id.as_deref() == Some(p.id.as_str()) {
            "*"
        } else {
            " "
        };
        out(&format!(
            "{mark} {:<20} model={:<24} key={} {}",
            p.id,
            p.default_model.as_deref().unwrap_or("-"),
            p.key.as_ref().map_or("-", |k| k.name()),
            if p.enabled { "" } else { "(disabled)" }
        ));
    }
    Ok(())
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
        env.connector
            .test(&entry, key, None)
            .map_err(|e| format!("connection test failed (use --no-test to skip): {e}"))?;
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
    env.catalog
        .get(&settings.id)
        .cloned()
        .ok_or_else(|| format!("provider {:?} is not in the catalog", settings.id).into())
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
        Some(i) => cfg.get(i),
        None => cfg.effective_default(),
    }
    .ok_or("no such provider configured")?;
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
    let mut cfg = ProviderConfig::load(&path)?;
    cfg.set_default(id)?;
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
}
