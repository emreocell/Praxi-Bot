use cli_provider::{PathEnv, install::run_install};
use std::time::Duration;
mod common;

fn fixture(mode: &str) -> (tempfile::TempDir, PathEnv) {
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_fake-cli"));
    let dir = common::fixture_dir(binary);
    common::install_binary(
        binary,
        &dir.path()
            .join(format!("npm{}", std::env::consts::EXE_SUFFIX)),
    );
    let env = PathEnv::for_test(dir.path().into())
        .with_env_var("FAKE_INSTALL_LOG", dir.path().to_string_lossy())
        .with_env_var("FAKE_INSTALL_MODE", mode)
        .with_env_var("PRAXI_PASSWORD", "MUST_NOT_ESCAPE")
        .with_env_var("DATABASE_URL", "MUST_NOT_ESCAPE")
        .with_env_var("NODE_OPTIONS", "MUST_NOT_ESCAPE")
        .with_env_var("BASH_ENV", "MUST_NOT_ESCAPE")
        .with_env_var("HTTPS_PROXY", "MUST_NOT_ESCAPE")
        .with_env_var("PRAXI_API_TOKEN", "MUST_NOT_ESCAPE");
    (dir, env)
}

#[tokio::test]
async fn installer_uses_registry_argv_streams_and_drops_non_allowlisted_environment() {
    let (dir, env) = fixture("");
    let mut output = String::new();
    run_install(
        "opencode",
        &env,
        dir.path(),
        Duration::from_secs(5),
        |text| output.push_str(&text),
    )
    .await
    .unwrap();
    let args: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("install-argv")).unwrap())
            .unwrap();
    assert_eq!(args, ["install", "-g", "opencode-ai"]);
    assert!(output.contains("fixture output"));
    let child_env = std::fs::read_to_string(dir.path().join("install-env")).unwrap();
    assert!(!child_env.contains("MUST_NOT_ESCAPE"));
    for key in [
        "NODE_OPTIONS",
        "BASH_ENV",
        "DATABASE_URL",
        "PRAXI_PASSWORD",
        "HTTPS_PROXY",
    ] {
        assert!(!child_env.contains(key));
    }
}

#[tokio::test]
async fn an_install_on_a_server_names_the_pinned_registry_itself() {
    let (dir, env) = fixture("");
    // The installer's units carry NPM_CONFIG_GLOBALCONFIG (the root-owned npmrc that pins the registry).
    // A registry set in the environment of the process is not handed to npm.
    let env = env
        .with_env_var("NPM_CONFIG_GLOBALCONFIG", "/etc/praxi/npmrc")
        .with_env_var("NPM_CONFIG_REGISTRY", "https://hijack.example/");
    run_install("opencode", &env, dir.path(), Duration::from_secs(5), |_| {})
        .await
        .unwrap();
    let args: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("install-argv")).unwrap())
            .unwrap();
    assert_eq!(
        args,
        [
            "install",
            "-g",
            "opencode-ai",
            "--registry",
            "https://registry.npmjs.org/"
        ]
    );
    let child_env = std::fs::read_to_string(dir.path().join("install-env")).unwrap();
    assert!(child_env.contains("NPM_CONFIG_GLOBALCONFIG"), "{child_env}");
    assert!(!child_env.contains("hijack.example"), "{child_env}");
}

#[tokio::test]
async fn installer_times_out_and_caps_output_without_shells() {
    let (dir, env) = fixture("hang");
    assert_eq!(
        run_install(
            "claude",
            &env,
            dir.path(),
            Duration::from_millis(300),
            |_| {}
        )
        .await,
        Err("timeout")
    );
    // The exclusive create proves the owned fixture has exited on Windows too.
    std::fs::rename(
        dir.path()
            .join(format!("npm{}", std::env::consts::EXE_SUFFIX)),
        dir.path().join("stopped"),
    )
    .unwrap();
    let (dir, env) = fixture("flood");
    assert_eq!(
        run_install("codex", &env, dir.path(), Duration::from_secs(5), |_| {}).await,
        Err("output_limit")
    );
    let (dir, env) = fixture("fail");
    assert_eq!(
        run_install("codex", &env, dir.path(), Duration::from_secs(5), |_| {}).await,
        Err("install_failed")
    );
}

#[tokio::test]
async fn installer_timeout_terminates_a_live_descendant() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let (dir, env) = fixture("");
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let env = env
        .with_env_var("PROBE_TREE_MODE", "hang")
        .with_env_var(
            "PROBE_TREE_ADDRESS",
            listener.local_addr().unwrap().to_string(),
        )
        .with_env_var(
            "PROBE_TREE_READY",
            dir.path().join("descendant.pid").to_string_lossy(),
        );
    let work = run_install("opencode", &env, dir.path(), Duration::from_secs(3), |_| {});
    let observe = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut ready = [0; 5];
        stream.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, b"ready");
        // The socket must belong to a live descendant before the timeout.
        stream.write_all(b"p").await.unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [b'p']);
        let exit = tokio::time::timeout(Duration::from_secs(8), stream.read(&mut byte)).await;
        // Clean up this fixture if the production tree guard regresses.
        if !matches!(exit, Ok(Ok(0) | Err(_))) {
            let _ = stream.write_all(b"q").await;
            panic!("installer descendant survived timeout");
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(work, observe)
    })
    .await
    .unwrap();
    assert_eq!(result, Err("timeout"));
}

#[test]
fn windows_npm_cmd_is_unwrapped_with_fixed_script_and_never_a_batch_runtime() {
    use cli_provider::install::resolve_installer;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("npm.cmd"), b"@echo off\r\nevil ignored\r\n").unwrap();
    let script = dir.path().join("node_modules/npm/bin/npm-cli.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, b"fixture").unwrap();
    let node = dir.path().join("node.exe");
    std::fs::write(&node, b"fixture").unwrap();
    let env = PathEnv::for_test(dir.path().into());
    let resolved = resolve_installer(&env).unwrap();
    assert_eq!(
        resolved.program,
        cli_provider::search::plain_path(std::fs::canonicalize(&node).unwrap())
    );
    assert_eq!(
        resolved.args,
        [
            cli_provider::search::plain_path(std::fs::canonicalize(script).unwrap())
                .to_string_lossy()
        ]
    );
    std::fs::remove_file(node).unwrap();
    std::fs::write(dir.path().join("node.cmd"), b"@echo off\r\nevil").unwrap();
    assert!(resolve_installer(&env).is_err());
}
