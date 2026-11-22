use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

const PASSPHRASE: &str = "backup-restore-test-only";

fn bot(root: &Path, data_dir: &str, json_out: bool, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bot"))
        .current_dir(root)
        .args(["--data-dir", data_dir])
        .args(["--port", "0", "--secrets-backend", "file"])
        .env("BOT_SECRETS_PASSPHRASE", "backup-restore-test-only")
        .env("PRAXI_TEST_BACKUP_PASS", PASSPHRASE)
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

const PASS: [&str; 2] = ["--passphrase-env", "PRAXI_TEST_BACKUP_PASS"];

#[test]
fn encrypted_backup_create_verify_restore_report_json() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    data_with_bot(&root.join("source"));
    let file = "praxi yedek ı.bak";
    let create = stdout(bot(
        root,
        "source",
        true,
        &[&["backup", "create", file][..], &PASS].concat(),
    ));
    let created: Value = serde_json::from_str(&create).unwrap();
    assert_eq!(created["encrypted"], json!(true));
    assert_eq!(created["secrets_included"], json!(false));
    let again = bot(
        root,
        "source",
        false,
        &[&["backup", "create", file][..], &PASS].concat(),
    );
    assert_failure(&again, "already exists");

    let verified: Value = serde_json::from_str(&stdout(bot(
        root,
        "source",
        true,
        &[&["backup", "verify", file][..], &PASS].concat(),
    )))
    .unwrap();
    assert_eq!(verified["files"], created["files"]);

    let restored: Value = serde_json::from_str(&stdout(bot(
        root,
        "target",
        true,
        &[&["backup", "restore", file][..], &PASS].concat(),
    )))
    .unwrap();
    assert_eq!(restored["files"], created["files"]);
    assert_eq!(restored["secrets_skipped"], json!(0));
    assert!(root.join("target/bot.db").is_file());

    let refused = bot(
        root,
        "target",
        false,
        &[&["backup", "restore", file][..], &PASS].concat(),
    );
    assert_failure(&refused, "re-run with `--replace`");
}

#[test]
fn passphrase_is_never_a_command_line_value_and_a_wrong_one_fails() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    data_with_bot(&root.join("source"));
    let file = "b.bak";
    stdout(bot(
        root,
        "source",
        false,
        &[&["backup", "create", file][..], &PASS].concat(),
    ));
    let rejected = bot(
        root,
        "source",
        false,
        &["backup", "verify", file, "--passphrase", "hunter2hunter2"],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--passphrase"));

    std::fs::write(root.join("wrong.txt"), "not the passphrase\n").unwrap();
    let wrong = bot(
        root,
        "source",
        false,
        &["backup", "verify", file, "--passphrase-file", "wrong.txt"],
    );
    assert_failure(&wrong, "wrong passphrase");
}

#[test]
fn backup_fails_with_text_on_stderr_in_both_output_modes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for json_out in [false, true] {
        let backup = bot(
            root,
            "empty",
            json_out,
            &[&["backup", "create", "missing.bak"][..], &PASS].concat(),
        );
        assert_failure(&backup, "nothing to back up");
        assert!(!root.join("missing.bak").exists());

        let verify = bot(
            root,
            "target",
            json_out,
            &[&["backup", "verify", "missing.bak"][..], &PASS].concat(),
        );
        assert_failure(&verify, "backup file missing.bak not found");
    }
}
