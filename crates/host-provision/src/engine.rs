//! The step engine: connect, preflight, install, pair, health.
//!
//! Every step is idempotent. A failed run stops at the failing step and a
//! re-run starts from the top: steps that are already satisfied on the server
//! (installed, paired) report `skip`, so "resume" is just running it again.
use std::{cmp::Ordering, time::Duration};

use serde::Deserialize;

use crate::{
    Error, facts,
    facts::{Existing, Facts},
    known_hosts::{HostKeyState, KnownHosts},
    remote,
    result::{ProvisionLog, Report, Status, StepResult},
    store::{HostRecord, Store},
    transport::{Output, RemoteCmd, StageFile, Sub, Transport, TransportError, sanitize},
};

/// What the caller wants done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// New server (also resumes a half-done add). Refuses to touch an
    /// existing plain `bot serve` install: that is `Repair`.
    Add,
    /// Re-run install, migrating a plain install to host mode in place; re-pair if needed.
    Repair,
    /// Install the app's version over an older one.
    Upgrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveLevel {
    /// Forget the server and revoke this computer on it; leave Praxi installed.
    Disconnect,
    Uninstall,
    /// Uninstall and delete the data directory, `/etc/praxi` and the user.
    Purge,
}

pub struct PairedHost {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct HostHealth {
    pub mode: String,
    pub version: String,
    pub has_display: bool,
    pub has_browser: bool,
    /// `None`: the host has no live-view route (older than PR #107).
    pub view_targets: Option<usize>,
}

/// This computer's side: the local daemon's host-link API (PR #111).
pub trait LocalHost {
    /// Consume a pairing ticket (role desktop) created on the server.
    fn pair(&self, ticket: &str) -> Result<PairedHost, String>;
    /// Is this host id currently paired here?
    fn knows_host(&self, id: &str) -> bool;
    /// Ask the host over the paired channel.
    fn health(&self, id: &str) -> Result<HostHealth, String>;
    fn revoke_host(&self, id: &str) -> Result<(), String>;
    /// Host ids paired on this computer right now (to tell a new pairing from an old one).
    fn host_ids(&self) -> Vec<String>;
    /// Remove a pairing from this computer (rollback of a pairing this run made).
    fn forget_host(&self, id: &str) -> Result<(), String>;
}

pub trait Ui {
    fn progress(&mut self, result: &StepResult);
    fn note(&mut self, line: &str);
    /// First contact with a server: show the fingerprint, return true to pin it.
    fn confirm_host_key(&mut self, host: &str, port: u16, fingerprint: &str) -> bool;
    /// The user asked to stop: no new pairing authority may be created and
    /// what this run already created is rolled back.
    fn cancelled(&self) -> bool {
        false
    }
}

/// What a pairing created that did not exist before this run.
#[derive(Debug, Default, Clone)]
pub struct Created {
    /// Desktop keys the server listed before the invite (never touched).
    before: Vec<String>,
    /// This computer's new pairing with the host, if it did not exist before.
    local_host: Option<String>,
    /// This computer's new device key on the server.
    server_key: Option<String>,
    /// The key was granted screen view + input.
    screen: bool,
}

impl Created {
    fn any(&self) -> bool {
        self.local_host.is_some() || self.server_key.is_some()
    }
}

pub struct Params {
    pub record: HostRecord,
    pub mode: Mode,
    /// The desktop app's own version without a leading `v` (what the host must run).
    pub app_version: String,
    /// The release tag this app was built from (`v0.1.0-alpha.1`), when the build knows it. A
    /// pre-release tag is not `v<app_version>`: the server must fetch the tag that really exists.
    pub release_tag: Option<String>,
    /// Install the virtual display and desktop session too.
    pub desktop: bool,
    /// Pause between health attempts right after a (re)start.
    pub retry_delay: Duration,
    /// May the engine download and install Praxi on the server? That needs a
    /// published release (the server fetches `v<app_version>` itself). When
    /// false the install step only DETECTS an existing host-mode install and
    /// the flow goes on to pair; anything else stops with a plain message.
    pub allow_install: bool,
}

pub struct Engine<'a> {
    pub transport: &'a dyn Transport,
    pub local: &'a dyn LocalHost,
    pub ui: &'a mut dyn Ui,
    pub store: &'a Store,
    pub known_hosts: &'a KnownHosts,
    pub params: Params,
}

const SHORT: Duration = Duration::from_secs(40);
const LONG: Duration = Duration::from_secs(1800);

#[derive(Deserialize)]
struct InviteOut {
    ticket: String,
}

