//! Unit tests for provider setup (`custom_entry`, `entry_for`, `configure_provider`).

use provider_catalog::{
    Auth, Catalog, ProviderChoice, ProviderKind, SetupError, api_key_secret_name, config_path,
    configure_provider, custom_entry, entry_for,
};
use secret_store::{MemoryStore, SecretRef, SecretStore};

#[test]
fn custom_entry_strips_trailing_slash_and_uses_no_inline_auth() {
    let entry = custom_entry("local", "http://127.0.0.1:11434/v1/");

    assert_eq!(entry.id, "local");
    assert_eq!(entry.base_url, "http://127.0.0.1:11434/v1");
    assert_eq!(
        entry.models_endpoint.as_deref(),
        Some("http://127.0.0.1:11434/v1/models")
    );
    assert_eq!(entry.kind, ProviderKind::OpenAiCompatible);
    assert_eq!(entry.auth, Auth::None);
}

#[test]
fn entry_for_rejects_unknown_catalog_id() {
    let err = entry_for(&ProviderChoice::Catalog("nope".into()), &Catalog::default())
        .expect_err("unknown catalog id must fail");

    match err {
        SetupError::Unknown(id) => assert_eq!(id, "nope"),
        other => panic!("expected SetupError::Unknown, got {other:?}"),
    }
}

#[test]
fn configure_provider_readd_keeps_existing_key_and_default_model() {
    let catalog = Catalog::default();
    let store = MemoryStore::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let choice = ProviderChoice::Custom {
        id: "local".into(),
        base_url: "http://127.0.0.1:11434/v1/".into(),
    };
    let expected_secret_name = api_key_secret_name("local");

    let (first_settings, _) = configure_provider(
        &catalog,
        &store,
        dir.path(),
        &choice,
        Some("secret-a"),
        Some("llama3.2"),
        1_000,
    )
    .expect("initial configure_provider");

    assert_eq!(
        first_settings.key.as_ref().map(SecretRef::name),
        Some(expected_secret_name.as_str())
    );
    assert_eq!(first_settings.default_model.as_deref(), Some("llama3.2"));
    assert_eq!(
        store
            .get(&expected_secret_name)
            .expect("store get")
            .expect("secret stored")
            .expose(),
        "secret-a"
    );

    let on_disk = std::fs::read_to_string(config_path(dir.path())).expect("read providers.toml");
    assert!(
        !on_disk.contains("secret-a"),
        "providers.toml must store the secret name, not its value"
    );

    let (readded_settings, cfg) =
        configure_provider(&catalog, &store, dir.path(), &choice, None, None, 2_000)
            .expect("re-add configure_provider");

    assert_eq!(
        readded_settings.key.as_ref().map(SecretRef::name),
        Some(expected_secret_name.as_str())
    );
    assert_eq!(readded_settings.default_model.as_deref(), Some("llama3.2"));
    assert_eq!(
        store
            .get(&expected_secret_name)
            .expect("store get after re-add")
            .expect("secret still stored")
            .expose(),
        "secret-a"
    );

    let persisted = cfg.get("local").expect("provider in config");
    assert_eq!(
        persisted.key.as_ref().map(SecretRef::name),
        Some(expected_secret_name.as_str())
    );
    assert_eq!(persisted.default_model.as_deref(), Some("llama3.2"));
}
