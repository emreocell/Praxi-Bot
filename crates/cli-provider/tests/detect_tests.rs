use cli_provider::detect::{AuthState, Detector, PathEnv};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-cli"))
}

fn install_fake(dir: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    let file_name = format!("{name}.exe");
    #[cfg(not(windows))]
    let file_name = name.to_owned();

    let dest = dir.join(file_name);
    std::fs::copy(fake_cli_path(), &dest).expect("copy fake-cli");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).expect("set exec");
    }

    dest
}

#[tokio::test]
async fn test_all_missing() {
    let temp = tempfile::tempdir().unwrap();
    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);

    let detected = detector.detect(false).await;
    assert_eq!(detected.len(), 6);

    for cli in &detected {
        assert!(!cli.installed);
        assert!(cli.version.is_none());
        assert_eq!(cli.auth, AuthState::Unknown);
        assert!(cli.account_label.is_none());
        assert!(!cli.runnable);
        assert_eq!(cli.detail.as_deref(), Some("not_on_path"));
    }
}

#[tokio::test]
async fn test_claude_signed_in_and_signed_out() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "claude");

    // Signed in
    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert!(claude.installed);
    assert_eq!(claude.version.as_deref(), Some("fake 1.2.3"));
    assert_eq!(claude.auth, AuthState::SignedIn);
    assert_eq!(claude.account_label.as_deref(), Some("claude@example.com"));
    assert!(claude.detail.is_none());

    // Signed out
    let env =
        PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_AUTH_SIGNED_OUT", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert!(claude.installed);
    assert_eq!(claude.auth, AuthState::SignedOut);
    assert!(claude.account_label.is_none());
}

#[tokio::test]
async fn test_claude_extra_fields_never_leaks_token() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "claude");

    let env =
        PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_CLAUDE_EXTRA_FIELDS", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();

    assert!(claude.installed);
    assert_eq!(claude.auth, AuthState::SignedIn);
    assert_eq!(claude.account_label.as_deref(), Some("alice@example.com"));

    // Check Debug representation
    let debug_str = format!("{claude:?}");
    assert!(!debug_str.contains("SUPER_SECRET_TOKEN_DO_NOT_LEAK"));

    // Check JSON representation
    let json_str = serde_json::to_string(&claude).unwrap();
    assert!(!json_str.contains("SUPER_SECRET_TOKEN_DO_NOT_LEAK"));
}

#[tokio::test]
async fn test_codex_status_variants() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "codex");

    // Signed in: "Logged in using ChatGPT"
    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let codex = detected.iter().find(|c| c.id == "codex").unwrap();
    assert!(codex.installed);
    assert_eq!(codex.auth, AuthState::SignedIn);
    assert_eq!(codex.version.as_deref(), Some("fake 1.2.3"));

    // chatgpt-plan follows codex installation, auth is unknown
    let chatgpt = detected.iter().find(|c| c.id == "chatgpt-plan").unwrap();
    assert!(chatgpt.installed);
    assert_eq!(chatgpt.auth, AuthState::Unknown);
    assert_eq!(chatgpt.version.as_deref(), Some("fake 1.2.3"));

    // Signed out: "Not logged in"
    let env =
        PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_AUTH_SIGNED_OUT", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let codex = detected.iter().find(|c| c.id == "codex").unwrap();
    assert_eq!(codex.auth, AuthState::SignedOut);

    // Garbage stderr
    let env =
        PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_CODEX_GARBAGE_STDERR", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let codex = detected.iter().find(|c| c.id == "codex").unwrap();
    assert_eq!(codex.auth, AuthState::Unknown);
    assert_eq!(codex.detail.as_deref(), Some("auth_unparsed"));
}

#[tokio::test]
async fn test_cursor_status_variants() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "cursor-agent");

    // JSON authenticated
    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let cursor = detected.iter().find(|c| c.id == "cursor").unwrap();
    assert!(cursor.installed);
    assert_eq!(cursor.auth, AuthState::SignedIn);

    // JSON unauthenticated
    let env =
        PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_AUTH_SIGNED_OUT", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let cursor = detected.iter().find(|c| c.id == "cursor").unwrap();
    assert_eq!(cursor.auth, AuthState::SignedOut);

    // Fallback to plain text
    let env = PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_CURSOR_PLAIN", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let cursor = detected.iter().find(|c| c.id == "cursor").unwrap();
    assert_eq!(cursor.auth, AuthState::SignedIn);
}

