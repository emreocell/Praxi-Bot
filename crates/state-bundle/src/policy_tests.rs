use std::{fs, path::Path, sync::Arc};

use agent_core::{AgentLoop, EffectivePolicy, LoopConfig, Providers};
use daemon_api::{AppState, DaemonConfig};
use permission_engine::{Decision, Policy};
use protocol::PermissionCategory;
use secret_store::MemoryStore;
use tool_registry::ToolRegistry;

use crate::{ImportOptions, import, tests};

fn save_policy(dir: &Path, policy: &Policy) {
    fs::write(dir.join("policy.json"), serde_json::to_vec(policy).unwrap()).unwrap();
}

fn database_with_grant(dir: &Path) {
    tests::database(dir, true);
    let storage = storage::Storage::open(dir).unwrap();
    let user = storage.list_users().unwrap()[0].id;
    let bot = storage.list_bots(user).unwrap()[0].id;
    storage
        .update_bot(
            bot,
            "assistant",
            &serde_json::json!({
                "bot": {"id": "assistant", "name": "assistant"},
                "models": {"executor": "fake/model"}
            }),
        )
        .unwrap();
    storage
        .append_audit(
            user,
            None,
            "user",
            "grant.always",
            &serde_json::json!({"bot_id": bot.to_string(), "category": "shell"}),
        )
        .unwrap();
}

// Exercise the real daemon loader and runtime grant replay without a provider,
// network, or tool execution. All database handles close before returning.
fn effective_policy(dir: &Path) -> EffectivePolicy {
    let storage = Arc::new(storage::Storage::open(dir).unwrap());
    let user = storage.list_users().unwrap()[0].id;
    let bot = storage.list_bots(user).unwrap()[0].id;
    let agent = AgentLoop::new(
        storage.clone(),
        ToolRegistry::new(),
        Providers::unconfigured(),
        LoopConfig::default(),
    );
    let _state = AppState::new(agent.clone(), storage, DaemonConfig::new("test-token")).unwrap();
    agent.effective_policy(bot).unwrap()
}

#[tokio::test]
async fn sec16_round_trip_keeps_global_deny_above_imported_grants() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("kaynak-Öcel-日本");
    let target = root.path().join("hedef-çalışma-🦀");
    fs::create_dir_all(&source).unwrap();
    database_with_grant(&source);
    save_policy(&source, &Policy::read_only());
    let before = effective_policy(&source);
    assert_eq!(
        before.grants.len(),
        1,
        "the fixture must replay a real grant"
    );
    assert_eq!(
        before.categories[&PermissionCategory::Shell].decision,
        Decision::Deny
    );

    let bytes = tests::bundle(&source).await;
    import(
        &target,
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    let after = effective_policy(&target);
    assert_eq!(
        after.grants.len(),
        1,
        "migration must preserve the grant record"
    );
    assert_eq!(
        after.categories[&PermissionCategory::Shell].decision,
        Decision::Deny,
        "SEC-16: migration must not activate a grant by losing global Deny"
    );
    assert!(!after.categories[&PermissionCategory::Shell].via_grant);
}

#[tokio::test]
async fn sec16_export_versions_the_default_without_changing_source_state() {
    let source = tempfile::tempdir().unwrap();
    tests::database(source.path(), false);
    let bytes = tests::bundle(source.path()).await;
    let entries = tests::unpack(&bytes);
    let payload = &entries
        .iter()
        .find(|(name, _)| name == "policy.json")
        .unwrap()
        .1;
    let document: serde_json::Value = serde_json::from_slice(payload).unwrap();
    assert_eq!(document["schema_version"], crate::POLICY_SCHEMA_VERSION);
    assert_eq!(
        daemon_api::validate_policy(std::str::from_utf8(payload).unwrap())
            .unwrap()
            .categories,
        Policy::approve_destructive().categories
    );
    assert!(!source.path().join("policy.json").exists());

    let target = tempfile::tempdir().unwrap();
    let result = import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.manifest.format_version, 2);
    assert!(
        result
            .manifest
            .files
            .iter()
            .any(|record| record.path == "policy.json")
    );
    assert_eq!(
        daemon_api::load_policy(target.path())
            .unwrap()
            .unwrap()
            .categories,
        Policy::approve_destructive().categories
    );
}

