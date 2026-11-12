//! Spawning the `git` CLI: no shell, hardened config, timeout, output cap.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tool_registry::ToolError;

/// Bytes kept per stream before the rest is read and dropped.
const HARD_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Result of a finished git process.
pub(crate) struct GitOutput {
    pub stdout: String,
    /// Stdout was cut at the capture cap.
    pub truncated: bool,
}

/// Runs git with a hardened configuration.
#[derive(Debug, Clone)]
pub(crate) struct GitRunner {
    bin: String,
    timeout: Duration,
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(mut r: R, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut cut = false;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                if n > room {
                    cut = true;
                }
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    (kept, cut)
}

impl GitRunner {
    pub(crate) fn new(bin: String, timeout: Duration) -> Self {
        Self { bin, timeout }
    }

    /// Neutralise every `filter.<name>.*` driver configured for the repository
    /// (clean/smudge/process filters run arbitrary commands selected via
    /// `.gitattributes`). Returns `-c` arguments overriding each driver.
    async fn filter_overrides(&self, cwd: &Path) -> Result<Vec<String>, ToolError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args([
            "--no-pager",
            "config",
            "--null",
            "--get-regexp",
            r"^filter\.",
        ])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
        let out = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| ToolError::Execution("git config timed out".into()))?
            .map_err(|e| {
                ToolError::Execution(format!("could not start git ({}): {e}", self.bin))
            })?;
        // Exit 1 = no matching keys; anything else (e.g. not a repo) is also
        // treated as "none": the real command will report its own error.
        let text = String::from_utf8_lossy(&out.stdout);
        let mut names: Vec<String> = Vec::new();
        for entry in text.split('\0') {
            let key = entry.split('\n').next().unwrap_or("");
            let Some(rest) = key.strip_prefix("filter.") else {
                continue;
            };
            let Some((name, _var)) = rest.rsplit_once('.') else {
                continue;
            };
            if name.is_empty() || name.contains(['=', '\n', '\0']) {
                return Err(ToolError::PermissionDenied(format!(
                    "refusing to run git: unsupported filter driver name {name:?} in repository config"
                )));
            }
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
        let mut args = Vec::with_capacity(names.len() * 8);
        for n in names {
            for var in ["clean", "smudge", "process"] {
                args.push("-c".to_string());
                args.push(format!("filter.{n}.{var}="));
            }
            args.push("-c".to_string());
            args.push(format!("filter.{n}.required=false"));
        }
        Ok(args)
    }

    /// Run `git <hardened globals> <args>` in `cwd`. `stdin` is written then closed.
    /// A non-zero exit becomes [`ToolError::Execution`] carrying stderr.
    pub(crate) async fn run(
        &self,
        cwd: &Path,
        args: &[String],
        stdin: Option<&str>,
        cap: usize,
    ) -> Result<GitOutput, ToolError> {
        let filter_overrides = self.filter_overrides(cwd).await?;
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--no-pager")
            .arg("--literal-pathspecs")
            .args(["-c", "core.quotepath=false"])
            .args(["-c", "core.fsmonitor=false"])
            .args(["-c", "core.pager=cat"])
            .args(["-c", "commit.gpgSign=false"])
            .args(["-c", "tag.gpgSign=false"])
            .args(["-c", "log.showSignature=false"])
            // A null device cannot become a directory populated by another
            // process, unlike the former predictable temporary directory.
            .args([
                "-c",
                if cfg!(windows) {
                    "core.hooksPath=NUL"
                } else {
                    "core.hooksPath=/dev/null"
                },
            ])
            .args(&filter_overrides)
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            ToolError::Execution(format!("could not start git ({}): {e}", self.bin))
        })?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let bytes = text.as_bytes().to_vec();
            tokio::spawn(async move {
                let _ = pipe.write_all(&bytes).await;
                let _ = pipe.shutdown().await;
            });
        }
        let cap = cap.min(HARD_CAPTURE_BYTES);
        let out_task = tokio::spawn(drain(child.stdout.take().expect("piped stdout"), cap));
        let err_task = tokio::spawn(drain(child.stderr.take().expect("piped stderr"), 64 * 1024));
        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(ToolError::Execution(format!("waiting for git failed: {e}"))),
            Err(_) => {
                let _ = child.kill().await;
                return Err(ToolError::Execution(format!(
                    "git timed out after {}s",
                    self.timeout.as_secs()
                )));
            }
        };
        let (stdout, truncated) = out_task.await.unwrap_or_default();
        let (stderr, _) = err_task.await.unwrap_or_default();
        if !status.success() {
            // Some failures (e.g. "nothing to commit") are reported on stdout.
            let mut msg = String::from_utf8_lossy(&stderr).trim().to_string();
            let out_msg = String::from_utf8_lossy(&stdout);
            if !out_msg.trim().is_empty() {
                if !msg.is_empty() {
                    msg.push_str("; ");
                }
                msg.push_str(out_msg.trim());
            }
            let code = status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            return Err(ToolError::Execution(format!(
                "git exited with {code}: {msg}"
            )));
        }
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            truncated,
        })
    }
}
