//! No daemon, keychain, credentials or network: this command must work offline.
use std::process::Command;

#[test]
fn logs_reads_only_recent_structured_records_with_the_requested_filters() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("logs")).unwrap();
    let entries: Vec<_> = [(1000, "warn"), (2000, "warn"), (3000, "error")]
        .into_iter()
        .map(|(timestamp_ms, level)| {
            serde_json::to_string(&support_logs::Entry {
                timestamp_ms,
                id: uuid::Uuid::now_v7().to_string(),
                level: level.into(),
                component: "daemon".into(),
                fields: [("phase".into(), "begin".into())].into(),
            })
            .unwrap()
        })
        .collect();
    std::fs::write(dir.path().join("logs/daemon.jsonl"), entries.join("\n")).unwrap();
    std::fs::write(
        dir.path().join("desktop-daemon.log"),
        "sk-legacy-canary private@example.test",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bot"))
        .arg("--data-dir")
        .arg(dir.path())
        .args(["logs", "--level", "warn", "--since", "1500"])
        .env_remove("BOT_API_TOKEN")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let entry: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(entry["timestamp_ms"], 2000);
    assert_eq!(entry["level"], "warn");
    assert!(!stdout.contains("canary"));
}
