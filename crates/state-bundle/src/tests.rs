use std::{
    fs,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use rusqlite::Connection;
use secret_store::{MemoryStore, SecretError, SecretValue};

use super::*;

pub(super) fn database(dir: &Path, populated: bool) {
    drop(storage::Storage::open(dir).unwrap());
    if populated {
        Connection::open(dir.join("bot.db")).unwrap().execute_batch(
            "INSERT INTO users VALUES ('00000000-0000-0000-0000-000000000001', 'owner', 1);
             INSERT INTO bots VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', 'assistant', '{\"model\":\"fake\"}', 2);
             INSERT INTO chats VALUES ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'hello', 3);"
        ).unwrap();
    }
}

pub(super) async fn bundle(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    export(dir, &MemoryStore::new(), ExportOptions::default(), &mut out)
        .await
        .unwrap();
    out
}

pub(super) fn unpack(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    tar::Archive::new(bytes)
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_str().unwrap().to_owned();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).unwrap();
            (path, contents)
        })
        .collect()
}

pub(super) fn pack(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap()
}

pub(super) fn alter_manifest(bytes: &[u8], change: impl FnOnce(&mut Manifest)) -> Vec<u8> {
    let mut entries = unpack(bytes);
    let last = entries.last_mut().unwrap();
    assert_eq!(last.0, "manifest.json");
    let mut manifest: Manifest = serde_json::from_slice(&last.1).unwrap();
    change(&mut manifest);
    last.1 = serde_json::to_vec(&manifest).unwrap();
    pack(&entries)
}

pub(super) fn replace_payload(bytes: &[u8], name: &str, payload: Vec<u8>) -> Vec<u8> {
    use sha2::{Digest, Sha256};

    let mut entries = unpack(bytes);
    let manifest_entry = entries.pop().unwrap();
    let mut manifest: Manifest = serde_json::from_slice(&manifest_entry.1).unwrap();
    let record = manifest.files.iter_mut().find(|f| f.path == name).unwrap();
    record.size = payload.len() as u64;
    record.sha256 = format!("{:x}", Sha256::digest(&payload));
    entries.iter_mut().find(|(path, _)| path == name).unwrap().1 = payload;
    entries.push((
        "manifest.json".into(),
        serde_json::to_vec(&manifest).unwrap(),
    ));
    pack(&entries)
}

// BUG-23: a valid archive checksum does not establish SQLite integrity.
#[tokio::test]
async fn corrupt_secondary_database_preserves_destination() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    let schedule = Connection::open(source.path().join("schedules.db")).unwrap();
    schedule
        .execute_batch("CREATE TABLE old_source(value TEXT)")
        .unwrap();
    drop(schedule);
    let bytes = replace_payload(
        &bundle(source.path()).await,
        "schedules.db",
        b"not a SQLite database, with a correct archive checksum".to_vec(),
    );
    let target = tempfile::tempdir().unwrap();
    database(target.path(), true);
    fs::write(
        target.path().join("schedules.db"),
        "existing schedule bytes",
    )
    .unwrap();
    fs::write(target.path().join("memory.db"), "existing memory bytes").unwrap();
    let originals: Vec<_> = ["bot.db", "schedules.db", "memory.db"]
        .into_iter()
        .map(|name| (name, fs::read(target.path().join(name)).unwrap()))
        .collect();
    let store = MemoryStore::new();
    store.set("existing", "untouched").unwrap();
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
    assert!(result.is_err(), "corrupt schedules.db was committed");
    assert!(result.unwrap_err().to_string().contains("schedules.db"));
    for (name, before) in originals {
        assert_eq!(
            fs::read(target.path().join(name)).unwrap(),
            before,
            "{name}"
        );
    }
    assert_eq!(
        store.get("existing").unwrap().unwrap().expose(),
        "untouched"
    );
    assert!(!target.path().join("import-staging").exists());
    assert!(!fs::read_dir(target.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("backup-")
    }));
}

