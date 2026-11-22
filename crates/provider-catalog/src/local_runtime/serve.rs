//! Starts the managed `llama-server` on loopback.
//!
//! The server listens on `127.0.0.1` only (never on a network address), has no web page and no
//! API key because nothing outside this computer can reach it. The child is killed when the
//! handle is dropped, so it never outlives the daemon.

use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

use tokio::process::{Child, Command};

use crate::local_detect::{LocalRuntime, detect_on};

/// The port `local_detect` already probes for a llama.cpp server.
pub const LLAMA_PORT: u16 = 8080;
/// Context window; small enough for 8 GB machines, large enough for chat.
const CONTEXT_TOKENS: u32 = 8192;
/// Loading a multi-GB model from a slow disk can take a while.
const READY_TIMEOUT: Duration = Duration::from_secs(180);

/// A running managed server.
#[derive(Debug)]
pub struct ServerHandle {
    child: Child,
    pub port: u16,
}

impl ServerHandle {
    /// True while the process is alive.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stops the server and waits for it.
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// Command-line arguments (pure, so a test can assert the loopback binding).
#[must_use]
pub fn server_args(model: &Path, port: u16, threads: Option<u32>) -> Vec<String> {
    let mut args = vec![
        "--model".to_owned(),
        model.to_string_lossy().into_owned(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
        "--ctx-size".to_owned(),
        CONTEXT_TOKENS.to_string(),
        "--no-webui".to_owned(),
    ];
    if let Some(t) = threads.filter(|t| *t > 0) {
        args.push("--threads".to_owned());
        args.push(t.to_string());
    }
    args
}

/// Why the server did not come up. `Display` is plain language.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServeError {
    #[error("port {0} is already used by another program")]
    PortBusy(u16),
    #[error("could not start the local model program: {0}")]
    Spawn(String),
    #[error("the local model program stopped while loading the model")]
    Exited,
    #[error("the model did not finish loading in time")]
    NotReady,
    #[error("the installed program was changed or is incomplete; install it again")]
    Untrusted,
}

/// Starts `binary` with `model` and waits until it answers on loopback.
///
/// # Errors
/// See [`ServeError`].
pub async fn start_server(
    binary: &Path,
    model: &Path,
    port: u16,
    threads: Option<u32>,
) -> Result<ServerHandle, ServeError> {
    if tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_err()
    {
        return Err(ServeError::PortBusy(port));
    }
    // Fail closed: re-hash the whole install folder right before the spawn.
    let b = binary.to_path_buf();
    let trusted = tokio::task::spawn_blocking(move || super::trusted::verify_install(&b)).await;
    if !matches!(trusted, Ok(Ok(()))) {
        return Err(ServeError::Untrusted);
    }
    let mut cmd = std::process::Command::new(binary);
    cmd.args(server_args(model, port, threads))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = binary.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut cmd = Command::from(cmd);
    cmd.kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| ServeError::Spawn(e.kind().to_string()))?;
    let mut handle = ServerHandle { child, port };
    let deadline = Instant::now() + READY_TIMEOUT;
    let runtime = [LocalRuntime {
        id: "llamacpp",
        port,
    }];
    loop {
        if !handle.is_running() {
            return Err(ServeError::Exited);
        }
        if !detect_on(&runtime).await.is_empty() {
            return Ok(handle);
        }
        if Instant::now() >= deadline {
            handle.stop().await;
            return Err(ServeError::NotReady);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_only_listens_on_this_computer() {
        let args = server_args(Path::new("m.gguf"), 8080, Some(6));
        let pos = args.iter().position(|a| a == "--host").unwrap();
        assert_eq!(args[pos + 1], "127.0.0.1");
        assert!(!args.iter().any(|a| a == "0.0.0.0" || a == "--api-key"));
        assert!(args.contains(&"--no-webui".to_owned()));
        assert!(server_args(Path::new("m"), 1, Some(0)).len() < args.len());
    }

    #[tokio::test]
    async fn an_unverified_program_is_never_started() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("llama-server");
        std::fs::write(&fake, b"not a program").unwrap();
        let err = start_server(&fake, Path::new("m"), 0, None)
            .await
            .unwrap_err();
        assert_eq!(err, ServeError::Untrusted);
    }

    #[tokio::test]
    async fn a_busy_port_is_reported_not_overtaken() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let err = start_server(Path::new("none"), Path::new("m"), port, None)
            .await
            .unwrap_err();
        assert_eq!(err, ServeError::PortBusy(port));
    }
}
