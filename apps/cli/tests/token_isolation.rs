//! Real headless launch: no provider credentials, desktop access or fixed port.
#![cfg(feature = "fake-provider")]

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        // Only the process owned by this test, never a process-name kill.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bot"));
    command
        .arg("--data-dir")
        .arg(dir)
        .args(["--json", "--port", "0", "serve", "--fake-provider"]);
    command
}

fn assert_no_secret_on_disk(dir: &Path, secret: &[u8]) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_secret_on_disk(&path, secret);
        } else {
            assert!(
                !std::fs::read(&path)
                    .unwrap()
                    .windows(secret.len())
                    .any(|window| window == secret),
                "management credential persisted in {}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn supervised_pipe_bootstrap_keeps_headless_management_off_disk() {
    // A trusted launcher must also guard its own credential-holding memory.
    process_guard::protect(process_guard::Policy {
        linux_no_new_privs: false,
        ..process_guard::Policy::default()
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let high = daemon_api::auth::launch_token().unwrap();
    let mut daemon = Daemon(
        command(dir.path())
            .arg("--management-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    writeln!(daemon.0.stdin.take().unwrap(), "{high}").unwrap();
    let stdout = daemon.0.stdout.take().unwrap();
    let announcement = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        }),
    )
    .await
    .expect("daemon did not announce startup")
    .unwrap();
    let base = announcement["listening"].as_str().unwrap();
    let low = daemon_api::read_token(dir.path()).unwrap();
    // Keep credentials out of assertion diagnostics even on a regression.
    let distinct = low != high;
    assert!(distinct, "disk and management authority must differ");
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for (token, status) in [(&low, 403), (&high, 200)] {
        let response = http
            .get(format!("{base}/v1/approvals"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    let observed: serde_json::Value = http
        .get(format!("{base}/v1/isolation"))
        .bearer_auth(&low)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(observed["privilege_split"], true);
    assert_eq!(observed["children_scrubbed"], true);
    assert_eq!(
        observed["memory_guard"],
        cfg!(any(windows, target_os = "linux"))
    );
    assert_eq!(observed["os_sandbox"], false);
    assert_no_secret_on_disk(dir.path(), high.as_bytes());
    drop(daemon);
}

#[tokio::test]
async fn credential_file_redirect_is_rejected_before_daemon_startup() {
    let dir = tempfile::tempdir().unwrap();
    // Deliberately not a minted management credential.
    let frame = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(frame.path(), format!("{}\n", "0".repeat(64))).unwrap();
    let mut command = tokio::process::Command::from(command(dir.path()));
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        command
            .arg("--ui-bootstrap-stdin")
            .stdin(std::fs::File::open(frame.path()).unwrap())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("regular file input started a daemon")
    .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pipe/socket"));
    assert!(!daemon_api::token_path(dir.path()).exists());
}

#[tokio::test]
async fn external_guarded_daemon_proves_ownership_via_challenge_without_proc_fd() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon(
        command(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let child_pid = daemon.0.id();
    let stdout = daemon.0.stdout.take().unwrap();
    let announcement = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        }),
    )
    .await
    .expect("daemon did not announce startup")
    .unwrap();
    let base = announcement["listening"].as_str().unwrap();
    let low = daemon_api::read_token(dir.path()).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    let isolation: serde_json::Value = http
        .get(format!("{base}/v1/isolation"))
        .bearer_auth(&low)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        isolation["memory_guard"],
        cfg!(any(windows, target_os = "linux"))
    );

    #[cfg(target_os = "linux")]
    {
        // Confirm PR_SET_DUMPABLE=0 is active and blocks /proc/<pid>/fd reads.
        assert!(
            std::fs::read_dir(format!("/proc/{child_pid}/fd")).is_err(),
            "non-dumpable daemon must block /proc/<pid>/fd reads"
        );
        // Confirm /proc/net/tcp reports our UID for the listening socket.
        let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
        let want = format!(":{port:04X}");
        let my_uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap());
        let tcp = std::fs::read_to_string("/proc/net/tcp").unwrap();
        let uids: Vec<u32> = tcp
            .lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let local = f.get(1)?;
                let (addr, _) = local.split_once(':')?;
                (f.get(3) == Some(&"0A")
                    && local.ends_with(&want)
                    && (addr == "0100007F" || addr == "00000000"))
                    .then(|| f.get(7)?.parse::<u32>().ok())
                    .flatten()
            })
            .collect();
        assert!(!uids.is_empty() && uids.iter().all(|&u| u == my_uid));
    }
    let _ = child_pid;

    let nonce = daemon_api::auth::launch_token().unwrap();
    let resp: serde_json::Value = http
        .post(format!("{base}/v1/auth/challenge"))
        .json(&serde_json::json!({ "nonce": nonce, "scope": "cli" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        resp["proof"].as_str().unwrap(),
        daemon_api::auth::listener_proof(&low, "cli", &nonce)
    );
    drop(daemon);
}