pub(super) async fn rejected(bytes: &[u8], opts: ImportOptions) -> ImportError {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("api-token"), "identity").unwrap();
    let result = import(dir.path(), &MemoryStore::new(), bytes, opts)
        .await
        .unwrap_err();
    assert!(!dir.path().join("import-staging").exists());
    assert!(!dir.path().join("bot.db").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("api-token")).unwrap(),
        "identity"
    );
    result
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Integrated coverage of the full install layout.
async fn populated_round_trip_with_selected_secrets_and_progress() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), true);
    let live = Connection::open(source.path().join("bot.db")).unwrap();
    live.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO users VALUES ('00000000-0000-0000-0000-000000000004', 'wal-only', 4)").unwrap();
    let schedule = Connection::open(source.path().join("schedules.db")).unwrap();
    schedule
        .execute_batch(include_str!(
            "../../scheduler/migrations/0001_schedules.sql"
        ))
        .unwrap();
    schedule.execute_batch("PRAGMA user_version=1; PRAGMA journal_mode=WAL; INSERT INTO schedules VALUES ('s', 'b', NULL, 'daily', '{}', '{}', 1, 0, NULL, 123, 'skip')").unwrap();
    for (path, value) in [
        ("skills/demo/SKILL.md", "skill"),
        ("workflows/test.toml", "workflow"),
        ("bots/a.toml", "profile"),
        ("profiles/b.toml", "profile2"),
        ("providers.toml", "provider"),
        ("mcp.toml", "mcp"),
    ] {
        let path = source.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    fs::write(
        source.path().join("host.toml"),
        "mode='client'\nstate='active'\nepoch=7\nleader='old'\nmax_computer_runs=1",
    )
    .unwrap();
    let store = MemoryStore::new();
    store.set("selected", "test-value").unwrap();
    store.set("unselected", "other").unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let cb: ProgressCallback = Arc::new(move |event| capture.lock().unwrap().push(event));
    let mut bytes = Vec::new();
    let manifest = export(
        source.path(),
        &store,
        ExportOptions {
            secrets: SecretSelection::Names(vec!["selected".into()]),
            epoch: 7,
            source_key: Some("public-key".into()),
            progress: Some(cb.clone()),
            ..Default::default()
        },
        &mut bytes,
    )
    .await
    .unwrap();
    assert_eq!(unpack(&bytes).last().unwrap().0, "manifest.json");
    assert!(!source.path().join("secrets.json").exists());
    let target = tempfile::tempdir().unwrap();
    let target_store = MemoryStore::new();
    let outcome = import(
        target.path(),
        &target_store,
        bytes.as_slice(),
        ImportOptions {
            host_mode: "host".into(),
            progress: Some(cb),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome.manifest, manifest);
    assert_eq!(outcome.secrets_written, ["selected"]);
    assert_eq!(
        target_store.get("selected").unwrap().unwrap().expose(),
        "test-value"
    );
    assert!(target_store.get("unselected").unwrap().is_none());
    let original = storage::Storage::open(source.path()).unwrap();
    let imported = storage::Storage::open(target.path()).unwrap();
    assert_eq!(
        original.list_users().unwrap(),
        imported.list_users().unwrap()
    );
    let user = original.list_users().unwrap()[0].id;
    assert_eq!(
        original.list_bots(user).unwrap(),
        imported.list_bots(user).unwrap()
    );
    assert_eq!(
        original.list_chats(user).unwrap(),
        imported.list_chats(user).unwrap()
    );
    let conn = Connection::open(target.path().join("schedules.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT name, next_run_at_ms FROM schedules", [], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        ("daily".into(), 123)
    );
    assert_eq!(
        fs::read_to_string(target.path().join("skills/demo/SKILL.md")).unwrap(),
        "skill"
    );
    let host: toml::Table =
        toml::from_str(&fs::read_to_string(target.path().join("host.toml")).unwrap()).unwrap();
    assert_eq!(host["mode"].as_str(), Some("host"));
    assert_eq!(host["state"].as_str(), Some("standby"));
    assert!(!host.contains_key("leader"));
    assert!(!target.path().join("secrets.json").exists());
    let events = events.lock().unwrap();
    assert!(events.iter().any(|p| p.phase == Phase::Verify));
    assert!(events.iter().any(|p| p.phase == Phase::Swap));
    assert_eq!(events.last().unwrap().bytes, bytes.len() as u64);
}

#[tokio::test]
async fn exclusions_are_central_and_apply_at_every_depth() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    for name in EXCLUSIONS {
        let name = name.replace('*', "123");
        for prefix in ["", "skills/nested/"] {
            let path = dir.path().join(format!("{prefix}{name}"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "excluded").unwrap();
        }
    }
    for (path, _) in unpack(&bundle(dir.path()).await) {
        assert!(
            matches!(path.as_str(), "bot.db" | "policy.json" | "manifest.json"),
            "unexpected {path}"
        );
    }
}

#[tokio::test]
async fn skill_lock_and_publication_staging_files_are_not_bundled() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    for (path, value) in [
        ("skills/demo/SKILL.md", "skill"),
        ("skills/.praxi-skills.lock", ""),
        ("skills/.praxi-publish-abc123/SKILL.md", "half published"),
        ("skills/.praxi-publish-old-abc123/skill/SKILL.md", "backup"),
        ("skills/demo/.praxi-write-abc123", "half written"),
        ("bots/a/skills/.praxi-skills.lock", ""),
        ("bots/a/skills/x/.praxi-write-abc123", "half written"),
    ] {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    let paths: Vec<String> = unpack(&bundle(dir.path()).await)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert!(
        paths.contains(&"skills/demo/SKILL.md".to_owned()),
        "{paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains(".praxi-")),
        "{paths:?}"
    );
}

#[tokio::test]
async fn tampered_payload_fails_sha256() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let mut bytes = bundle(dir.path()).await;
    bytes[512 + 120] ^= 1;
    assert!(
        matches!(rejected(&bytes, ImportOptions::default()).await, ImportError::Invalid(ref s) if s.contains("sha256"))
    );
}

#[tokio::test]
async fn traversal_absolute_and_windows_paths_are_rejected() {
    for name in [
        "../x",
        "/x",
        "C:/x",
        "skills/../../x",
        "skills\\x",
        "skills/NUL.txt",
        "skills/a.",
    ] {
        let mut header = tar::Header::new_ustar();
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(0);
        header.set_cksum();
        let bytes = header.as_bytes().to_vec();
        assert!(
            matches!(
                rejected(&bytes, ImportOptions::default()).await,
                ImportError::Invalid(_)
            ),
            "{name}"
        );
    }
}

#[tokio::test]
async fn symlinks_hardlinks_and_special_entries_are_rejected() {
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Directory,
        tar::EntryType::Fifo,
        tar::EntryType::XHeader,
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_path("skills/x").unwrap();
        header.set_size(0);
        header.set_entry_type(kind);
        header.set_link_name("outside").unwrap();
        header.set_cksum();
        assert!(matches!(
            rejected(header.as_bytes(), ImportOptions::default()).await,
            ImportError::Invalid(_)
        ));
    }
}

