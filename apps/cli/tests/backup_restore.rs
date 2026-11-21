use std::fs::File;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use state_bundle::Manifest;

fn bot(root: &Path, data_dir: &str, json_out: bool, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bot"))
        .current_dir(root)
        .args(["--data-dir", data_dir])
        .args(["--port", "0", "--secrets-backend", "file"])
        .env("BOT_SECRETS_PASSPHRASE", "backup-restore-test-only")
        .env_remove("BOT_CATALOG_DIR")
        .args(json_out.then_some("--json"))
        .args(args)
        .output()
        .unwrap()
}

fn data_with_bot(dir: &Path) {
    let storage = storage::Storage::open(dir).unwrap();
    let user = protocol::UserId::new();
    storage
        .insert_user(&storage::User {
            id: user,
            name: "owner".into(),
            created_at_ms: 1,
        })
        .unwrap();
    storage
        .insert_bot(&storage::Bot {
            id: protocol::BotId::new(),
            user_id: user,
            name: "Atlas".into(),
            config: json!({"bot": {"name": "Atlas"}}),
            created_at_ms: 1,
        })
        .unwrap();
}

fn manifest(file: &Path) -> Manifest {
    let mut archive = tar::Archive::new(File::open(file).unwrap());
    let entry = archive
        .entries()
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.path().unwrap() == Path::new("manifest.json"))
        .expect("backup manifest");
    serde_json::from_reader(entry).unwrap()
}

fn stdout(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn assert_failure(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error: ") && stderr.contains(message),
        "{stderr}"
    );
}

#[test]
fn backup_and_restore_report_results_in_both_output_modes() {
    for json_out in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        data_with_bot(&root.join("source"));
        let file = "praxi yedek ı.tar";
        let backup = stdout(bot(root, "source", json_out, &["backup", file]));
        let existing = bot(root, "source", json_out, &["backup", file]);
        assert_failure(&existing, "already exists");
        let manifest = manifest(&root.join(file));
        let files = manifest.files.len();
        let bytes: u64 = manifest.files.iter().map(|entry| entry.size).sum();
        if json_out {
            assert_eq!(
                serde_json::from_str::<Value>(&backup).unwrap(),
                json!({"file": file, "files": files, "bytes": bytes})
            );
        } else {
            assert_eq!(
                backup,
                format!(
                    "Backup written to {file} ({files} files, {bytes} bytes).\nIt does not contain API keys or tokens; enter them again after a restore.\n"
                )
            );
        }

        let app_version = manifest.app_version;
        let summary =
            format!("Restored {files} files from {file} (created with Praxi {app_version}).\n");
        let keys = "API keys are not part of a backup: enter them again (`bot secrets set`, or the app's settings).\n";
        let restore = stdout(bot(root, "target", json_out, &["restore", file]));
        let mut expected = json!({
            "file": file, "files": files, "app_version": app_version, "previous_state": null,
        });
        if json_out {
            assert_eq!(serde_json::from_str::<Value>(&restore).unwrap(), expected);
        } else {
            assert_eq!(restore, format!("{summary}{keys}"));
        }

        let refused = bot(root, "target", json_out, &["restore", file]);
        assert_failure(&refused, "re-run with `--replace`");
        let replacement = stdout(bot(
            root,
            "target",
            json_out,
            &["restore", file, "--replace"],
        ));
        let previous = root
            .join("target")
            .read_dir()
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("backup-"))
            .expect("previous state directory");
        assert!(previous.path().join("bot.db").is_file());
        let previous = Path::new("target").join(previous.file_name());
        if json_out {
            expected["previous_state"] = json!(previous.to_str().unwrap());
            assert_eq!(
                serde_json::from_str::<Value>(&replacement).unwrap(),
                expected
            );
        } else {
            assert_eq!(
                replacement,
                format!(
                    "{summary}The previous state was kept in {}.\n{keys}",
                    previous.display()
                )
            );
        }
    }
}

#[test]
fn backup_and_restore_fail_with_text_on_stderr_in_both_output_modes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for json_out in [false, true] {
        let backup = bot(root, "empty", json_out, &["backup", "missing.tar"]);
        assert_failure(&backup, "nothing to back up");
        assert!(!root.join("missing.tar").exists());

        let restore = bot(root, "target", json_out, &["restore", "missing.tar"]);
        assert_failure(&restore, "backup file missing.tar not found");
    }
}