#[tokio::test]
async fn sec16_relaxation_needs_approval_before_grants_or_secrets_are_installed() {
    use secret_store::SecretStore;

    let source = tempfile::tempdir().unwrap();
    database_with_grant(source.path());
    let source_store = MemoryStore::new();
    source_store.set("incoming", "new").unwrap();
    let mut bytes = Vec::new();
    crate::export(
        source.path(),
        &source_store,
        crate::ExportOptions {
            secrets: crate::SecretSelection::All,
            ..Default::default()
        },
        &mut bytes,
    )
    .await
    .unwrap();

    let target = tempfile::tempdir().unwrap();
    database_with_grant(target.path());
    save_policy(target.path(), &Policy::read_only());
    let old_policy = fs::read(target.path().join("policy.json")).unwrap();
    let old_database = fs::read(target.path().join("bot.db")).unwrap();
    let store = MemoryStore::new();
    store.set("incoming", "old").unwrap();
    let result = import(
        target.path(),
        &store,
        bytes.as_slice(),
        ImportOptions {
            replace: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(crate::ImportError::PolicyRelaxation)));
    assert_eq!(
        fs::read(target.path().join("policy.json")).unwrap(),
        old_policy
    );
    assert_eq!(
        fs::read(target.path().join("bot.db")).unwrap(),
        old_database
    );
    assert_eq!(store.get("incoming").unwrap().unwrap().expose(), "old");
    assert!(!target.path().join("import-staging").exists());
    assert!(!fs::read_dir(target.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("backup-")
    }));

    let outcome = import(
        target.path(),
        &store,
        bytes.as_slice(),
        ImportOptions {
            replace: true,
            allow_policy_relaxation: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(outcome.backup_dir.unwrap().join("policy.json")).unwrap(),
        old_policy
    );
    assert_eq!(store.get("incoming").unwrap().unwrap().expose(), "new");
    let effective = effective_policy(target.path());
    assert_eq!(effective.grants.len(), 1);
    assert_eq!(
        effective.categories[&PermissionCategory::Shell].decision,
        Decision::Allow
    );
    assert!(effective.categories[&PermissionCategory::Shell].via_grant);
}

#[tokio::test]
async fn sec16_stricter_import_preserves_its_deny_and_backs_up_the_old_policy() {
    let source = tempfile::tempdir().unwrap();
    database_with_grant(source.path());
    save_policy(source.path(), &Policy::read_only());
    let bytes = tests::bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    database_with_grant(target.path());
    save_policy(target.path(), &Policy::approve_destructive());
    let outcome = import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions {
            replace: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        effective_policy(target.path()).categories[&PermissionCategory::Shell].decision,
        Decision::Deny
    );
    assert_eq!(
        daemon_api::load_policy(&outcome.backup_dir.unwrap())
            .unwrap()
            .unwrap()
            .categories,
        Policy::approve_destructive().categories
    );
}