#[tokio::test]
async fn secondary_future_schemas_name_the_database() {
    for db in ["memory.db", "schedules.db"] {
        let source = tempfile::tempdir().unwrap();
        database(source.path(), false);
        if db == "schedules.db" {
            drop(scheduler::ScheduleStore::open(source.path()).unwrap());
        } else {
            drop(
                memory_store::MemoryStore::open(
                    source.path(),
                    Arc::new(memory_store::HashEmbedder::new(8)),
                )
                .unwrap(),
            );
        }
        let conn = Connection::open(source.path().join(db)).unwrap();
        let supported: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        conn.execute_batch("CREATE TABLE future_table(value TEXT); PRAGMA user_version=999;")
            .unwrap();
        drop(conn);
        let error = rejected(&bundle(source.path()).await, ImportOptions::default()).await;
        assert!(
            matches!(error, ImportError::IncompatibleSchema { db: name, found: 999, supported: actual } if name == db && actual == supported)
        );
    }
}

#[tokio::test]
async fn older_secondary_schemas_migrate_and_are_self_contained() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    for db in ["schedules.db", "memory.db"] {
        let conn = Connection::open(source.path().join(db)).unwrap();
        conn.execute_batch("CREATE TABLE legacy_marker(value TEXT); PRAGMA journal_mode=WAL; INSERT INTO legacy_marker VALUES ('preserved');").unwrap();
    }
    let bytes = bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    for db in ["bot.db", "schedules.db", "memory.db"] {
        let conn = Connection::open(target.path().join(db)).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(version > 0, "{db}");
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "delete", "{db}");
        assert!(!target.path().join(format!("{db}-wal")).exists());
        assert!(!target.path().join(format!("{db}-shm")).exists());
        if db != "bot.db" {
            assert_eq!(
                conn.query_row("SELECT value FROM legacy_marker", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "preserved"
            );
        }
    }
    drop(scheduler::ScheduleStore::open(target.path()).unwrap());
    drop(
        memory_store::MemoryStore::open(
            target.path(),
            Arc::new(memory_store::HashEmbedder::new(8)),
        )
        .unwrap(),
    );
}

