//! Smoke tests for the sign-in flows that run on a real pseudo terminal (`openpty` on Linux):
//! stand-in programs that behave like the vendors' TUIs (terminal queries, menus, a long link),
//! driven through the same `login::start` / `next_prompt` path the daemon uses. Nothing here
//! reaches a vendor or the network. `real_clis_print_their_link_on_a_pty` is the one test that
//! starts installed vendor CLIs (in an empty home, never typing a code); it is `#[ignore]`d:
//! `cargo test -p cli-provider --test pty_login_smoke -- --ignored --nocapture`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use cli_provider::detect::PathEnv;
use cli_provider::login::{Prompt, login_spec, pin_installed, start};

fn fake(bin: &Path, name: &str, script: &str) {
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn env_with(bin: &Path, home: &Path) -> PathEnv {
    PathEnv::for_test(bin.to_path_buf())
        .with_env_var("HOME", home.to_string_lossy())
        .with_env_var(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
}

async fn prompt_of(engine: &str, bin: &Path, home: &Path) -> (Option<Prompt>, String) {
    let env = env_with(bin, home);
    let pins = pin_installed(engine, &env).expect("pin");
    let spec = login_spec(engine).expect("spec");
    let mut run = start(spec, &env, Some(&pins)).expect("start");
    let prompt = run.next_prompt(spec, Duration::from_secs(20)).await;
    let output = run.private_output();
    run.terminate().await;
    (prompt, output)
}

/// `OpenCode`: a full-screen prompt library asks the terminal where the cursor is, shows a menu
/// with colours and a hidden cursor, takes Enter on the default, then prints the link and code.
#[tokio::test]
async fn an_opencode_like_menu_is_answered_and_its_link_and_code_are_found() {
    let dir = tempfile::tempdir().unwrap();
    let (bin, home) = (dir.path().join("bin"), dir.path().join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    fake(
        &bin,
        "opencode",
        r"printf '\033[?25l\033[6n'
/bin/stty raw -echo
dd bs=1 count=6 2>/dev/null >/dev/null
printf '\033[1;36m\342\227\206\033[0m  Select login method\r\n\342\224\202  \033[32m\342\227\217\033[0m OpenCode Console account\r\n\342\224\202  \342\227\213 API key (service account)\r\n'
dd bs=1 count=1 2>/dev/null >/dev/null
printf '\033[2K\r\342\227\207  Go to: \033[4mhttps://opencode.ai/auth/device?user_code=ABCD-1234\033[0m\r\n   Enter code: ABCD-1234\r\n'
sleep 30",
    );
    let (prompt, output) = prompt_of("opencode", &bin, &home).await;
    assert_eq!(
        prompt,
        Some(Prompt::Page {
            url: "https://opencode.ai/auth/device?user_code=ABCD-1234".into(),
            code: Some("ABCD-1234".into()),
        }),
        "{output:?}"
    );
}

/// `Antigravity`: the sign-in link is far longer than a line of the terminal and there is a
/// spinner redrawn over it first.
#[tokio::test]
async fn an_antigravity_like_run_with_a_very_long_link_is_found_in_one_piece() {
    let dir = tempfile::tempdir().unwrap();
    let (bin, home) = (dir.path().join("bin"), dir.path().join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let long = "a".repeat(700);
    fake(
        &bin,
        "agy",
        &format!(
            r"printf 'Starting\r|\r/\r-\r\033[2KSign in with Google:\r\nhttps://accounts.google.com/o/oauth2/v2/auth?client_id=x&state={long}&scope=openid\r\nPaste the code: '
IFS= read -r _code
sleep 30"
        ),
    );
    let (prompt, output) = prompt_of("agy", &bin, &home).await;
    let Some(Prompt::Page { url, code: None }) = prompt else {
        panic!("no link: {output:?}")
    };
    assert!(
        url.ends_with("&scope=openid") && url.contains(&long),
        "{url}"
    );
}

/// A program that exits without ever printing a link ends the wait at once instead of
/// waiting for the timeout.
#[tokio::test]
async fn a_cli_that_quits_early_gives_no_link() {
    let dir = tempfile::tempdir().unwrap();
    let (bin, home) = (dir.path().join("bin"), dir.path().join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    fake(&bin, "qodercli", "echo 'Error: not today'; exit 3");
    let env = env_with(&bin, &home);
    let pins = pin_installed("qoder", &env).expect("pin");
    let spec = login_spec("qoder").unwrap();
    let mut run = start(spec, &env, Some(&pins)).expect("start");
    let began = std::time::Instant::now();
    assert_eq!(run.next_prompt(spec, Duration::from_secs(20)).await, None);
    assert!(began.elapsed() < Duration::from_secs(5));
    assert_eq!(run.wait_exit(Duration::from_secs(5)).await, Some(Some(3)));
}

/// `vibe --setup`-like menu: arrow keys move, Enter picks, then it asks for a key. Typed through
/// the same terminal the `shell_interactive` tool uses.
#[tokio::test]
async fn a_vibe_like_arrow_key_menu_can_be_driven() {
    use pty_session::{PtySession, SpawnSpec, State};
    let script = r"stty -echo -icanon min 1
printf 'Choose a provider\r\n  > Mistral\r\n    Other\r\n'
k=$(dd bs=3 count=1 2>/dev/null | od -An -c | tr -d ' ')
case $k in
  *033*[B*) printf 'picked: Other\r\n' ;;
  *) printf 'picked: unexpected %s\r\n' $k ;;
esac
dd bs=1 count=1 2>/dev/null >/dev/null
printf 'API key: '
IFS= read -r _key
echo done";
    let session = PtySession::spawn(&SpawnSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: None,
        env: vec![("PATH".into(), std::env::var_os("PATH").unwrap())],
        cols: 120,
        rows: 30,
    })
    .unwrap();
    let wait = |s: &PtySession, want: &'static str| {
        let text = s.text();
        (text.contains(want), text)
    };
    let mut seen = String::new();
    for _ in 0..40 {
        let (found, text) = wait(&session, "Other\n");
        if found {
            seen = text;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen.contains("Choose a provider"), "{seen:?}");
    session.write(b"\x1b[B").unwrap(); // arrow down
    for _ in 0..40 {
        if session.text().contains("picked: Other") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        session.text().contains("picked: Other"),
        "{:?}",
        session.text()
    );
    session.write(b"\r").unwrap();
    for _ in 0..40 {
        if session.text().contains("API key:") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(session.text().contains("API key:"));
    session.write(b"not-a-real-key\r").unwrap();
    let snap = session
        .read(Duration::from_secs(5), Duration::from_millis(300), 8192)
        .await;
    assert!(
        snap.lines.iter().any(|l| l == "done") || matches!(snap.state, State::Exited(_)),
        "{snap:?}"
    );
}

/// The installed vendor CLIs, in an empty home, with no browser: do they print a link on a
/// Linux terminal, and does the parser find it? Never types a code and never signs in.
#[tokio::test]
#[ignore = "starts the vendor CLIs installed on this machine and may reach their servers"]
async fn real_clis_print_their_link_on_a_pty() {
    // The ambient `PATH` (the vendors' launchers need `env bash`, `node`, ...), an empty home.
    for engine in ["cursor", "agy", "opencode", "kiro", "qoder", "kimi"] {
        let home = tempfile::tempdir().unwrap();
        let mut env = PathEnv::from_ambient().with_env_var("HOME", home.path().to_string_lossy());
        env.home_dir = Some(home.path().to_path_buf());
        let Ok(pins) = pin_installed(engine, &env) else {
            eprintln!("{engine}: not installed here, skipped");
            continue;
        };
        let spec = login_spec(engine).unwrap();
        let mut run = start(spec, &env, Some(&pins)).expect("start");
        let prompt = run.next_prompt(spec, Duration::from_secs(40)).await;
        let output = run.private_output();
        run.terminate().await;
        match &prompt {
            Some(Prompt::Page { url, code }) => {
                let shown: String = url.chars().take(60).collect();
                eprintln!("{engine}: link {shown}... code {}", code.is_some());
            }
            None => eprintln!("{engine}: NO LINK; output was {output:?}"),
        }
    }
}