fn fail_from(id: &'static str, title: &str, e: &TransportError) -> StepResult {
    let hint = match e {
        TransportError::Auth => {
            "Check the user name (Ubuntu images: ubuntu, Oracle Linux: opc) and pass --key <private key file>, or load the key into ssh-agent (`ssh-add`). Passwords are not supported yet."
        }
        TransportError::Network(_) | TransportError::Timeout => {
            "Check the address and port, that the server is running, and that the cloud firewall allows inbound SSH (port 22)."
        }
        TransportError::NoHostKey(_) => {
            "The server answered but Praxi could not read its identity. Update the OpenSSH client on this computer, or check the server's SSH settings."
        }
        TransportError::HostKeyChanged => {
            "The server's identity changed. If you did not reinstall it, do not continue."
        }
        TransportError::Other(_) => "See the log for details.",
    };
    StepResult::fail(id, title, e.to_string(), hint)
}

/// Install step without permission to install: pair with what is there.
/// The GitHub release tag the server downloads: the tag the app was built from, else `v<version>`.
fn install_tag(release_tag: Option<&str>, app: &str) -> Result<String, crate::Error> {
    crate::validate::version_tag(release_tag.filter(|t| !t.is_empty()).unwrap_or(app))
}

fn detect_only(existing: &Existing, app: &str) -> StepResult {
    const T: &str = "Install Praxi";
    match existing {
        Existing::Installed {
            version,
            host_mode: true,
            active: true,
            ..
        } if facts::semver(version) == facts::semver(app) => StepResult::skip(
            "install",
            T,
            format!("Praxi {version} already installed in host mode"),
        ),
        Existing::Installed {
            version,
            host_mode: true,
            active: true,
            ..
        } => StepResult::warn(
            "install",
            T,
            format!(
                "Praxi {version} is installed in host mode (this app is {app}); nothing was changed"
            ),
            "Install was not needed. If the versions differ a lot, update the one that is older.",
        ),
        Existing::Installed {
            host_mode: true,
            active: false,
            ..
        } => StepResult::fail(
            "install",
            T,
            "Praxi is installed in host mode but its service is not running",
            "On the server run: sudo systemctl start praxi",
        ),
        Existing::Installed {
            host_mode: false, ..
        } => StepResult::fail(
            "install",
            T,
            "Praxi on this server is not running in host mode",
            "Switching it to host mode needs a published Praxi release, which does not exist yet.",
        ),
        Existing::Fresh => StepResult::fail(
            "install",
            T,
            "Praxi is not installed on this server, and automatic install needs a published release",
            "No release has been published yet. Install Praxi on the server by hand, then add it again.",
        ),
    }
}