#[tokio::test]
async fn memory_vectors_import_without_an_embedder_or_dimension_change() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    drop(
        memory_store::MemoryStore::open(
            source.path(),
            Arc::new(memory_store::HashEmbedder::new(32)),
        )
        .unwrap(),
    );
    drop(scheduler::ScheduleStore::open(source.path()).unwrap());
    let bytes = bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    let imported = import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        imported
            .manifest
            .db_schemas
            .as_ref()
            .unwrap()
            .get("memory.db"),
        Some(&1)
    );
    let conn = Connection::open(target.path().join("memory.db")).unwrap();
    let dim: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key='embedding_dim'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dim, "32");
    drop(conn);
    drop(
        memory_store::MemoryStore::open(
            target.path(),
            Arc::new(memory_store::HashEmbedder::new(32)),
        )
        .unwrap(),
    );
}

#[tokio::test]
async fn database_manifest_mismatches_are_rejected() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    drop(scheduler::ScheduleStore::open(source.path()).unwrap());
    let bytes = bundle(source.path()).await;
    for change in [
        |m: &mut Manifest| {
            m.db_schemas
                .as_mut()
                .unwrap()
                .insert("schedules.db".into(), 0);
        },
        |m: &mut Manifest| {
            m.db_schemas.as_mut().unwrap().remove("schedules.db");
        },
        |m: &mut Manifest| {
            m.db_schemas
                .as_mut()
                .unwrap()
                .insert("unknown.db".into(), 1);
        },
    ] {
        assert!(matches!(
            rejected(&alter_manifest(&bytes, change), ImportOptions::default()).await,
            ImportError::InvalidDatabase { .. }
        ));
    }
}

#[tokio::test]
async fn manifest_without_optional_database_schemas_imports() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    drop(scheduler::ScheduleStore::open(source.path()).unwrap());
    let bytes = alter_manifest(&bundle(source.path()).await, |m| m.db_schemas = None);
    let target = tempfile::tempdir().unwrap();
    let imported = import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    assert!(imported.manifest.db_schemas.is_none());
    drop(scheduler::ScheduleStore::open(target.path()).unwrap());
}

#[tokio::test]
async fn secondary_schema_version_without_tables_is_rejected() {
    for db in ["memory.db", "schedules.db"] {
        let source = tempfile::tempdir().unwrap();
        database(source.path(), false);
        let conn = Connection::open(source.path().join(db)).unwrap();
        conn.execute_batch("PRAGMA user_version=1;").unwrap();
        drop(conn);
        assert!(
            matches!(rejected(&bundle(source.path()).await, ImportOptions::default()).await, ImportError::InvalidDatabase { db: name, .. } if name == db)
        );
    }
}

#[tokio::test]
async fn caps_apply_to_stream_entries_and_trailing_padding() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let bytes = bundle(dir.path()).await;
    for opts in [
        ImportOptions {
            max_bytes: 600,
            ..Default::default()
        },
        ImportOptions {
            max_entry_bytes: 10,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            rejected(&bytes, opts).await,
            ImportError::SizeLimit
        ));
    }
    let mut extended = bytes.clone();
    extended.extend_from_slice(&[0; 2048]);
    assert!(matches!(
        rejected(
            &extended,
            ImportOptions {
                max_bytes: bytes.len() as u64,
                ..Default::default()
            }
        )
        .await,
        ImportError::SizeLimit
    ));
}

