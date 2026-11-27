//! `bot browser install`: a checked Chrome for Testing in the data folder, for a computer
//! (usually a Linux server) that has no browser the bot can use.

use std::{
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use daemon_api::browser_install::{self, InstallError};

#[derive(clap::Subcommand)]
pub enum BrowserAction {
    /// Download a checksum-verified Chrome for Testing into the data folder and start it once.
    ///
    /// For a Linux computer without a usable Chrome or Chromium (for example Ubuntu, where
    /// the only Chromium is the snap one the service cannot run). The archive comes from
    /// storage.googleapis.com over https and is pinned by SHA-256; nothing is unpacked
    /// before it matches. Needs about 1 GB of free disk space and no root. Run it as the
    /// service's user (`sudo praxi browser install` on a host set up with install-linux.sh).
    Install,
}

pub fn run(
    action: &BrowserAction,
    data_dir: &Path,
    json: bool,
    out: &mut dyn Write,
) -> crate::Result<()> {
    match action {
        BrowserAction::Install => install(data_dir, json, out),
    }
}

fn install(data_dir: &Path, json: bool, out: &mut dyn Write) -> crate::Result<()> {
    let total = browser_install::asset_for(std::env::consts::OS, std::env::consts::ARCH)
        .map_or(0, |a| a.size_bytes);
    // Progress goes to stderr, once per 10 %, so `--json` output stays clean.
    let last = AtomicU64::new(u64::MAX);
    let progress = move |done: u64| {
        let step = done.saturating_mul(10).checked_div(total).unwrap_or(0);
        if last.swap(step, Ordering::Relaxed) != step {
            eprintln!("downloading the browser: {}%", step * 10);
        }
    };
    let result = crate::current_thread_runtime()?.block_on(browser_install::install(
        data_dir,
        std::env::consts::OS,
        std::env::consts::ARCH,
        &browser_install::System,
        &progress,
    ));
    match result {
        Ok(done) => {
            if json {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "installed": true,
                        "path": done.binary,
                        "already_there": done.already_there,
                    })
                )?;
            } else if done.already_there {
                writeln!(
                    out,
                    "A verified browser was already installed: {}",
                    done.binary.display()
                )?;
            } else {
                writeln!(
                    out,
                    "Browser installed and started once: {}",
                    done.binary.display()
                )?;
            }
            Ok(())
        }
        Err(e @ InstallError::Busy) => Err(crate::client::fail(
            crate::client::exit::REJECTED,
            e.to_string(),
        )),
        Err(e) => Err(crate::client::fail(
            crate::client::exit::GENERIC,
            e.to_string(),
        )),
    }
}
