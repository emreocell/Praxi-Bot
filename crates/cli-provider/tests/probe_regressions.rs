use cli_provider::detect::{AuthState, DetectedCli, Detector, PathEnv};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DEADLINE: Duration = Duration::from_secs(15);

fn install_fake(root: &Path, name: &str) -> PathBuf {
    let program = root.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_fake-cli"), &program).unwrap();
    program
}

async fn tree_fixture(mode: &str) -> (tempfile::TempDir, PathEnv, TcpListener, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("probe ö 空");
    std::fs::create_dir(&root).unwrap();
    let program = install_fake(&root, "claude");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let env = PathEnv::for_test(root.clone())
        .with_env_var("PROBE_TREE_MODE", mode)
        .with_env_var(
            "PROBE_TREE_ADDRESS",
            listener.local_addr().unwrap().to_string(),
        )
        .with_env_var(
            "PROBE_TREE_READY",
            root.join("descendant.pid").to_str().unwrap(),
        );
    (dir, env, listener, program)
}

/// A live socket is a portable process-lifetime witness. No PID lookup, external
/// shell or OS utility is needed, and the explicit quit command cleans up a red
/// test even when production cleanup is deliberately reverted.
struct TreeObserver {
    stream: TcpStream,
    cleanup: std::net::TcpStream,
}

impl TreeObserver {
    async fn accept(listener: &TcpListener) -> Self {
        let (mut stream, _) = tokio::time::timeout(DEADLINE, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut ready = [0; 5];
        tokio::time::timeout(DEADLINE, stream.read_exact(&mut ready))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&ready, b"ready");
        let stream = stream.into_std().unwrap();
        let cleanup = stream.try_clone().unwrap();
        Self {
            stream: TcpStream::from_std(stream).unwrap(),
            cleanup,
        }
    }

    async fn assert_alive(&mut self) {
        self.stream.write_all(b"p").await.unwrap();
        let mut reply = [0];
        tokio::time::timeout(DEADLINE, self.stream.read_exact(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&reply, b"p");
    }

    async fn assert_dead(&mut self) {
        let mut byte = [0];
        let result = tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut byte))
            .await
            .expect("descendant retained its live socket after probe cleanup");
        match result {
            Ok(0) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            other => panic!("descendant did not exit: {other:?}"),
        }
    }
}

impl Drop for TreeObserver {
    fn drop(&mut self) {
        let _ = self.cleanup.write_all(b"q");
    }
}

struct ProbeTask(tokio::task::JoinHandle<Vec<DetectedCli>>);

impl Drop for ProbeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn probe_tree_fixture_really_outlives_its_leader() {
    let (_dir, env, listener, program) = tree_fixture("exit").await;
    let mut command = tokio::process::Command::new(program);
    command
        .arg("--version")
        .envs(env.env_vars)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut leader = command.spawn().unwrap();
    let mut observer = TreeObserver::accept(&listener).await;
    let status = tokio::time::timeout(DEADLINE, leader.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    observer.assert_alive().await;
    observer.stream.write_all(b"q").await.unwrap();
    observer.assert_dead().await;
}

#[tokio::test]
async fn leader_exit_cleans_descendants_before_draining_inherited_output() {
    for mode in ["exit", "closed"] {
        let (_dir, env, listener, _program) = tree_fixture(mode).await;
        let detector = Detector::with_env(env);
        let mut task = ProbeTask(tokio::spawn(async move { detector.detect(false).await }));
        let mut observer = TreeObserver::accept(&listener).await;
        let detected = tokio::time::timeout(DEADLINE, &mut task.0)
            .await
            .unwrap()
            .unwrap();
        observer.assert_dead().await;
        let claude = detected.iter().find(|cli| cli.id == "claude").unwrap();
        assert_eq!(
            claude.version.as_deref(),
            Some("fake 1.2.3"),
            "{mode}: {claude:?}"
        );
        assert_eq!(claude.auth, AuthState::SignedIn, "{mode}: {claude:?}");
        assert!(claude.detail.is_none(), "{mode}: {claude:?}");
    }
}

#[tokio::test]
async fn cancelling_a_probe_future_kills_the_whole_tree() {
    let (_dir, env, listener, _program) = tree_fixture("hang").await;
    let detector = Detector::with_env(env);
    let mut task = ProbeTask(tokio::spawn(async move { detector.detect(false).await }));
    let mut observer = TreeObserver::accept(&listener).await;
    observer.assert_alive().await;
    task.0.abort();
    assert!((&mut task.0).await.unwrap_err().is_cancelled());
    observer.assert_dead().await;
}

async fn cursor_status(env: PathEnv) -> DetectedCli {
    Detector::with_env(env)
        .detect(false)
        .await
        .into_iter()
        .find(|cli| cli.id == "cursor")
        .unwrap()
}

#[tokio::test]
async fn cursor_plain_status_rejects_negatives_and_conflicting_streams() {
    let dir = tempfile::tempdir().unwrap();
    install_fake(dir.path(), "cursor-agent");
    for text in [
        "not logged in",
        "not signed in",
        "not authenticated",
        "unauthenticated",
        "logged out",
        "signed out",
        "no account",
        "please log in",
        "please sign in",
        "please login",
    ] {
        let env = PathEnv::for_test(dir.path().into())
            .with_env_var("FAKE_CURSOR_PLAIN", "1")
            .with_env_var(
                "FAKE_CURSOR_TEXT",
                format!("Notice: \u{1b}[31m{text}\u{1b}[0m."),
            );
        let cursor = cursor_status(env).await;
        assert_eq!(cursor.auth, AuthState::SignedOut, "{text}: {cursor:?}");
        assert!(cursor.detail.is_none());
    }
    let env = PathEnv::for_test(dir.path().into())
        .with_env_var("FAKE_CURSOR_PLAIN", "1")
        .with_env_var("FAKE_CURSOR_TEXT", "Authenticated")
        .with_env_var("FAKE_CURSOR_STDERR", "Not logged in");
    let cursor = cursor_status(env).await;
    assert_eq!(cursor.auth, AuthState::Unknown);
    assert_eq!(cursor.detail.as_deref(), Some("auth_ambiguous"));
}

#[tokio::test]
async fn cursor_positive_help_with_failing_exit_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    install_fake(dir.path(), "cursor-agent");
    let env = PathEnv::for_test(dir.path().into())
        .with_env_var("FAKE_CURSOR_PLAIN", "1")
        .with_env_var(
            "FAKE_CURSOR_TEXT",
            "Usage: status\nLogged in as <account>\nExample output",
        )
        .with_env_var("FAKE_CURSOR_FAIL", "1");
    let cursor = cursor_status(env).await;
    assert_eq!(cursor.auth, AuthState::Unknown);
    assert_eq!(cursor.detail.as_deref(), Some("auth_unparsed"));
}

#[tokio::test]
async fn cursor_structured_status_takes_precedence_over_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    install_fake(dir.path(), "cursor-agent");
    for (json, plain, expected) in [
        (
            r#"{"authenticated":false}"#,
            "Authenticated",
            AuthState::SignedOut,
        ),
        (
            r#"{"auth":{"loggedIn":true}}"#,
            "Not logged in",
            AuthState::SignedIn,
        ),
    ] {
        let env = PathEnv::for_test(dir.path().into())
            .with_env_var("FAKE_CURSOR_JSON", json)
            .with_env_var("FAKE_CURSOR_TEXT", plain);
        let cursor = cursor_status(env).await;
        assert_eq!(cursor.auth, expected);
        assert!(cursor.detail.is_none());
    }
}