#[tokio::test]
async fn replacement_requires_opt_in_and_retains_three_backups() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), true);
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    let bytes = bundle(source.path()).await;
    assert!(matches!(
        import(
            dir.path(),
            &MemoryStore::new(),
            bytes.as_slice(),
            ImportOptions::default()
        )
        .await,
        Err(ImportError::TargetNotEmpty)
    ));
    fs::write(dir.path().join("identity.key"), "local-key").unwrap();
    fs::write(dir.path().join("secrets.enc"), "local-store").unwrap();
    for _ in 0..5 {
        let outcome = import(
            dir.path(),
            &MemoryStore::new(),
            bytes.as_slice(),
            ImportOptions {
                replace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(outcome.backup_dir.unwrap().join("bot.db").exists());
    }
    let count = fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("backup-")
        })
        .count();
    assert_eq!(count, 3);
    assert_eq!(
        fs::read_to_string(dir.path().join("identity.key")).unwrap(),
        "local-key"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("secrets.enc")).unwrap(),
        "local-store"
    );
}

#[tokio::test]
async fn truncated_import_preserves_target_and_secrets() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), true);
    let before = fs::read(dir.path().join("bot.db")).unwrap();
    let bytes = bundle(dir.path()).await;
    let store = MemoryStore::new();
    store.set("existing", "untouched").unwrap();
    assert!(
        import(
            dir.path(),
            &store,
            &bytes[..bytes.len() / 2],
            ImportOptions {
                replace: true,
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(fs::read(dir.path().join("bot.db")).unwrap(), before);
    assert_eq!(
        store.get("existing").unwrap().unwrap().expose(),
        "untouched"
    );
    assert!(!dir.path().join("import-staging").exists());
}

#[tokio::test]
async fn older_schema_migrates_forward() {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("bot.db")).unwrap();
    conn.execute_batch(include_str!("../../storage/migrations/0001_init.sql"))
        .unwrap();
    conn.execute_batch("PRAGMA user_version=1").unwrap();
    drop(conn);
    let bytes = bundle(dir.path()).await;
    let target = tempfile::tempdir().unwrap();
    let outcome = import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.manifest.schema_version, 1);
    let conn = Connection::open(target.path().join("bot.db")).unwrap();
    assert_eq!(
        storage::migrate::current_version(&conn).unwrap(),
        storage::migrate::latest_version()
    );
    conn.prepare("SELECT spec_json FROM runs").unwrap();
}

#[tokio::test]
async fn future_versions_schema_disagreement_and_missing_records_rejected() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let bytes = bundle(dir.path()).await;
    for change in [
        |m: &mut Manifest| m.format_version = FORMAT_VERSION + 1,
        |m: &mut Manifest| m.schema_version = 999,
        |m: &mut Manifest| m.schema_version = 1,
        |m: &mut Manifest| m.files.clear(),
        |m: &mut Manifest| m.secret_names.push("absent".into()),
    ] {
        assert!(matches!(
            rejected(&alter_manifest(&bytes, change), ImportOptions::default()).await,
            ImportError::Invalid(_)
                | ImportError::InvalidDatabase { .. }
                | ImportError::IncompatibleSchema { .. }
        ));
    }
}

#[tokio::test]
async fn duplicates_unlisted_files_and_entries_after_manifest_rejected() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let bytes = bundle(dir.path()).await;
    let entries = unpack(&bytes);
    let mut duplicate = entries.clone();
    duplicate.insert(0, entries[0].clone());
    let mut unlisted = entries.clone();
    unlisted.insert(0, ("api-token".into(), b"bad".to_vec()));
    let mut after = entries;
    after.push(("skills/after".into(), vec![]));
    for malformed in [duplicate, unlisted, after] {
        assert!(matches!(
            rejected(&pack(&malformed), ImportOptions::default()).await,
            ImportError::Invalid(_)
        ));
    }
}

