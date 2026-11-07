//! Hermetic tests for the skill hub.

use std::fs;

use tempfile::TempDir;

use crate::hub::source::IndexFile;
use crate::hub::{
    CurlFetcher, FindingKind, GitCli, HubPaths, InstallOptions, InstallSource, LockEntry, LockFile,
    RiskLevel, SkillHub, SourceSpec, scan_dir,
};

/// Run cargo test -p skills -- --ignored to run these.
/// They require git CLI available in PATH.

#[test]
fn local_install() {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("source");
    fs::create_dir_all(&src).expect("mkdir");
    fs::write(
        src.join("SKILL.md"),
        "---\nname: local_test\nversion: 0.1.0\ndescription: Local skill\n---\n# hi",
    )
    .expect("write");
    let dest = tmp.path().join("dest");
    let paths = HubPaths {
        skills_dir: dest.clone(),
        lock_file: dest.join("skills.lock.json"),
        usage_file: dest.join("skills-usage.json"),
    };
    let hub = SkillHub::with_backends(paths, Box::new(CurlFetcher), Box::new(GitCli));
    let preview = hub.preview(&InstallSource::Local(src)).expect("preview");
    assert_eq!(preview.name, "local_test");
    let opts = InstallOptions::default();
    hub.install(&preview, &opts).expect("install");
    assert!(dest.join("local_test").is_dir());
    let list = hub.list().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].entry.name, "local_test");
}

#[test]
fn uninstall() {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("source");
    fs::create_dir_all(&src).expect("mkdir");
    fs::write(
        src.join("SKILL.md"),
        "---\nname: uninst\nversion: 0.1.0\ndescription: Uninstall test\n---\n# hi",
    )
    .expect("write");
    let dest = tmp.path().join("dest");
    let paths = HubPaths {
        skills_dir: dest.clone(),
        lock_file: dest.join("skills.lock.json"),
        usage_file: dest.join("skills-usage.json"),
    };
    let hub = SkillHub::with_backends(paths, Box::new(CurlFetcher), Box::new(GitCli));
    let preview = hub.preview(&InstallSource::Local(src)).expect("preview");
    hub.install(&preview, &InstallOptions::default())
        .expect("install");
    hub.uninstall("uninst").expect("uninstall");
    assert!(!dest.join("uninst").is_dir());
    let list = hub.list().expect("list");
    assert!(list.is_empty());
}

#[test]
fn scan_scripts() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("skill");
    fs::create_dir_all(&root).expect("mkdir");
    fs::write(
        root.join("SKILL.md"),
        "---\nname: scan_me\nversion: 0.1.0\ndescription: Scan\n---\n#",
    )
    .expect("write");
    // Script with pipe to shell.
    fs::write(root.join("run.sh"), "#!/bin/sh | sh\nhello").expect("write");
    let report = scan_dir(&root).expect("scan");
    assert!(report.requires_acceptance());
    let has_script = report
        .findings
        .iter()
        .any(|f| f.kind == FindingKind::Script);
    assert!(has_script);
}

#[test]
fn scan_credentials() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("skill");
    fs::create_dir_all(&root).expect("mkdir");
    fs::write(
        root.join("SKILL.md"),
        "---\nname: cred_test\nversion: 0.1.0\ndescription: Creds\n---\n#",
    )
    .expect("write");
    fs::write(root.join("secrets.txt"), "AKIAIOSFODNN7EXAMPLE\nother data").expect("write");
    let report = scan_dir(&root).expect("scan");
    let has_creds = report
        .findings
        .iter()
        .any(|f| f.kind == FindingKind::Credential);
    assert!(has_creds);
}

#[test]
fn hash_stable() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("skill");
    fs::create_dir_all(&root).expect("mkdir");
    fs::write(
        root.join("SKILL.md"),
        "---\nname: stable\nversion: 0.1.0\ndescription: Stable\n---\n#",
    )
    .expect("write");
    fs::write(root.join("a.txt"), "content").expect("write");
    let root2 = tmp.path().join("skill2");
    fs::create_dir_all(&root2).expect("mkdir");
    fs::write(
        root2.join("SKILL.md"),
        "---\nname: stable\nversion: 0.1.0\ndescription: Stable\n---\n#",
    )
    .expect("write");
    fs::write(root2.join("a.txt"), "content").expect("write");
    let r1 = scan_dir(&root).expect("r1");
    let r2 = scan_dir(&root2).expect("r2");
    assert_eq!(r1.fingerprint, r2.fingerprint);
}

#[test]
fn lock_roundtrip() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = HubPaths {
        skills_dir: tmp.path().join("skills"),
        lock_file: tmp.path().join("lock.json"),
        usage_file: tmp.path().join("usage.json"),
    };
    let mut lock = LockFile::default();
    lock.skills.insert(
        "x".to_string(),
        LockEntry {
            name: "x".to_string(),
            version: "1.0.0".to_string(),
            source: SourceSpec::Local {
                path: "/tmp/x".to_string(),
            },
            pin: None,
            commit: None,
            hash: "abc".to_string(),
            risk: RiskLevel::Low,
            installed_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: None,
        },
    );
    lock.save(&paths.lock_file).expect("save");
    let read = LockFile::load(&paths.lock_file).expect("load");
    assert_eq!(read.skills.get("x").unwrap().version, "1.0.0");
}

#[test]
fn usage_counters() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = HubPaths {
        skills_dir: tmp.path().join("skills"),
        lock_file: tmp.path().join("lock.json"),
        usage_file: tmp.path().join("usage.json"),
    };
    let hub = SkillHub::with_backends(paths, Box::new(CurlFetcher), Box::new(GitCli));
    let before = hub.usage("foo");
    assert_eq!(before.uses, 0);
    let after = hub.record_use("foo").expect("record");
    assert_eq!(after.uses, 1);
    assert!(after.last_used_at.is_some());
}

#[test]
fn index_parsing() {
    let idx_json = r#"{
        "version": 1,
        "skills": [
            {
                "name": "indie_tool",
                "description": "Indexed tool",
                "git": "https://example.com/repo.git",
                "commit": "abc123",
                "subdir": null,
                "sha256": null
            }
        ]
    }"#;
    let idx: IndexFile = serde_json::from_str(idx_json).expect("parse index");
    assert_eq!(idx.version, 1);
    assert_eq!(idx.skills.len(), 1);
    assert_eq!(idx.skills[0].name, "indie_tool");
}

// Note: The git clone test requires git CLI availability on Windows CI.
// For now we skip it; developers can run locally.