#[tokio::test]
async fn sec16_invalid_local_policies_never_become_missing_policies() {
    let source = tempfile::tempdir().unwrap();
    tests::database(source.path(), false);
    let bytes = tests::bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    database_with_grant(target.path());
    let before = fs::read(target.path().join("bot.db")).unwrap();
    for malformed in [
        "{",
        "{}",
        r#"{"categories":{"shell":{"level":{"level":"unknown"}}}}"#,
        r#"{"categories":{"unknown":{"level":{"level":"deny"}}}}"#,
        r#"{"schema_version":2,"categories":{}}"#,
        r#"{"schema_version":null,"categories":{}}"#,
        r#"{"categories":{},"unknown_constraint":true}"#,
        r#"{"categories":{"shell":{"level":{"level":"ask"},"by_initator":{}}}}"#,
        r#"{"categories":{"shell":{"level":{"level":"allow","paths":["safe/**"]}}}}"#,
    ] {
        fs::write(target.path().join("policy.json"), malformed).unwrap();
        assert!(
            crate::export(
                target.path(),
                &MemoryStore::new(),
                crate::ExportOptions::default(),
                tokio::io::sink(),
            )
            .await
            .is_err(),
            "malformed source policy was exported: {malformed}"
        );
        assert!(
            import(
                target.path(),
                &MemoryStore::new(),
                bytes.as_slice(),
                ImportOptions {
                    replace: true,
                    allow_policy_relaxation: true,
                    ..Default::default()
                },
            )
            .await
            .is_err(),
            "malformed destination policy was ignored: {malformed}"
        );
        assert_eq!(
            fs::read_to_string(target.path().join("policy.json")).unwrap(),
            malformed
        );
        assert_eq!(fs::read(target.path().join("bot.db")).unwrap(), before);
        assert!(!target.path().join("import-staging").exists());
    }
}

#[tokio::test]
async fn sec16_missing_legacy_and_malformed_bundle_policies_fail_even_with_approval() {
    let source = tempfile::tempdir().unwrap();
    tests::database(source.path(), false);
    let bytes = tests::bundle(source.path()).await;
    let mut invalid_bundles = Vec::new();
    for malformed in [
        "{",
        "{}",
        r#"{"categories":{}}"#,
        r#"{"schema_version":0,"categories":{}}"#,
        r#"{"schema_version":2,"categories":{}}"#,
        r#"{"schema_version":"1","categories":{}}"#,
        r#"{"schema_version":null,"categories":{}}"#,
        r#"{"schema_version":1,"categories":{"shell":{"level":{"level":"unknown"}}}}"#,
        r#"{"schema_version":1,"categories":{"shell":{"level":{"level":"ask"},"by_initiator":{"unknown":{"level":"deny"}}}}}"#,
        r#"{"schema_version":1,"categories":{"shell":{"level":{"level":"allow_with_limits","path_patterns":[],"extra_limit":1}}}}"#,
    ] {
        // Recompute the valid manifest hash: rejection must validate policy
        // semantics, rather than merely noticing archive corruption.
        invalid_bundles.push(tests::replace_payload(
            &bytes,
            "policy.json",
            malformed.as_bytes().to_vec(),
        ));
    }
    for version in [1, crate::FORMAT_VERSION] {
        let mut entries = tests::unpack(&bytes);
        entries.retain(|(name, _)| name != "policy.json");
        let manifest_entry = entries.last_mut().unwrap();
        let mut manifest: crate::Manifest = serde_json::from_slice(&manifest_entry.1).unwrap();
        manifest.format_version = version;
        manifest.files.retain(|record| record.path != "policy.json");
        manifest_entry.1 = serde_json::to_vec(&manifest).unwrap();
        invalid_bundles.push(tests::pack(&entries));
    }
    invalid_bundles.push(tests::alter_manifest(&bytes, |manifest| {
        manifest.format_version = 1;
    }));

    for invalid in invalid_bundles {
        assert!(matches!(
            tests::rejected(
                &invalid,
                ImportOptions {
                    allow_policy_relaxation: true,
                    ..Default::default()
                }
            )
            .await,
            crate::ImportError::Invalid(_)
        ));
    }
}

#[tokio::test]
async fn sec16_rechecks_a_destination_tightened_at_the_swap_boundary() {
    let source = tempfile::tempdir().unwrap();
    tests::database(source.path(), false);
    let bytes = tests::bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    database_with_grant(target.path());
    save_policy(target.path(), &Policy::approve_destructive());
    let before = fs::read(target.path().join("bot.db")).unwrap();
    let target_path = target.path().to_path_buf();
    let progress: crate::ProgressCallback = Arc::new(move |progress| {
        if progress.phase == crate::Phase::Swap {
            save_policy(&target_path, &Policy::read_only());
        }
    });
    let result = import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions {
            replace: true,
            progress: Some(progress),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(result, Err(crate::ImportError::PolicyRelaxation)));
    assert_eq!(fs::read(target.path().join("bot.db")).unwrap(), before);
    assert_eq!(
        daemon_api::load_policy(target.path())
            .unwrap()
            .unwrap()
            .categories,
        Policy::read_only().categories
    );
    assert!(!target.path().join("import-staging").exists());
}