#[tokio::test]
async fn sqlite_sidecar_entries_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let entries = unpack(&bundle(dir.path()).await);
    for sidecar in [
        "bot.db-wal",
        "bot.db-shm",
        "bot.db-journal",
        "schedules.db-wal",
        "memory.db-shm",
    ] {
        let mut malformed = entries.clone();
        malformed.insert(0, (sidecar.into(), b"crafted".to_vec()));
        let error = rejected(&pack(&malformed), ImportOptions::default()).await;
        assert!(
            matches!(&error, ImportError::Invalid(msg) if msg.contains("allowlist")),
            "{sidecar}: {error:?}"
        );
    }
}

#[tokio::test]
async fn all_does_not_carry_mailbox_passwords_unless_named() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let store = MemoryStore::new();
    store.set("provider/key", "k").unwrap();
    store.set("email/work/password", "mailbox-pw").unwrap();
    store.set("email/a.b/password", "mailbox-pw-2").unwrap();
    // Not a managed mailbox secret: still exported by `All`.
    store.set("email/notes", "n").unwrap();
    let mut bytes = Vec::new();
    let manifest = export(
        dir.path(),
        &store,
        ExportOptions {
            secrets: SecretSelection::All,
            ..Default::default()
        },
        &mut bytes,
    )
    .await
    .unwrap();
    assert_eq!(manifest.secret_names, ["email/notes", "provider/key"]);
    assert!(
        !bytes.windows(10).any(|w| w == b"mailbox-pw"),
        "a mailbox password is in the bundle"
    );
    // Naming it explicitly is the user's choice and still works.
    let mut bytes = Vec::new();
    let manifest = export(
        dir.path(),
        &store,
        ExportOptions {
            secrets: SecretSelection::Names(vec!["email/work/password".into()]),
            ..Default::default()
        },
        &mut bytes,
    )
    .await
    .unwrap();
    assert_eq!(manifest.secret_names, ["email/work/password"]);
}

#[tokio::test]
async fn all_none_and_missing_secret_selection() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let store = MemoryStore::new();
    store.set("a", "1").unwrap();
    store.set("b", "2").unwrap();
    for (selection, expected) in [
        (SecretSelection::None, vec![]),
        (SecretSelection::All, vec!["a", "b"]),
        (
            SecretSelection::Names(vec!["a".into(), "a".into()]),
            vec!["a"],
        ),
    ] {
        let mut bytes = Vec::new();
        let manifest = export(
            dir.path(),
            &store,
            ExportOptions {
                secrets: selection,
                ..Default::default()
            },
            &mut bytes,
        )
        .await
        .unwrap();
        assert_eq!(manifest.secret_names, expected);
    }
    assert!(
        export(
            dir.path(),
            &store,
            ExportOptions {
                secrets: SecretSelection::Names(vec!["missing".into()]),
                ..Default::default()
            },
            tokio::io::sink()
        )
        .await
        .is_err()
    );
}