impl Engine<'_> {
    fn rec(&self) -> &HostRecord {
        &self.params.record
    }

    fn log(&self) -> ProvisionLog {
        ProvisionLog::new(Some(self.store.log_path(&self.rec().name)))
    }

    fn emit(&mut self, report: &mut Report, r: StepResult) -> bool {
        self.log().line(
            r.id,
            &format!("{} {}: {}", r.status.label(), r.title, r.detail),
        );
        self.ui.progress(&r);
        let cont = r.status != Status::Fail && !self.ui.cancelled();
        report.results.push(r);
        cont
    }

    /// Run a constant remote command; non-zero exit becomes a failed step.
    fn exec(
        &self,
        id: &'static str,
        title: &str,
        cmd: RemoteCmd,
        stdin: &[u8],
        timeout: Duration,
        log_stdout: bool,
    ) -> Result<Output, StepResult> {
        let log = self.log();
        let out = self
            .transport
            .run(cmd, stdin, timeout)
            .map_err(|e| fail_from(id, title, &e))?;
        log.line(id, &format!("exit {}", out.code));
        if log_stdout {
            log.line(id, &out.stdout);
        }
        log.line(id, &out.stderr);
        if out.ok() {
            Ok(out)
        } else {
            let tail: Vec<&str> = out.stderr.lines().rev().take(4).collect();
            let tail = tail.into_iter().rev().collect::<Vec<_>>().join(" | ");
            Err(StepResult::fail(
                id,
                title,
                format!(
                    "exit {}: {}",
                    out.code,
                    sanitize(&crate::result::redact(&tail), 400)
                ),
                "Re-running is safe. The full redacted log is in the host's provision.log.",
            ))
        }
    }

    fn upload(
        &self,
        id: &'static str,
        title: &str,
        file: StageFile,
        bytes: &str,
    ) -> Result<(), StepResult> {
        self.transport
            .upload(file, bytes.as_bytes())
            .map_err(|e| fail_from(id, title, &e))
    }

    /// Run the whole flow for `params.mode`.
    pub fn run(&mut self) -> Report {
        let mut report = Report::default();
        let mut created = None;
        let _ = self.flow(&mut report, &mut created);
        if let Some(c) = created.take()
            && self.ui.cancelled()
        {
            let note = self.rollback(&c);
            self.emit(
                &mut report,
                StepResult::warn(
                    "pair.rollback",
                    "Undo the pairing",
                    "setup was cancelled after pairing; the new pairing was undone",
                    &note,
                ),
            );
        }
        // Best effort; the staging dir holds only the scripts.
        let _ = self
            .transport
            .run(RemoteCmd::Cleanup, b"", Duration::from_secs(15));
        report
    }

    fn flow(&mut self, report: &mut Report, created: &mut Option<Created>) -> Option<()> {
        self.step_hostkey(report)?;
        self.step_connect(report)?;
        let facts = self.step_preflight(report)?;
        self.step_install(report, &facts)?;
        self.step_pair(report, created)?;
        self.step_health(report);
        Some(())
    }

    // ---- 1. host key --------------------------------------------------

    fn step_hostkey(&mut self, report: &mut Report) -> Option<()> {
        const T: &str = "Server identity (host key)";
        let (host, port) = (self.rec().host.clone(), self.rec().port);
        let scanned = match self.transport.scan_host_keys() {
            Ok(k) => k,
            Err(e) => return self.finish(report, fail_from("hostkey", T, &e)),
        };
        let state = match self.known_hosts.check(&host, port, &scanned) {
            Ok(s) => s,
            Err(e) => {
                return self.finish(
                    report,
                    StepResult::fail(
                        "hostkey",
                        T,
                        e.to_string(),
                        "The server offered no usable host key.",
                    ),
                );
            }
        };
        let r = match state {
            HostKeyState::Trusted { fingerprint } => StepResult::pass(
                "hostkey",
                T,
                format!("matches the pinned key {fingerprint}"),
            ),
            HostKeyState::Mismatch { pinned, seen } => {
                report.hard_stop = true;
                StepResult::fail(
                    "hostkey",
                    T,
                    format!("MISMATCH: pinned {pinned}, server now presents {seen}"),
                    "This is not the server you set up (or it was reinstalled). Nothing was sent. If you rebuilt the server on purpose, run `bot host remove <name>` and add it again.",
                )
            }
            HostKeyState::Unknown { key } => {
                let fp = key.fingerprint();
                if self.ui.confirm_host_key(&host, port, &fp) {
                    match self.known_hosts.pin(&host, port, &key) {
                        Ok(()) => {
                            self.params.record.fingerprint = Some(fp.clone());
                            StepResult::pass("hostkey", T, format!("first contact, pinned {fp}"))
                        }
                        Err(e) => StepResult::fail(
                            "hostkey",
                            T,
                            e.to_string(),
                            "Could not write Praxi's known_hosts file.",
                        ),
                    }
                } else {
                    let why = if self.ui.cancelled() {
                        "setup was cancelled"
                    } else {
                        "not accepted"
                    };
                    StepResult::fail(
                        "hostkey",
                        T,
                        format!("{why} ({fp})"),
                        "Compare the fingerprint with your provider's console, then run again and accept (or pass --yes).",
                    )
                }
            }
        };
        self.finish(report, r)
    }

    /// Push a result; `Some(())` when the flow may continue.
    fn finish(&mut self, report: &mut Report, r: StepResult) -> Option<()> {
        self.emit(report, r).then_some(())
    }

    // ---- 2. connect ---------------------------------------------------

    fn step_connect(&mut self, report: &mut Report) -> Option<()> {
        const T: &str = "SSH login";
        let r = match self.exec("connect", T, RemoteCmd::Probe, b"", SHORT, true) {
            Ok(o) if o.stdout.trim() == "praxi-ok" => {
                let user = self.rec().user.clone();
                StepResult::pass("connect", T, format!("logged in as {user}"))
            }
            Ok(_) => StepResult::fail(
                "connect",
                T,
                "unexpected answer from the server",
                "The login shell must be POSIX-compatible.",
            ),
            Err(r) => {
                if r.detail.contains("host key") {
                    report.hard_stop = true;
                }
                r
            }
        };
        self.finish(report, r)
    }

    // ---- 3. preflight -------------------------------------------------

    fn step_preflight(&mut self, report: &mut Report) -> Option<Facts> {
        const T: &str = "Preflight";
        let staged = self.upload(
            "preflight",
            T,
            StageFile::Provision,
            &remote::provision_sh(),
        );
        let out = staged.and_then(|()| {
            self.exec(
                "preflight",
                T,
                RemoteCmd::Provision(Sub::Preflight),
                b"",
                SHORT * 3,
                true,
            )
        });
        let out = match out {
            Ok(o) => o,
            Err(r) => {
                self.finish(report, r)?;
                return None;
            }
        };
        let facts = Facts::parse(&out.stdout);
        let mut ok = true;
        for r in facts::evaluate(&facts) {
            ok &= self.emit(report, r);
        }
        ok.then_some(facts)
    }

    // ---- 4. install ---------------------------------------------------

    fn step_install(&mut self, report: &mut Report, facts: &Facts) -> Option<()> {
        const T: &str = "Install Praxi";
        let app = self.params.app_version.clone();
        let existing = facts::existing(facts);
        let mode = self.params.mode;
        let name = self.rec().name.clone();

        if !self.params.allow_install {
            let r = detect_only(&existing, &app);
            return self.finish(report, r);
        }

        let app_tag = install_tag(self.params.release_tag.as_deref(), &app).ok();
        let (run, snapshot) = match (&existing, mode) {
            (Existing::Installed { version, build, .. }, _)
                if facts::semver(version) > facts::semver(&app)
                    || build
                        .as_deref()
                        .zip(app_tag.as_deref())
                        .and_then(|(b, a)| facts::release_cmp(b, a))
                        == Some(Ordering::Greater) =>
            {
                let r = StepResult::warn(
                    "install",
                    T,
                    format!("server runs {version}, newer than this app ({app}); not downgrading"),
                    "Update the desktop app, or reinstall the server explicitly.",
                );
                return self.finish(report, r);
            }
            (Existing::Fresh, Mode::Upgrade) => {
                let r = StepResult::fail(
                    "install",
                    T,
                    "nothing is installed on this server",
                    "Use `bot host add` for a new server.",
                );
                return self.finish(report, r);
            }
            (
                Existing::Installed {
                    host_mode: false, ..
                },
                Mode::Add,
            ) => {
                let r = StepResult::fail(
                    "install",
                    T,
                    "this server already runs Praxi in plain `bot serve` mode",
                    "Migrate it in place (keeps its data) with `bot host repair <user@server>`.",
                );
                return self.finish(report, r);
            }
            // An update skips the install only when the server reports exactly the tag this app
            // attests: equal cargo versions (`0.1.0`) say nothing about which alpha is running.
            (
                Existing::Installed {
                    version,
                    build,
                    host_mode: true,
                    active: true,
                },
                Mode::Upgrade,
            ) if build.as_deref().map(|b| b.strip_prefix('v').unwrap_or(b))
                == app_tag.as_deref().map(|t| t.strip_prefix('v').unwrap_or(t)) =>
            {
                let r = StepResult::skip(
                    "install",
                    T,
                    format!("Praxi {version} already installed in host mode"),
                );
                return self.finish(report, r);
            }
            (
                Existing::Installed {
                    version,
                    host_mode: true,
                    active: true,
                    ..
                },
                Mode::Add,
            ) if facts::semver(version) == facts::semver(&app) => {
                let r = StepResult::skip(
                    "install",
                    T,
                    format!("Praxi {version} already installed in host mode"),
                );
                return self.finish(report, r);
            }
            (Existing::Fresh, _) => (true, facts.flag("data_dir")),
            (Existing::Installed { .. }, _) => (true, facts.flag("data_dir")),
        };
        if !run {
            return Some(());
        }

        let tag = match install_tag(self.params.release_tag.as_deref(), &app) {
            Ok(tag) => tag,
            Err(e) => {
                let r = StepResult::fail(
                    "install",
                    T,
                    format!("cannot tell which release to install: {e}"),
                    "Update the desktop app to a published release.",
                );
                return self.finish(report, r);
            }
        };
        self.ui.note(&format!(
            "installing {tag} on {name} (this takes a few minutes)"
        ));
        let steps = |s: &mut Self, report: &mut Report| -> Option<()> {
            let r = s
                .upload(
                    "install.stage",
                    T,
                    StageFile::Installer,
                    &remote::installer_sh(),
                )
                .and_then(|()| {
                    s.upload(
                        "install.stage",
                        T,
                        StageFile::SetupHeadless,
                        &remote::setup_headless_sh(),
                    )
                })
                .map(|()| {
                    StepResult::pass(
                        "install.stage",
                        "Upload install scripts",
                        "2 scripts staged (mode 0700 dir)",
                    )
                })
                .unwrap_or_else(|r| r);
            s.finish(report, r)?;
            if snapshot {
                let r = s
                    .exec(
                        "install.snapshot",
                        "Snapshot existing data",
                        RemoteCmd::Provision(Sub::Snapshot),
                        b"",
                        Duration::from_secs(900),
                        true,
                    )
                    .map(|o| {
                        let line = sanitize(
                            o.stdout
                                .lines()
                                .chain(o.stderr.lines())
                                .find(|l| l.contains("snapshot:"))
                                .unwrap_or("done")
                                .trim(),
                            200,
                        );
                        StepResult::pass("install.snapshot", "Snapshot existing data", line)
                    })
                    .unwrap_or_else(|r| r);
                s.finish(report, r)?;
            }
            let r = s
                .exec(
                    "install.gh",
                    "GitHub CLI (release verification)",
                    RemoteCmd::Provision(Sub::InstallGh),
                    b"",
                    Duration::from_secs(900),
                    true,
                )
                .map(|_| {
                    StepResult::pass(
                        "install.gh",
                        "GitHub CLI (release verification)",
                        ">= 2.80 present",
                    )
                })
                .unwrap_or_else(|r| r);
            s.finish(report, r)?;
            let mut params = format!("version={tag}\ndesktop={}\n", u8::from(s.params.desktop));
            if mode == Mode::Upgrade {
                params.push_str("update=1\n");
            }
            let r = s
                .exec(
                    "install.run",
                    T,
                    RemoteCmd::Provision(Sub::Install),
                    params.as_bytes(),
                    LONG,
                    true,
                )
                .map(|_| {
                    StepResult::pass(
                        "install.run",
                        T,
                        format!(
                            "Praxi {tag} installed in host mode (127.0.0.1 only, no inbound port)"
                        ),
                    )
                })
                .unwrap_or_else(|r| r);
            s.finish(report, r)
        };
        steps(self, report)?;
        self.params.record.installed_version = Some(app);
        let _ = self.store.save(self.rec());
        Some(())
    }

    // ---- 5. pair ------------------------------------------------------

    fn server_desktops(&self, id: &'static str, title: &str) -> Result<Vec<String>, StepResult> {
        let out = self.exec(
            id,
            title,
            RemoteCmd::Provision(Sub::Devices),
            b"",
            SHORT,
            false,
        )?;
        let v: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|_| {
            StepResult::fail(
                id,
                title,
                "the host answered with something unexpected",
                "Is the service running? Try `bot host repair`.",
            )
        })?;
        let peers = v.get("peers").unwrap_or(&v);
        Ok(peers
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["role"] == "desktop")
            .filter_map(|p| p["key"].as_str().map(str::to_owned))
            .collect())
    }

    fn step_pair(&mut self, report: &mut Report, created: &mut Option<Created>) -> Option<()> {
        const T: &str = "Pair this computer";
        // Already paired here and the server still lists our key: nothing to do.
        if let (Some(host_id), Some(key)) = (
            self.rec().paired_host_id.clone(),
            self.rec().desktop_key.clone(),
        ) && self.local.knows_host(&host_id)
            && self
                .server_desktops("pair", T)
                .is_ok_and(|keys| keys.contains(&key))
        {
            let r = StepResult::skip("pair", T, "already paired");
            return self.finish(report, r);
        }
        let r = self.pair_now(T);
        match r {
            Ok((detail, c)) => {
                *created = Some(c);
                self.finish(report, StepResult::pass("pair", T, detail))
            }
            Err(r) => {
                self.finish(report, r)?;
                None
            }
        }
    }

    /// Pairing is a transaction: whatever this run created (the server's new
    /// device entry, its screen grant, this computer's new peer) is undone
    /// when a later part fails or the user cancels. Pairings that existed
    /// before are never touched. The invite ticket exists only in memory and
    /// is dropped; with no invite-revoke command on the host it simply
    /// expires on its own.
    fn pair_now(&mut self, title: &str) -> Result<(String, Created), StepResult> {
        let mut txn = Created::default();
        match self.pair_txn(title, &mut txn) {
            Ok(detail) => Ok((detail, txn)),
            Err(mut r) => {
                if txn.any() {
                    let note = self.rollback(&txn);
                    r.remediation = Some(match r.remediation.take() {
                        Some(h) => format!("{h} {note}"),
                        None => note,
                    });
                }
                Err(r)
            }
        }
    }

    /// Undo `c`; returns one plain sentence saying what was undone and what,
    /// if anything, is left (with the command to remove it).
    fn rollback(&mut self, c: &Created) -> String {
        let mut notes: Vec<String> = Vec::new();
        let mut key = c.server_key.clone();
        if key.is_none() && c.local_host.is_some() {
            // The key was not identified yet: take it from the server's list only if it is unambiguous.
            if let Ok(after) = self.server_desktops("pair", "Undo the pairing") {
                let new: Vec<&String> = after.iter().filter(|k| !c.before.contains(k)).collect();
                if let [only] = new.as_slice() {
                    key = Some((*only).clone());
                }
            }
        }
        match &key {
            Some(k) => {
                let params = format!("key={k}\n");
                let run = |s: &Self, sub: Sub| {
                    s.transport
                        .run(RemoteCmd::Provision(sub), params.as_bytes(), SHORT)
                        .is_ok_and(|o| o.ok())
                };
                let screen_ok = !c.screen || run(self, Sub::ScreenRevoke);
                let revoked = run(self, Sub::Revoke);
                if revoked && screen_ok {
                    notes.push("This computer's device entry and screen access were removed from the server.".into());
                } else {
                    notes.push(format!(
                        "The server could not be reached to remove this computer: on the server run `bot host screen {k} --revoke` and `bot host revoke {k}` (data dir /var/lib/praxi)."
                    ));
                }
            }
            None if c.local_host.is_some() => notes.push(
                "This computer's device entry on the server could not be identified: on the server run `bot host devices`, then `bot host revoke <key>` for the newest desktop.".into(),
            ),
            None => {}
        }
        if let Some(id) = &c.local_host {
            match self.local.forget_host(id) {
                Ok(()) => notes.push("The pairing on this computer was removed.".into()),
                Err(e) => notes.push(format!(
                    "The pairing on this computer could not be removed ({}).",
                    sanitize(&e, 120)
                )),
            }
        }
        notes.join(" ")
    }

    fn pair_txn(&mut self, title: &str, txn: &mut Created) -> Result<String, StepResult> {
        let fail = |detail: String, hint: &str| StepResult::fail("pair", title, detail, hint);
        let cancelled = |ui: &dyn Ui| {
            ui.cancelled().then(|| {
                StepResult::fail(
                    "pair",
                    title,
                    "cancelled before pairing",
                    "Nothing was paired.",
                )
            })
        };
        if let Some(r) = cancelled(&*self.ui) {
            return Err(r);
        }
        let before = self.server_desktops("pair", title)?;
        txn.before.clone_from(&before);
        let known_before = self.local.host_ids();
        let out = self.exec(
            "pair",
            title,
            RemoteCmd::Provision(Sub::Invite),
            b"",
            SHORT,
            false,
        )?;
        // The ticket is single use and short lived: it goes straight to the
        // local daemon and is never written to disk or the log.
        let invite: InviteOut = serde_json::from_str(out.stdout.trim()).map_err(|_| {
            fail(
                "the host did not return an invite".into(),
                "Is the service running in host mode? Try `bot host repair`.",
            )
        })?;
        if let Some(r) = cancelled(&*self.ui) {
            return Err(r);
        }
        let paired = self.local.pair(&invite.ticket).map_err(|e| {
            fail(
                format!("this computer could not pair: {}", sanitize(&e, 200)),
                "Is the desktop daemon running here (`bot serve` or the app)? The ticket lives 2 minutes; just run the command again.",
            )
        })?;
        if !known_before.contains(&paired.id) {
            txn.local_host = Some(paired.id.clone());
        }
        let after = self.server_desktops("pair", title)?;
        let new: Vec<&String> = after.iter().filter(|k| !before.contains(k)).collect();
        if new.is_empty() && known_before.contains(&paired.id) {
            // This computer was already paired with this host (for example by
            // ticket): the invite just confirmed it. No second device entry,
            // nothing created, nothing to roll back, privileges left as they are.
            self.params.record.paired_host_id = Some(paired.id.clone());
            self.store
                .save(self.rec())
                .map_err(|e| fail(e.to_string(), "Could not save the local server record."))?;
            return Ok(format!(
                "already connected to {}; nothing changed",
                sanitize(&paired.name, 60)
            ));
        }
        let [key] = new.as_slice() else {
            return Err(fail(
                "paired, but this computer's key could not be identified on the host".into(),
                "Run `bot host devices` on the server, then `bot host screen <key>` for this desktop.",
            ));
        };
        let key = (*key).clone();
        txn.server_key = Some(key.clone());
        if let Some(r) = cancelled(&*self.ui) {
            return Err(r);
        }
        let params = format!("key={key}\n");
        txn.screen = true;
        self.exec(
            "pair",
            title,
            RemoteCmd::Provision(Sub::Screen),
            params.as_bytes(),
            SHORT,
            true,
        )?;
        self.params.record.paired_host_id = Some(paired.id.clone());
        self.params.record.desktop_key = Some(key);
        self.store
            .save(self.rec())
            .map_err(|e| fail(e.to_string(), "Could not save the local server record."))?;
        Ok(format!(
            "paired with {} and granted screen view + input",
            sanitize(&paired.name, 60)
        ))
    }

    // ---- 6. health ----------------------------------------------------

    fn step_health(&mut self, report: &mut Report) {
        const T: &str = "Health";
        let app = self.params.app_version.clone();
        let st = self.exec(
            "health.service",
            T,
            RemoteCmd::Provision(Sub::Status),
            b"",
            SHORT,
            true,
        );
        let Ok(st) = st else {
            self.emit(report, st.unwrap_err());
            return;
        };
        let f = Facts::parse(&st.stdout);
        let svc_ok = f.service_active() && f.flag("unit_hostmode");
        self.emit(
            report,
            if svc_ok {
                StepResult::pass("health.service", "Service running in host mode", "praxi.service active, `bot serve --host`")
            } else {
                StepResult::fail("health.service", "Service running in host mode", format!("active={} host_mode={}", f.get("unit_active"), f.flag("unit_hostmode")), "Run `bot host repair <user@server>`; check `journalctl -u praxi` on the server.")
            },
        );
        if self.params.desktop {
            let up = f.get("xvfb") == "active" && f.get("desktop") == "active";
            self.emit(
                report,
                if up {
                    StepResult::pass("health.desktop", "Virtual display and desktop session", "praxi-xvfb and praxi-desktop active")
                } else {
                    StepResult::warn("health.desktop", "Virtual display and desktop session", format!("xvfb={} desktop={}", f.get("xvfb"), f.get("desktop")), "Run `bot host repair`; the host works without a screen but live view will be empty.")
                },
            );
            self.emit(
                report,
                if f.flag("browser") {
                    StepResult::pass("health.browser", "Browser for the bot's computer", "a non-snap Chrome/Chromium is installed")
                } else {
                    StepResult::warn("health.browser", "Browser for the bot's computer", "no usable browser found on the host", "Run `bot host repair <user@server>` to retry the browser download (the server must reach storage.googleapis.com). Until then the bot's browser tools report that no browser is installed.")
                },
            );
        }
        let Some(id) = self.rec().paired_host_id.clone() else {
            return;
        };
        let mut health = Err(String::new());
        for attempt in 0..6 {
            health = self.local.health(&id);
            if health.is_ok() {
                break;
            }
            if attempt < 5 {
                std::thread::sleep(self.params.retry_delay);
            }
        }
        let h = match health {
            Ok(h) => h,
            Err(e) => {
                self.emit(report, StepResult::fail("health.channel", "Paired channel", sanitize(&e, 200), "The host is installed but this computer cannot reach it; is the local daemon running? Try again in a minute."));
                return;
            }
        };
        self.emit(
            report,
            StepResult::pass(
                "health.channel",
                "Paired channel",
                "answers over the encrypted peer link",
            ),
        );
        self.emit(
            report,
            if h.mode == "host" {
                StepResult::pass("health.mode", "Host mode", "host")
            } else {
                StepResult::fail(
                    "health.mode",
                    "Host mode",
                    format!("mode is '{}'", h.mode),
                    "Run `bot host repair <user@server>`.",
                )
            },
        );
        let same = facts::semver(&h.version) == facts::semver(&app);
        self.emit(
            report,
            if same {
                StepResult::pass(
                    "health.version",
                    "Version matches this app",
                    h.version.clone(),
                )
            } else {
                StepResult::fail(
                    "health.version",
                    "Version matches this app",
                    format!("host {} vs app {app}", h.version),
                    "Run `bot host upgrade <user@server>`.",
                )
            },
        );
        if self.params.desktop {
            self.emit(
                report,
                if h.has_display {
                    StepResult::pass(
                        "health.display",
                        "Display detected by the host",
                        if h.has_browser {
                            "display and browser"
                        } else {
                            "display"
                        },
                    )
                } else {
                    StepResult::warn(
                        "health.display",
                        "Display detected by the host",
                        "no display",
                        "Live view needs the virtual display; run `bot host repair`.",
                    )
                },
            );
        }
        let r = match h.view_targets {
            Some(n) if n > 0 => StepResult::pass(
                "health.live_view",
                "Live-view target",
                format!("{n} available"),
            ),
            Some(_) => StepResult::warn(
                "health.live_view",
                "Live-view target",
                "none yet",
                "Targets appear when a display or browser session is up.",
            ),
            None => StepResult::warn(
                "health.live_view",
                "Live-view target",
                "needs #107 (the host has no live-view route)",
                "Upgrade the host to a build with live view.",
            ),
        };
        self.emit(report, r);
        // A host with no model key reports setup_required: expected here.
    }

    // ---- remove -------------------------------------------------------

    /// Forget a server (and optionally uninstall/purge it). The server record
    /// is deleted only when every requested step succeeded.
    pub fn remove(&mut self, level: RemoveLevel) -> Report {
        let mut report = Report::default();
        let ok = self.remove_flow(&mut report, level);
        let _ = self
            .transport
            .run(RemoteCmd::Cleanup, b"", Duration::from_secs(15));
        if ok.is_some() {
            let rec = self.rec().clone();
            let _ = self.known_hosts.forget(&rec.host, rec.port);
            let _ = self.store.delete(&rec.name);
        }
        report
    }

    fn remove_flow(&mut self, report: &mut Report, level: RemoveLevel) -> Option<()> {
        const T: &str = "Remove server";
        let need_server = level != RemoveLevel::Disconnect;
        let reachable = self.step_hostkey_quiet().is_ok()
            && self
                .exec(
                    "remove.connect",
                    "SSH login",
                    RemoteCmd::Probe,
                    b"",
                    SHORT,
                    false,
                )
                .is_ok();
        if need_server && !reachable {
            self.emit(report, StepResult::fail("remove.connect", "SSH login", "the server cannot be reached", "Uninstall needs the server; nothing was changed. Use a plain `bot host remove` to only forget it."));
            return None;
        }
        if reachable {
            // The helper is needed by every remote removal step, paired or not
            // (the staging dir is deleted at the end of every run).
            let staged = self.upload(
                "remove.stage",
                T,
                StageFile::Provision,
                &remote::provision_sh(),
            );
            if let Err(r) = staged {
                if need_server {
                    self.emit(report, r);
                    return None;
                }
                self.emit(
                    report,
                    StepResult::warn(
                        "remove.stage",
                        T,
                        r.detail,
                        "Could not stage the helper; only forgetting the server locally.",
                    ),
                );
            }
            if let Some(key) = self.rec().desktop_key.clone() {
                let params = format!("key={key}\n");
                let r = self.exec(
                    "remove.revoke",
                    "Revoke this computer on the server",
                    RemoteCmd::Provision(Sub::Revoke),
                    params.as_bytes(),
                    SHORT,
                    false,
                );
                self.emit(
                    report,
                    r.map_or_else(
                        |r| {
                            StepResult::warn(
                                "remove.revoke",
                                "Revoke this computer on the server",
                                r.detail,
                                "Revoke it later with `bot host revoke` on the server.",
                            )
                        },
                        |_| {
                            StepResult::pass(
                                "remove.revoke",
                                "Revoke this computer on the server",
                                "done",
                            )
                        },
                    ),
                );
            }
            if need_server {
                if let Err(r) = self.upload(
                    "remove.stage",
                    T,
                    StageFile::Installer,
                    &remote::installer_sh(),
                ) {
                    self.emit(report, r);
                    return None;
                }
                let sub = if level == RemoveLevel::Purge {
                    Sub::Purge
                } else {
                    Sub::Uninstall
                };
                let r = self
                    .exec(
                        "remove.uninstall",
                        "Uninstall Praxi on the server",
                        RemoteCmd::Provision(sub),
                        b"",
                        Duration::from_secs(300),
                        true,
                    )
                    .map(|_| {
                        StepResult::pass(
                            "remove.uninstall",
                            "Uninstall Praxi on the server",
                            if level == RemoveLevel::Purge {
                                "removed, data deleted"
                            } else {
                                "removed, data kept"
                            },
                        )
                    })
                    .unwrap_or_else(|r| r);
                self.finish(report, r)?;
            }
        } else {
            self.emit(
                report,
                StepResult::warn(
                    "remove.connect",
                    "SSH login",
                    "server not reachable; only forgetting it locally",
                    "Revoke this computer on the server later if it comes back.",
                ),
            );
        }
        if let Some(id) = self.rec().paired_host_id.clone() {
            let r = self.local.revoke_host(&id);
            self.emit(
                report,
                r.map_or_else(
                    |e| {
                        StepResult::warn(
                            "remove.local",
                            "Revoke the host on this computer",
                            sanitize(&e, 200),
                            "Run `bot host revoke <key>` here.",
                        )
                    },
                    |()| {
                        StepResult::pass("remove.local", "Revoke the host on this computer", "done")
                    },
                ),
            );
        }
        self.emit(
            report,
            StepResult::pass(
                "remove.forget",
                "Forget the server",
                "local record and pinned key removed",
            ),
        );
        Some(())
    }

    fn step_hostkey_quiet(&self) -> Result<(), Error> {
        let rec = self.rec();
        let scanned = self
            .transport
            .scan_host_keys()
            .map_err(|e| Error::Failed(e.to_string()))?;
        match self.known_hosts.check(&rec.host, rec.port, &scanned)? {
            HostKeyState::Trusted { .. } => Ok(()),
            _ => Err(Error::Failed("host key not trusted".into())),
        }
    }
}

#[cfg(test)]
mod install_tag_tests {
    use super::install_tag;

    #[test]
    fn a_pre_release_app_installs_its_own_tag_not_v_plus_the_cargo_version() {
        assert_eq!(install_tag(None, "0.1.0").unwrap(), "v0.1.0");
        assert_eq!(
            install_tag(Some("v0.1.0-alpha.1"), "0.1.0").unwrap(),
            "v0.1.0-alpha.1"
        );
        assert!(install_tag(Some("v0.1.0; rm -rf /"), "0.1.0").is_err());
    }
}