#[tokio::test]
async fn sec16_non_regular_and_oversized_policy_files_fail_closed() {
    let source = tempfile::tempdir().unwrap();
    tests::database(source.path(), false);
    let bytes = tests::bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(target.path().join("policy.json")).unwrap();
    assert!(
        import(
            target.path(),
            &MemoryStore::new(),
            bytes.as_slice(),
            ImportOptions::default()
        )
        .await
        .is_err()
    );
    assert!(target.path().join("policy.json").is_dir());
    assert!(!target.path().join("bot.db").exists());

    let oversized = vec![b' '; usize::try_from(crate::policy::MAX_POLICY_BYTES).unwrap() + 1];
    fs::write(source.path().join("policy.json"), &oversized).unwrap();
    assert!(matches!(
        crate::export(
            source.path(),
            &MemoryStore::new(),
            crate::ExportOptions::default(),
            tokio::io::sink()
        )
        .await,
        Err(crate::ImportError::SizeLimit)
    ));
    let invalid = tests::replace_payload(&bytes, "policy.json", oversized);
    assert!(matches!(
        tests::rejected(&invalid, ImportOptions::default()).await,
        crate::ImportError::SizeLimit
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn sec16_dangling_policy_symlink_is_not_a_missing_policy() {
    let source = tempfile::tempdir().unwrap();
    tests::database(source.path(), false);
    let bytes = tests::bundle(source.path()).await;
    std::os::unix::fs::symlink("absent", source.path().join("policy.json")).unwrap();
    assert!(
        crate::export(
            source.path(),
            &MemoryStore::new(),
            crate::ExportOptions::default(),
            tokio::io::sink()
        )
        .await
        .is_err()
    );
    assert!(
        import(
            source.path(),
            &MemoryStore::new(),
            bytes.as_slice(),
            ImportOptions::default()
        )
        .await
        .is_err()
    );
    assert!(
        fs::symlink_metadata(source.path().join("policy.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(windows)]
#[test]
fn sec16_windows_policy_and_database_roll_back_after_a_locked_staged_database() {
    use secret_store::SecretStore;
    use std::os::windows::fs::OpenOptionsExt;

    let target = tempfile::tempdir().unwrap();
    fs::write(target.path().join("bot.db"), b"old database").unwrap();
    save_policy(target.path(), &Policy::read_only());
    let old_policy = fs::read(target.path().join("policy.json")).unwrap();
    let staging = crate::install::Staging::create(target.path()).unwrap();
    fs::write(staging.0.join("bot.db"), b"new database").unwrap();
    save_policy(&staging.0, &Policy::approve_destructive());
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(staging.0.join("bot.db"))
        .unwrap();
    let store = MemoryStore::new();
    store.set("incoming", "old").unwrap();
    let secrets = [("incoming".into(), zeroize::Zeroizing::new("new".into()))].into();
    let result = crate::install::commit(target.path(), &staging.0, &store, &secrets);
    // Explicitly release the denied-sharing handle before any temp cleanup.
    drop(lock);
    assert!(matches!(result, Err(crate::ImportError::Io(_))));
    assert_eq!(
        fs::read(target.path().join("policy.json")).unwrap(),
        old_policy
    );
    assert_eq!(
        fs::read(target.path().join("bot.db")).unwrap(),
        b"old database"
    );
    assert!(staging.0.join("policy.json").is_file());
    assert_eq!(store.get("incoming").unwrap().unwrap().expose(), "old");
}