struct FailOnce {
    inner: MemoryStore,
    fail: AtomicBool,
}
impl SecretStore for FailOnce {
    fn get(&self, n: &str) -> std::result::Result<Option<SecretValue>, SecretError> {
        self.inner.get(n)
    }
    fn list_names(&self) -> std::result::Result<Vec<String>, SecretError> {
        self.inner.list_names()
    }
    fn delete(&self, n: &str) -> std::result::Result<bool, SecretError> {
        self.inner.delete(n)
    }
    fn set(&self, n: &str, v: &str) -> std::result::Result<(), SecretError> {
        self.inner.set(n, v)?;
        if n == "b" && self.fail.swap(false, Ordering::SeqCst) {
            return Err(SecretError::Unavailable("injected".into()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn secret_write_failure_rolls_back_previous_values_and_files() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), true);
    let source_store = MemoryStore::new();
    source_store.set("a", "new").unwrap();
    source_store.set("b", "new").unwrap();
    let mut bytes = Vec::new();
    export(
        dir.path(),
        &source_store,
        ExportOptions {
            secrets: SecretSelection::All,
            ..Default::default()
        },
        &mut bytes,
    )
    .await
    .unwrap();
    let target = tempfile::tempdir().unwrap();
    database(target.path(), true);
    let before = fs::read(target.path().join("bot.db")).unwrap();
    let store = FailOnce {
        inner: MemoryStore::new(),
        fail: AtomicBool::new(true),
    };
    store.inner.set("a", "old").unwrap();
    assert!(matches!(
        import(
            target.path(),
            &store,
            bytes.as_slice(),
            ImportOptions {
                replace: true,
                ..Default::default()
            }
        )
        .await,
        Err(ImportError::Secret(_))
    ));
    assert_eq!(store.get("a").unwrap().unwrap().expose(), "old");
    assert!(store.get("b").unwrap().is_none());
    assert_eq!(fs::read(target.path().join("bot.db")).unwrap(), before);
    assert!(!target.path().join("import-staging").exists());
}

#[tokio::test]
async fn checksum_and_manifest_absence_rejected() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let bytes = bundle(dir.path()).await;
    let mut corrupted = bytes.clone();
    corrupted[0] ^= 1;
    assert!(matches!(
        rejected(&corrupted, ImportOptions::default()).await,
        ImportError::Invalid(_)
    ));
    let mut entries = unpack(&bytes);
    entries.pop();
    assert!(matches!(
        rejected(&pack(&entries), ImportOptions::default()).await,
        ImportError::Invalid(_)
    ));
}

#[test]
fn staging_is_private_and_never_reuses_an_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let staging = install::Staging::create(dir.path()).unwrap();
    assert!(install::Staging::create(dir.path()).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&staging.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    drop(staging);
    assert!(!dir.path().join("import-staging").exists());
}

#[test]
fn chats_alone_require_replace() {
    let target = tempfile::tempdir().unwrap();
    database(target.path(), true);
    Connection::open(target.path().join("bot.db"))
        .unwrap()
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM bots;")
        .unwrap();
    assert!(matches!(
        install::check_target(target.path(), false),
        Err(ImportError::TargetNotEmpty)
    ));
    install::check_target(target.path(), true).unwrap();
}

#[tokio::test]
async fn invalid_host_file_leaves_target_untouched() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    fs::write(dir.path().join("host.toml"), "[").unwrap();
    let bytes = bundle(dir.path()).await;
    assert!(matches!(
        rejected(&bytes, ImportOptions::default()).await,
        ImportError::Invalid(_)
    ));
}

#[tokio::test]
async fn streaming_backpressure_and_cancelled_receive_cleanup() {
    use tokio::io::AsyncReadExt;
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    fs::create_dir(source.path().join("skills")).unwrap();
    fs::write(source.path().join("skills/large"), vec![42; 256 * 1024]).unwrap();
    let (sink, mut reader) = tokio::io::duplex(1024);
    let store = MemoryStore::new();
    let writer = export(source.path(), &store, ExportOptions::default(), sink);
    let consumer = async {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        bytes
    };
    let (manifest, bytes) = tokio::join!(writer, consumer);
    assert!(manifest.unwrap().files.iter().any(|f| f.size == 256 * 1024));

    let target = tempfile::tempdir().unwrap();
    database(target.path(), true);
    let before = fs::read(target.path().join("bot.db")).unwrap();
    let (mut feeder, source) = tokio::io::duplex(4096);
    let target_store = MemoryStore::new();
    let target_path = target.path().to_path_buf();
    let receive = tokio::spawn(async move {
        import(
            target_path,
            &target_store,
            source,
            ImportOptions {
                replace: true,
                ..Default::default()
            },
        )
        .await
    });
    // A complete database entry and a partial skill entry, then block forever.
    let skill_header = unpack(&bytes)
        .iter()
        .take_while(|(name, _)| name != "skills/large")
        .map(|(_, payload)| 512 + payload.len().div_ceil(512) * 512)
        .sum::<usize>();
    feeder
        .write_all(&bytes[..skill_header + 1024])
        .await
        .unwrap();
    // Waiting for input to drain ensures the receive task has started staging.
    while !target.path().join("import-staging/skills/large").exists() {
        tokio::task::yield_now().await;
    }
    receive.abort();
    assert!(receive.await.unwrap_err().is_cancelled());
    assert!(!target.path().join("import-staging").exists());
    assert_eq!(fs::read(target.path().join("bot.db")).unwrap(), before);
}