#[tokio::test]
async fn test_opencode_variants() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "opencode");

    // Signed in via OPENCODE_API_KEY
    let env = PathEnv::for_test(temp.path().to_path_buf())
        .with_env_var("OPENCODE_API_KEY", "some_key_value");
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let opencode = detected.iter().find(|c| c.id == "opencode").unwrap();
    assert!(opencode.installed);
    assert_eq!(opencode.auth, AuthState::SignedIn);
    let debug_str = format!("{opencode:?}");
    assert!(!debug_str.contains("some_key_value"));

    // Signed out when no key and no auth.json
    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let opencode = detected.iter().find(|c| c.id == "opencode").unwrap();
    assert_eq!(opencode.auth, AuthState::SignedOut);

    // Unknown when auth.json exists
    let auth_dir = temp.path().join("local_app").join("opencode");
    std::fs::create_dir_all(&auth_dir).unwrap();
    std::fs::write(auth_dir.join("auth.json"), "SENTINEL_TOKEN").unwrap();

    let env = PathEnv::for_test(temp.path().to_path_buf())
        .with_local_app_data(temp.path().join("local_app"));
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let opencode = detected.iter().find(|c| c.id == "opencode").unwrap();
    assert_eq!(opencode.auth, AuthState::Unknown);
    let debug_str = format!("{opencode:?}");
    assert!(!debug_str.contains("SENTINEL_TOKEN"));
}

#[tokio::test]
async fn test_gemini_variants() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "gemini");

    // Signed in via GEMINI_API_KEY
    let env = PathEnv::for_test(temp.path().to_path_buf())
        .with_env_var("GEMINI_API_KEY", "gemini_secret_key");
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let gemini = detected.iter().find(|c| c.id == "gemini").unwrap();
    assert!(gemini.installed);
    assert_eq!(gemini.auth, AuthState::SignedIn);
    assert!(!format!("{gemini:?}").contains("gemini_secret_key"));

    // Signed in via GOOGLE_API_KEY
    let env = PathEnv::for_test(temp.path().to_path_buf())
        .with_env_var("GOOGLE_API_KEY", "google_secret_key");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let gemini = detected.iter().find(|c| c.id == "gemini").unwrap();
    assert_eq!(gemini.auth, AuthState::SignedIn);

    // Signed out when no key and no oauth_creds.json
    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let gemini = detected.iter().find(|c| c.id == "gemini").unwrap();
    assert_eq!(gemini.auth, AuthState::SignedOut);

    // Unknown when oauth_creds.json exists
    let gemini_dir = temp.path().join("home").join(".gemini");
    std::fs::create_dir_all(&gemini_dir).unwrap();
    std::fs::write(gemini_dir.join("oauth_creds.json"), "SENTINEL_TOKEN").unwrap();

    let env = PathEnv::for_test(temp.path().to_path_buf()).with_home(temp.path().join("home"));
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let gemini = detected.iter().find(|c| c.id == "gemini").unwrap();
    assert_eq!(gemini.auth, AuthState::Unknown);
    assert!(!format!("{gemini:?}").contains("SENTINEL_TOKEN"));
}

#[tokio::test]
async fn test_timeout_and_overflow() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "claude");

    // Overflow (> 64 KiB)
    let env = PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_CLI_OVERFLOW", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert!(claude.installed);
    assert_eq!(claude.detail.as_deref(), Some("output_too_large"));
    assert!(claude.version.is_none());

    // Timeout (simulated with FAKE_CLI_TIMEOUT)
    let env = PathEnv::for_test(temp.path().to_path_buf()).with_env_var("FAKE_CLI_TIMEOUT", "1");
    let detector = Detector::with_env(env);
    let detected = detector.detect(true).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert_eq!(claude.detail.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn test_windows_cmd_shim_refused_and_multiline() {
    let temp = tempfile::tempdir().unwrap();

    // Create a .cmd file that cannot be unwrapped safely
    let cmd_file = temp.path().join("claude.cmd");
    std::fs::write(&cmd_file, "@echo off\r\ncmd.exe /c echo dangerous\r\n").unwrap();

    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();

    assert!(claude.installed);
    assert!(claude.version.is_none());
    assert_eq!(claude.auth, AuthState::Unknown);
    assert_eq!(claude.detail.as_deref(), Some("shim_refused"));

    // Multi-line fixture output: only first line is taken as version
    let temp2 = tempfile::tempdir().unwrap();
    install_fake(temp2.path(), "codex");
    let env2 =
        PathEnv::for_test(temp2.path().to_path_buf()).with_env_var("FAKE_CLI_MULTILINE", "1");
    let detector2 = Detector::with_env(env2);
    let detected2 = detector2.detect(true).await;
    let codex2 = detected2.iter().find(|c| c.id == "codex").unwrap();
    assert_eq!(codex2.version.as_deref(), Some("fake 1.2.3"));
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        output.is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .any(|field| field == pid.to_string())
        })
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
}

