//! ARCH-02: tools resolve a caller, writes compare a version, and sharing is explicit.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};

use serde_json::{Value, json};
use tempfile::TempDir;
use tool_registry::{ContentPart, Tool, ToolContext, ToolOutput};

use crate::hub::{HubPaths, InstallOptions, InstallSource, SkillHub};
use crate::{SKILL_FILE, SkillContext, SkillIndex, SkillPaths, SkillRoots, Source, tools};

fn content(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: Description for {name}\n---\n{body}\n")
}

fn put(root: &Path, name: &str, body: &str) {
    fs::create_dir_all(root.join(name)).unwrap();
    fs::write(root.join(name).join(SKILL_FILE), content(name, body)).unwrap();
}

fn context(bot: Option<&str>) -> ToolContext {
    ToolContext {
        bot_id: bot.map(str::to_owned),
        ..ToolContext::default()
    }
}

fn tool(index: &SkillIndex, name: &str) -> Arc<dyn Tool> {
    tools(index)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

fn data(output: &ToolOutput) -> &Value {
    output
        .content
        .iter()
        .find_map(|part| match part {
            ContentPart::Json { data } => Some(data),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn precedence_and_scope_labels_are_per_caller() {
    let tmp = TempDir::new().unwrap();
    let bundled = tmp.path().join("shipped");
    let shared = tmp.path().join("skills");
    put(&bundled, "x", "shipped x");
    put(&bundled, "bundled-only", "shipped only");
    put(&shared, "x", "shared x");
    let original = fs::read(shared.join("x/SKILL.md")).unwrap();
    let index = SkillIndex::load_roots(SkillRoots {
        bundled: Some(bundled.clone()),
        ..SkillRoots::from_data_dir(tmp.path())
    });
    tool(&index, "skill_write")
        .call(
            &context(Some("a")),
            json!({"name":"x","content":content("x", "bot x")}),
        )
        .await
        .unwrap();
    for (id, body, scope) in [
        (Some("a"), "bot x", "bot"),
        (Some("b"), "shared x", "shared"),
        (None, "shared x", "shared"),
    ] {
        let view = index.for_bot(id).unwrap();
        assert_eq!(view.read("x", None).unwrap(), body);
        assert_eq!(view.get("bundled-only").unwrap().source, Source::Bundled);
        let list = tool(&index, "skill_list")
            .call(&context(id), json!({}))
            .await
            .unwrap();
        let row = data(&list)
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "x")
            .unwrap();
        assert_eq!(row["scope"], scope);
    }
    assert_eq!(fs::read(shared.join("x/SKILL.md")).unwrap(), original);
    assert_eq!(
        fs::read_to_string(bundled.join("x/SKILL.md")).unwrap(),
        content("x", "shipped x")
    );
    assert!(
        !bundled.join(".praxi-skills.lock").exists(),
        "bundled root is read-only"
    );
}

#[test]
fn independently_loaded_concurrent_writers_compare_under_the_file_lock() {
    let tmp = TempDir::new().unwrap();
    let paths = SkillPaths::from_data_dir(tmp.path(), Some("a"));
    SkillIndex::load(paths.clone())
        .write("x", &content("x", "original"))
        .unwrap();
    let barrier = Barrier::new(2);
    let barrier = &barrier;
    let result = std::thread::scope(|scope| {
        let a = SkillIndex::load(paths.clone());
        let b = SkillIndex::load(paths.clone());
        let first = scope.spawn(move || {
            barrier.wait();
            a.write("x", &content("x", "first"))
        });
        let second = scope.spawn(move || {
            barrier.wait();
            b.write("x", &content("x", "second"))
        });
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(result.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        result
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .contains("version conflict")
    );
    assert_eq!(SkillIndex::load(paths).get("x").unwrap().version, "0.1.1");
}

#[tokio::test]
async fn tool_read_returns_the_version_required_for_an_update() {
    let tmp = TempDir::new().unwrap();
    put(&tmp.path().join("skills"), "x", "shared");
    let index = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let ctx = context(Some("a"));
    let read = tool(&index, "skill_read");
    let write = tool(&index, "skill_write");
    assert!(
        data(&read.call(&ctx, json!({"name":"x"})).await.unwrap())["expected_version"].is_null()
    );
    write
        .call(&ctx, json!({"name":"x","content":content("x", "first")}))
        .await
        .unwrap();
    let first_read = read.call(&ctx, json!({"name":"x"})).await.unwrap();
    let version = data(&first_read)["expected_version"].as_str().unwrap();
    assert_eq!(version, "0.1.0");
    assert!(
        write
            .call(
                &ctx,
                json!({"name":"x","content":content("x", "unconditional")})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("version conflict")
    );
    write
        .call(
            &ctx,
            json!({"name":"x","content":content("x", "updated"),"expected_version":version}),
        )
        .await
        .unwrap();
    assert!(
        write
            .call(
                &ctx,
                json!({"name":"x","content":content("x", "stale"),"expected_version":version})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("version conflict")
    );
    assert_eq!(
        data(&read.call(&ctx, json!({"name":"x"})).await.unwrap())["version"],
        "0.1.1"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("skills/x/SKILL.md")).unwrap(),
        content("x", "shared")
    );
}

#[test]
fn a_missing_file_does_not_match_a_previously_observed_version() {
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    let saved = index.write("x", &content("x", "original")).unwrap();
    fs::remove_file(saved).unwrap();
    assert!(
        index
            .write("x", &content("x", "stale recreate"))
            .unwrap_err()
            .contains("version conflict")
    );
}

#[test]
fn unsafe_names_versions_and_exhaustion_leave_current_bytes_untouched() {
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    for name in ["con", "nul.txt", "lpt1", "com9.md"] {
        assert!(index.write(name, &content(name, "body")).is_err());
    }
    for version in ["../outside", "C:stream", "with space", "a@b", ".", "1."] {
        let text = format!("---\nname: x\ndescription: x\nversion: {version}\n---\nbody\n");
        assert!(index.write("x", &text).is_err(), "{version}");
    }
    let text = "---\nname: x\ndescription: x\nversion: 18446744073709551615\n---\noriginal\n";
    let path = index.write("x", text).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(
        index
            .write("x", &content("x", "new"))
            .unwrap_err()
            .contains("exhausted")
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn publish_copies_extra_files_and_refuses_a_different_shared_skill_without_replace() {
    let tmp = TempDir::new().unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let a = root.for_bot(Some("a")).unwrap();
    let b = root.for_bot(Some("b")).unwrap();
    let a_path = a.write("x", &content("x", "first")).unwrap();
    let extra = a_path.parent().unwrap().join("refs");
    fs::create_dir(&extra).unwrap();
    fs::write(extra.join("api.md"), "reference content").unwrap();
    let first = a.publish("x", false).unwrap();
    assert_eq!(first, tmp.path().join("skills/x@0.1.0"));
    assert_eq!(
        root.for_bot(None)
            .unwrap()
            .read("x", Some("refs/api.md"))
            .unwrap(),
        "reference content"
    );
    b.write("x", &content("x", "second")).unwrap();
    assert!(b.publish("x", false).unwrap_err().contains("replace: true"));
    assert_eq!(
        root.for_bot(None).unwrap().read("x", None).unwrap(),
        "first"
    );
    b.publish("x", true).unwrap();
    assert_eq!(
        root.for_bot(None).unwrap().read("x", None).unwrap(),
        "second"
    );
    assert_eq!(
        a.read("x", None).unwrap(),
        "first",
        "publication must not modify another bot"
    );
    b.write("x", &content("x", "third")).unwrap();
    assert!(b.publish("x", false).is_err());
    let newer = b.publish("x", true).unwrap();
    assert_eq!(newer, tmp.path().join("skills/x@0.1.1"));
    assert!(!first.exists());
    assert_eq!(
        root.for_bot(None).unwrap().read("x", None).unwrap(),
        "third"
    );
}

#[test]
fn shared_hub_install_keeps_acceptance_and_provenance_under_bot_shadow_and_publish() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    put(&source, "from-hub", "installed instructions");
    let data_dir = tmp.path().join("data");
    let hub = SkillHub::new(HubPaths::from_data_dir(&data_dir));
    let preview = hub
        .preview(&InstallSource::Local(source.join("from-hub")))
        .unwrap();
    assert!(matches!(
        hub.install(&preview, &InstallOptions::default()),
        Err(crate::hub::HubError::NotAccepted { .. })
    ));
    hub.install(
        &preview,
        &InstallOptions {
            accept: Some(preview.report.fingerprint.clone()),
            ..InstallOptions::default()
        },
    )
    .unwrap();
    let lock_before = fs::read(&hub.paths().lock_file).unwrap();
    let installed_path = data_dir.join("skills/from-hub/SKILL.md");
    let bytes_before = fs::read(&installed_path).unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(&data_dir));
    for id in ["a", "b"] {
        let skill = root.for_bot(Some(id)).unwrap().get("from-hub").unwrap();
        assert_eq!(skill.source, Source::User);
        assert_eq!(skill.body, "installed instructions");
    }
    let a = root.for_bot(Some("a")).unwrap();
    a.write("from-hub", &content("from-hub", "learned instructions"))
        .unwrap();
    assert!(a.publish("from-hub", false).is_err());
    a.publish("from-hub", true).unwrap();
    assert_eq!(fs::read(installed_path).unwrap(), bytes_before);
    assert_eq!(fs::read(&hub.paths().lock_file).unwrap(), lock_before);
    let installed = hub.list().unwrap();
    assert_eq!(installed.len(), 1);
    assert!(installed[0].present);
    assert!(!installed[0].modified);
    assert_eq!(
        root.for_bot(Some("b"))
            .unwrap()
            .read("from-hub", None)
            .unwrap(),
        "learned instructions"
    );
}

#[test]
fn prompt_resolver_is_per_bot_and_botless_prompt_contains_no_private_suggestions() {
    let tmp = TempDir::new().unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    root.for_bot(Some("a"))
        .unwrap()
        .write("only-a", &content("only-a", "private"))
        .unwrap();
    let hints = vec!["only-a".to_owned()];
    let ctx = SkillContext::default();
    assert!(
        root.prompt_block_for_bot(Some("a"), &ctx, &hints)
            .unwrap()
            .contains("[suggested] only-a")
    );
    for id in [None, Some("b")] {
        assert!(
            !root
                .prompt_block_for_bot(id, &ctx, &hints)
                .unwrap()
                .contains("only-a")
        );
    }
    assert!(!root.prompt_block(&ctx).contains("only-a"));
}

fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_dir(target, link);
    #[cfg(not(any(unix, windows)))]
    let result: std::io::Result<()> = Err(std::io::ErrorKind::Unsupported.into());
    match result {
        Ok(()) => true,
        // ERROR_PRIVILEGE_NOT_HELD (1314) is not always mapped to PermissionDenied.
        Err(error)
            if cfg!(windows)
                && (error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314)) =>
        {
            // A junction needs no privilege and is also a reparse point.
            let junction = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|out| out.status.success());
            if !junction {
                eprintln!(
                    "Skipping directory link assertion: neither symlink nor junction could be created ({error})"
                );
            }
            junction
        }
        Err(error) => panic!("create test symlink: {error}"),
    }
}

#[test]
fn bot_directory_links_cannot_cross_scope_boundaries() {
    let tmp = TempDir::new().unwrap();
    let roots = SkillRoots::from_data_dir(tmp.path());
    let root = SkillIndex::load_roots(roots.clone());
    root.for_bot(Some("b"))
        .unwrap()
        .write("private", &content("private", "B only"))
        .unwrap();
    let stale_a = root.for_bot(Some("a")).unwrap();
    if !link_dir(&tmp.path().join("bots/b"), &tmp.path().join("bots/a")) {
        return;
    }
    assert!(root.for_bot(Some("a")).unwrap_err().contains("reparse"));
    assert!(
        stale_a
            .write("private", &content("private", "stolen"))
            .is_err()
    );
    assert!(stale_a.publish("private", true).is_err());

    assert_eq!(
        root.for_bot(Some("b"))
            .unwrap()
            .read("private", None)
            .unwrap(),
        "B only"
    );
}

#[test]
fn linked_skill_directories_cannot_write_or_publish_another_scope() {
    let tmp = TempDir::new().unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let b = root.for_bot(Some("b")).unwrap();
    let b_path = b.write("x", &content("x", "B only")).unwrap();
    let a_dir = tmp.path().join("bots/a/skills");
    fs::create_dir_all(&a_dir).unwrap();
    if !link_dir(b_path.parent().unwrap(), &a_dir.join("x")) {
        return;
    }
    let a = root.for_bot(Some("a")).unwrap();
    assert!(a.get("x").is_none());
    assert!(a.write("x", &content("x", "stolen")).is_err());
    assert!(a.publish("x", true).is_err());
    assert_eq!(
        fs::read_to_string(b_path).unwrap().lines().last().unwrap(),
        "B only"
    );
}

#[cfg(windows)]
#[test]
fn windows_failed_atomic_replace_preserves_current_file_and_releases_lock() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = TempDir::new().unwrap();
    let index = SkillIndex::load(SkillPaths::from_data_dir(tmp.path(), Some("a")));
    let path = index.write("x", &content("x", "original")).unwrap();
    let before = fs::read(&path).unwrap();
    // Allow readers but deny delete sharing, making replacement deterministically fail.
    let pinned = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    assert!(index.write("x", &content("x", "blocked")).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    drop(pinned);
    index.write("x", &content("x", "after release")).unwrap();
    assert_eq!(index.get("x").unwrap().version, "0.1.1");
    assert_eq!(index.read("x", None).unwrap(), "after release");
}

#[test]
fn publication_overrides_legacy_directories_named_unlike_their_skill() {
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("skills");
    fs::create_dir_all(shared.join("zzz@legacy")).unwrap();
    fs::write(
        shared.join("zzz@legacy/SKILL.md"),
        content("x", "legacy body"),
    )
    .unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let bot = root.for_bot(Some("a")).unwrap();
    bot.write("x", &content("x", "published body")).unwrap();
    assert!(bot.publish("x", false).is_err());
    bot.publish("x", true).unwrap();
    assert_eq!(
        root.for_bot(None).unwrap().read("x", None).unwrap(),
        "published body"
    );
    assert!(shared.join("zzz@legacy/SKILL.md").exists());
}

#[test]
fn publishing_a_bundled_skill_name_requires_replace_and_says_every_bot_is_affected() {
    let tmp = TempDir::new().unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let a = root.for_bot(Some("a")).unwrap();
    a.write("custom-dropdowns", &content("custom-dropdowns", "mine"))
        .unwrap();
    let error = a.publish("custom-dropdowns", false).unwrap_err();
    assert!(error.contains("built-in"), "{error}");
    assert!(error.contains("all bots"), "{error}");
    assert!(error.contains("replace"), "{error}");
    assert_ne!(
        root.for_bot(Some("b"))
            .unwrap()
            .read("custom-dropdowns", None)
            .unwrap(),
        "mine"
    );
    assert!(
        !tmp.path()
            .join("skills")
            .join("custom-dropdowns@0.1.0")
            .exists()
    );
    a.publish("custom-dropdowns", true).unwrap();
    assert_eq!(
        root.for_bot(Some("b"))
            .unwrap()
            .read("custom-dropdowns", None)
            .unwrap(),
        "mine"
    );
}

#[test]
fn publishing_an_on_disk_bundled_name_also_requires_replace() {
    let tmp = TempDir::new().unwrap();
    let shipped = tmp.path().join("shipped");
    put(&shipped, "shipped-only", "shipped");
    let root = SkillIndex::load_roots(SkillRoots {
        bundled: Some(shipped),
        ..SkillRoots::from_data_dir(tmp.path())
    });
    let a = root.for_bot(Some("a")).unwrap();
    a.write("shipped-only", &content("shipped-only", "mine"))
        .unwrap();
    assert!(
        a.publish("shipped-only", false)
            .unwrap_err()
            .contains("built-in")
    );
    a.publish("shipped-only", true).unwrap();
}

#[test]
fn stray_versioned_directories_do_not_block_publishing_the_skill() {
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("skills");
    // Bad version, name mismatch and version mismatch, none of them a real publication of x.
    fs::create_dir_all(shared.join("x@not a version")).unwrap();
    fs::create_dir_all(shared.join("x@2.0.0")).unwrap();
    fs::write(shared.join("x@2.0.0/SKILL.md"), content("other", "o")).unwrap();
    fs::create_dir_all(shared.join("x@9.9.9")).unwrap();
    fs::write(shared.join("x@9.9.9/SKILL.md"), content("x", "stray")).unwrap();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let a = root.for_bot(Some("a")).unwrap();
    a.write("x", &content("x", "real")).unwrap();
    a.publish("x", true).unwrap();
    assert!(shared.join("x@0.1.0/SKILL.md").exists());
    for stray in ["x@not a version", "x@2.0.0", "x@9.9.9"] {
        assert!(shared.join(stray).exists(), "{stray} must be left alone");
    }
    let view = root.for_bot(None).unwrap();
    assert_eq!(view.read("x", None).unwrap(), "real");
    assert!(
        view.issues().iter().any(|i| i.path.contains("x@9.9.9")),
        "{:?}",
        view.issues()
    );
}

#[test]
fn loading_does_not_create_lock_files_in_either_scope() {
    let tmp = TempDir::new().unwrap();
    put(&tmp.path().join("skills"), "s", "shared");
    put(&tmp.path().join("bots/a/skills"), "m", "mine");
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let a = root.for_bot(Some("a")).unwrap();
    assert!(a.get("s").is_some() && a.get("m").is_some());
    assert!(a.issues().is_empty(), "{:?}", a.issues());
    assert!(!tmp.path().join("skills/.praxi-skills.lock").exists());
    assert!(!tmp.path().join("bots/a/skills/.praxi-skills.lock").exists());
}

#[test]
fn readers_share_the_lock_instead_of_taking_it_exclusively() {
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("skills");
    put(&shared, "s", "shared");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(shared.join(".praxi-skills.lock"))
        .unwrap();
    fs4::FileExt::lock_shared(&lock).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let data_dir = tmp.path().to_path_buf();
    std::thread::spawn(move || {
        let root = SkillIndex::load_roots(SkillRoots::from_data_dir(&data_dir));
        let view = root.for_bot(None).unwrap();
        let _ = tx.send((view.get("s").is_some(), view.issues().len()));
    });
    let (found, issues) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a reader must not wait for another shared lock holder");
    assert!(found);
    assert_eq!(issues, 0);
    drop(lock);
}

#[cfg(unix)]
#[test]
fn a_read_only_shared_directory_still_loads_without_issues() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("skills");
    put(&shared, "s", "shared");
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o555)).unwrap();
    let writable = fs::File::create(shared.join("probe")).is_ok();
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    let view = root.for_bot(None).unwrap();
    let (found, issues) = (view.get("s").is_some(), view.issues());
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // running with privileges that ignore directory modes
    }
    assert!(found);
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn an_unchanged_scope_is_not_reloaded_on_every_resolution() {
    let tmp = TempDir::new().unwrap();
    put(&tmp.path().join("skills"), "s", "shared");
    let root = SkillIndex::load_roots(SkillRoots::from_data_dir(tmp.path()));
    root.set_racy_window(std::time::Duration::ZERO);
    let first = root.for_bot(Some("a")).unwrap();
    let again = root.for_bot(Some("a")).unwrap();
    assert!(first.same_view(&again));
    assert!(!first.same_view(&root.for_bot(Some("b")).unwrap()));
    // Out-of-process edits are noticed by the directory stamp.
    put(&tmp.path().join("skills"), "t", "new shared");
    let after_external = root.for_bot(Some("a")).unwrap();
    assert!(after_external.get("t").is_some());
    // A write through a tool-bound view is visible immediately to the next resolution.
    after_external.write("m", &content("m", "mine")).unwrap();
    root.reload();
    assert!(root.for_bot(Some("a")).unwrap().get("m").is_some());
}