#[tokio::test]
async fn separate_memory_database_includes_committed_wal_data() {
    let source = tempfile::tempdir().unwrap();
    database(source.path(), false);
    let conn = Connection::open(source.path().join("memory.db")).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE facts (value TEXT); INSERT INTO facts VALUES ('remember me');").unwrap();
    let bytes = bundle(source.path()).await;
    let target = tempfile::tempdir().unwrap();
    import(
        target.path(),
        &MemoryStore::new(),
        bytes.as_slice(),
        ImportOptions::default(),
    )
    .await
    .unwrap();
    let imported = Connection::open(target.path().join("memory.db")).unwrap();
    assert_eq!(
        imported
            .query_row("SELECT value FROM facts", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "remember me"
    );
}

#[cfg(windows)]
#[test]
fn swap_failure_restores_renamed_roots_and_secrets() {
    use std::os::windows::fs::OpenOptionsExt;
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(target.path().join("bots")).unwrap();
    fs::write(target.path().join("bots/original"), "old profile").unwrap();
    fs::write(target.path().join("bot.db"), "locked database").unwrap();
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(target.path().join("bot.db"))
        .unwrap();
    let staging = install::Staging::create(target.path()).unwrap();
    let store = MemoryStore::new();
    store.set("a", "old").unwrap();
    let secrets = BTreeMap::from([("a".into(), Zeroizing::new("new".into()))]);
    assert!(matches!(
        install::commit(
            target.path(),
            &staging.0,
            &store,
            &secrets,
            &permission_engine::Policy::approve_destructive(),
            true,
        ),
        Err(ImportError::Io(_))
    ));
    assert_eq!(
        fs::read_to_string(target.path().join("bots/original")).unwrap(),
        "old profile"
    );
    assert_eq!(store.get("a").unwrap().unwrap().expose(), "old");
}

#[cfg(unix)]
#[test]
fn swap_failure_restores_renamed_roots_and_secrets() {
    use std::os::unix::fs::symlink;
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(target.path().join("bots")).unwrap();
    fs::write(target.path().join("bots/original"), "old profile").unwrap();
    let staging = install::Staging::create(target.path()).unwrap();
    symlink("missing", staging.0.join("skills")).unwrap();
    let store = MemoryStore::new();
    store.set("a", "old").unwrap();
    let secrets = BTreeMap::from([("a".into(), Zeroizing::new("new".into()))]);
    assert!(
        install::commit(
            target.path(),
            &staging.0,
            &store,
            &secrets,
            &permission_engine::Policy::approve_destructive(),
            true,
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(target.path().join("bots/original")).unwrap(),
        "old profile"
    );
    assert_eq!(store.get("a").unwrap().unwrap().expose(), "old");
}

#[tokio::test]
async fn all_does_not_carry_agentmail_api_keys_unless_named() {
    let dir = tempfile::tempdir().unwrap();
    database(dir.path(), false);
    let store = MemoryStore::new();
    store.set("provider/key", "k").unwrap();
    store.set("email/bot-mail/api_key", "am_key_in_store").unwrap();
    // Not a managed account secret (extra path segment): still exported.
    store.set("email/a/b/api_key", "x").unwrap();
    let mut bytes = Vec::new();
    let manifest = export(
        dir.path(),
        &store,
        ExportOptions {
            secrets: SecretSelection::All,
            ..Default::default()
        },
        &mut bytes,
    )
    .await
    .unwrap();
    assert_eq!(manifest.secret_names, ["email/a/b/api_key", "provider/key"]);
    assert!(
        !bytes.windows(15).any(|w| w == b"am_key_in_store"),
        "an AgentMail key is in the bundle"
    );
}