#[tokio::test]
async fn test_secret_env_is_not_passed_to_the_probe() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "claude");
    let env = PathEnv::for_test(temp.path().to_path_buf())
        .with_env_var("API_KEY", "SENTINEL_KEY_DO_NOT_LEAK")
        .with_env_var("ACCESS_TOKEN", "SENTINEL_TOKEN_DO_NOT_LEAK")
        .with_env_var("DB_SECRET", "SENTINEL_SECRET_DO_NOT_LEAK")
        .with_env_var("USER_CREDENTIAL", "SENTINEL_CREDENTIAL_DO_NOT_LEAK");
    let detector = Detector::with_env(env);
    let detected = detector.detect(false).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert_eq!(claude.version.as_deref(), Some("fake 1.2.3"));
    let rendered = format!("{claude:?}{}", serde_json::to_string(claude).unwrap());
    assert!(!rendered.contains("SENTINEL_"), "{rendered}");
    assert!(!rendered.contains("LEAKED"), "{rendered}");
}

#[tokio::test]
async fn test_batch_node_shim_is_not_executed() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    std::fs::write(dir.join("cli.js"), "console.log('nope')\n").unwrap();
    std::fs::write(dir.join("claude.cmd"), "\"%~dp0\\cli.js\" %*\r\n").unwrap();
    #[cfg(windows)]
    std::fs::write(
        dir.join("node.cmd"),
        "@echo off\r\necho RAN> \"%~dp0spawned.txt\"\r\necho fake 1.2.3\r\n",
    )
    .unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            dir.join("node.cmd"),
            "#!/bin/sh\necho RAN > \"$(dirname \"$0\")/spawned.txt\"\necho fake 1.2.3\n",
        )
        .unwrap();
        std::fs::set_permissions(dir.join("node.cmd"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    let detector = Detector::with_env(PathEnv::for_test(dir.to_path_buf()));
    let detected = detector.detect(false).await;
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert!(
        !dir.join("spawned.txt").exists(),
        "node.cmd must not be spawned"
    );
    assert_eq!(claude.detail.as_deref(), Some("shim_refused"));
    assert!(claude.version.is_none());
}

#[tokio::test]
async fn test_timeout_kills_the_child_tree() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "claude");
    let marker = temp.path().join("grandchild.pid");
    let env = PathEnv::for_test(temp.path().to_path_buf())
        .with_env_var("FAKE_SPAWN_TREE", "1")
        .with_env_var("FAKE_TREE_MARKER", marker.to_string_lossy().into_owned());
    let detector = Detector::with_env(env);
    let started = std::time::Instant::now();
    let detected = detector.detect(false).await;
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "probe did not time out"
    );
    let claude = detected.iter().find(|c| c.id == "claude").unwrap();
    assert_eq!(claude.detail.as_deref(), Some("timeout"));

    let pid: u32 = std::fs::read_to_string(&marker)
        .expect("grandchild pid file")
        .trim()
        .parse()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !pid_alive(pid),
        "grandchild {pid} survived the probe timeout"
    );
}

#[tokio::test]
async fn test_caching_and_refresh() {
    let temp = tempfile::tempdir().unwrap();
    install_fake(temp.path(), "claude");

    let env = PathEnv::for_test(temp.path().to_path_buf());
    let detector = Detector::with_env(env);

    let detected1 = detector.detect(false).await;
    let claude1 = detected1.iter().find(|c| c.id == "claude").unwrap();
    assert_eq!(claude1.auth, AuthState::SignedIn);

    // Remove the binary
    #[cfg(windows)]
    std::fs::remove_file(temp.path().join("claude.exe")).unwrap();
    #[cfg(not(windows))]
    std::fs::remove_file(temp.path().join("claude")).unwrap();

    // Cached probe within 60s still returns signed in
    let detected2 = detector.detect(false).await;
    let claude2 = detected2.iter().find(|c| c.id == "claude").unwrap();
    assert!(claude2.installed);

    // refresh=true bypasses cache and detects missing binary
    let detected3 = detector.detect(true).await;
    let claude3 = detected3.iter().find(|c| c.id == "claude").unwrap();
    assert!(!claude3.installed);
    assert_eq!(claude3.detail.as_deref(), Some("not_on_path"));
}
