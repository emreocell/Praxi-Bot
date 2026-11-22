#![cfg(all(windows, feature = "windows-sandbox"))]

use std::os::windows::process::CommandExt;
use std::{io, process::Command};
use windows_sandbox_spike::{SandboxConfig, run_command};

fn enabled() -> SandboxConfig {
    SandboxConfig {
        enabled: true,
        ..SandboxConfig::default()
    }
}

#[test]
fn command_runs_in_workspace_but_cannot_read_or_write_outside_canary() {
    let parent = tempfile::tempdir().unwrap();
    let canary = parent.path().join("private canary.txt");
    std::fs::write(&canary, "outside-secret-canary").unwrap();
    // Control: this exact caller can read the file. A broken/never-started shell
    // cannot pass the workspace success marker below.
    let control = Command::new("cmd.exe")
        .args(["/d", "/s", "/c"])
        .raw_arg(format!("\"type \"{}\"\"", canary.display()))
        .output()
        .unwrap();
    assert!(control.status.success(), "{control:?}");
    assert!(String::from_utf8_lossy(&control.stdout).contains("outside-secret-canary"));
    let workspace = parent.path().join("fresh workspace");
    let command = format!(
        "(echo inside-ok)>inside.txt & type inside.txt >roundtrip.txt & type \"{}\" >leak.txt 2>read-error.txt & (echo overwritten)>\"{}\" 2>write-error.txt & exit /b 0",
        canary.display(),
        canary.display()
    );
    assert_eq!(run_command(&enabled(), &workspace, &command).unwrap(), 0);
    assert_eq!(
        std::fs::read_to_string(workspace.join("roundtrip.txt"))
            .unwrap()
            .trim(),
        "inside-ok"
    );
    assert!(
        !std::fs::read_to_string(workspace.join("leak.txt"))
            .unwrap()
            .contains("outside-secret-canary")
    );
    assert_ne!(
        std::fs::read(workspace.join("read-error.txt"))
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        std::fs::read_to_string(&canary).unwrap(),
        "outside-secret-canary"
    );
    // Reusing a directory must never alter its ACL or run a second command.
    assert!(run_command(&enabled(), &workspace, "echo unsafe>second.txt").is_err());
    assert!(!workspace.join("second.txt").exists());
}

#[test]
fn timeout_stops_the_shell() {
    let parent = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        timeout_ms: 200,
        ..enabled()
    };
    let error = run_command(
        &config,
        &parent.path().join("timeout"),
        "for /l %i in (0,0,1) do @rem spin",
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error:?}");
}
