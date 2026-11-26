//! Release selection, service activation and rollback for installer-managed Linux hosts.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use crate::Result;
use daemon_api::host::HostUpdateState;
use semver::Version;

#[cfg(target_os = "linux")]
#[path = "db_snapshot.rs"]
mod db_snapshot;

fn release_version(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

pub(super) fn newer_release(candidate: &str, installed: &str) -> bool {
    release_version(candidate)
        .zip(release_version(installed))
        .is_some_and(|(candidate, installed)| candidate.cmp_precedence(&installed).is_gt())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Off,
    Notify,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Channel {
    Stable,
    PreRelease,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    mode: Mode,
    channel: Option<Channel>,
}

impl Settings {
    /// An explicit channel always wins. Without one a server follows the kind of release it runs, so
    /// a server installed from a pre-release can still take the next one with one click.
    fn channel(&self, installed: &str) -> Channel {
        self.channel.unwrap_or_else(|| {
            if release_version(installed).is_some_and(|version| !version.pre.is_empty()) {
                Channel::PreRelease
            } else {
                Channel::Stable
            }
        })
    }
}

/// `/etc/praxi/updates.toml` is a few lines; anything near this size is not it.
const MAX_SETTINGS: u64 = 16 * 1024;

fn parse_settings(text: &str) -> Result<Settings> {
    toml::from_str(text)
        .map_err(|_| "invalid /etc/praxi/updates.toml; check the mode and channel".into())
}

fn channel_accepts(channel: Channel, version: &Version, flagged_prerelease: bool) -> bool {
    channel == Channel::PreRelease || (version.pre.is_empty() && !flagged_prerelease)
}

/// Whether a request from a paired desktop may proceed: never a downgrade, never a release the
/// configured channel does not follow. The reasons are for the journal, not for the requester.
fn admit_request(
    requested: &str,
    installed: &str,
    channel: Channel,
) -> std::result::Result<(), &'static str> {
    let version = release_version(requested).ok_or("invalid release tag")?;
    if requested != installed && !newer_release(requested, installed) {
        return Err("managed updates cannot downgrade the server");
    }
    if !channel_accepts(channel, &version, false) {
        return Err(
            "the server follows the stable channel; set channel = \"pre-release\" in /etc/praxi/updates.toml to install a pre-release",
        );
    }
    Ok(())
}

/// Who connected to the update socket, as the kernel reported it.
struct Peer {
    pid: u32,
    uid: u32,
}

/// Only the managed daemon's main process may request an update: same process as systemd's
/// `MainPID`, same non-root user, and in the `praxi.service` control group. A shared service UID
/// alone would also let subprocesses such as shell tools (or a lookalike process) in.
fn peer_is_daemon(peer: &Peer, daemon_pid: u32, daemon_uid: u32, peer_cgroups: &str) -> bool {
    peer.pid == daemon_pid
        && daemon_uid != 0
        && peer.uid == daemon_uid
        && in_service_cgroup(peer_cgroups)
}

/// `/proc/<pid>/cgroup`: the unified (or systemd) hierarchy path must end in `praxi.service`.
fn in_service_cgroup(cgroups: &str) -> bool {
    cgroups.lines().any(|line| {
        let mut parts = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        (id == "0" || controllers == "name=systemd")
            && path.rsplit('/').next() == Some("praxi.service")
    })
}

/// The effective user of a process, from `/proc/<pid>/status`.
fn status_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[derive(Debug, PartialEq, Eq)]
enum Readiness {
    Ready,
    /// Still starting (or not answering yet).
    Waiting,
    /// Up, but its components did not start; waiting longer does not help.
    Failed,
}

fn readiness(health: &serde_json::Value) -> Readiness {
    let healthy = matches!(health["status"].as_str(), Some("ok" | "setup_required"))
        && health["host"]["mode"] == "host";
    match health["startup"].as_str() {
        Some("ready") if healthy => Readiness::Ready,
        Some("degraded") => Readiness::Failed,
        _ => Readiness::Waiting,
    }
}

/// The steps of one activation. Implemented by the live host and by the fakes of the tests.
trait Steps {
    /// Checks that need nothing stopped (disk space).
    fn preflight(&mut self) -> Result<()>;
    /// Stop the daemon and whatever would start it again.
    async fn stop(&mut self) -> Result<()>;
    /// Copy the databases while nothing runs.
    fn snapshot(&mut self) -> Result<()>;
    /// Point the launchers at the new release.
    fn switch(&mut self) -> Result<()>;
    /// Start the daemon and wait until it reports ready.
    async fn start(&mut self) -> Result<()>;
    fn restore_programs(&mut self) -> Result<()>;
    fn restore_data(&mut self) -> Result<()>;
}

/// Stop, copy the data, switch, start. A release that does not become ready is replaced again by
/// the previous program files **and** the previous data: migrations only move forward, so the old
/// release can start only on the data it knew.
async fn transact(steps: &mut impl Steps) -> Result<HostUpdateState> {
    steps.preflight()?;
    if let Err(error) = steps.stop().await {
        // Some of the units may be down already.
        let _ = steps.start().await;
        return Err(error);
    }
    if let Err(error) = steps.snapshot() {
        let _ = steps.start().await;
        return Err(error);
    }
    if let Err(error) = steps.switch() {
        // A rename can have completed even if the following fsync failed. The new release never
        // ran, so the data is as it was.
        let _ = steps.restore_programs();
        let _ = steps.start().await;
        return Err(error);
    }
    if steps.start().await.is_ok() {
        return Ok(HostUpdateState::Updated);
    }
    // The new release may have opened (and migrated) the data: stop it first, so nothing writes
    // while the old files come back.
    let rolled_back = steps.stop().await.is_ok()
        && steps.restore_data().is_ok()
        && steps.restore_programs().is_ok()
        && steps.start().await.is_ok();
    Ok(if rolled_back {
        HostUpdateState::RolledBack
    } else {
        HostUpdateState::RollbackFailed
    })
}

#[cfg(target_os = "linux")]
pub(super) use systemd::{recover, run};

#[cfg(target_os = "linux")]
mod systemd {
    use super::{
        Channel, HostUpdateState, Peer, Readiness, Result, Settings, Steps, admit_request,
        channel_accepts, db_snapshot, newer_release, parse_settings, peer_is_daemon, readiness,
        release_version, status_uid, transact,
    };
    use daemon_api::host::{HostUpdateStatus, UPDATE_STATUS};
    use std::{
        io::{BufRead, Read, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        path::{Path, PathBuf},
        process::Stdio,
        time::Duration,
    };

    const UNIT: &str = "praxi.service";
    /// The units that hold the daemon up (`praxi-xvfb.service` upholds `praxi.service`): they are
    /// stopped first, or systemd would start the daemon again while its data is being copied.
    const DISPLAY_UNITS: [&str; 2] = ["praxi-xvfb.service", "praxi-desktop.service"];
    const PREFIX: &str = "/opt/praxi";
    const CONFIG: &str = "/etc/praxi/updates.toml";
    /// The daemon reports `degraded` after its own startup timeout; give it that long, and a minute
    /// for the process to appear and the schema migration to finish before it listens.
    const READY_TIMEOUT: Duration = Duration::from_secs(daemon_api::STARTUP_TIMEOUT.as_secs() + 60);
    /// Crashes of the new release after which waiting longer is pointless.
    const MAX_RESTARTS: u32 = 3;

    /// Open `path` for reading without ever following a link (or blocking on a pipe), and read it
    /// whole, within `limit` bytes, from the descriptor that was checked. `owner` is the user the
    /// file must belong to (root in production).
    fn read_trusted_file(path: &Path, owner: u32, limit: u64) -> Result<Option<String>> {
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "cannot read {} (it must be a regular file, not a link): {e}",
                    path.display()
                )
                .into());
            }
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
            return Err(format!(
                "{} must be a regular file owned by root and not writable by others",
                path.display()
            )
            .into());
        }
        if meta.len() > limit {
            return Err(format!("{} is too large", path.display()).into());
        }
        let mut text = String::new();
        file.take(limit + 1).read_to_string(&mut text)?;
        if text.len() as u64 > limit {
            return Err(format!("{} is too large", path.display()).into());
        }
        Ok(Some(text))
    }

    fn read_settings(path: &Path, owner: u32) -> Result<Settings> {
        read_trusted_file(path, owner, super::MAX_SETTINGS)?
            .map_or_else(|| Ok(Settings::default()), |text| parse_settings(&text))
    }

    fn settings() -> Result<Settings> {
        let path = Path::new(CONFIG);
        super::super::trusted_root_path(path.parent().ok_or("invalid config path")?)?;
        read_settings(path, 0)
    }

    fn read_status() -> Option<HostUpdateStatus> {
        read_trusted_file(Path::new(UPDATE_STATUS), 0, 4096)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    fn publish(state: HostUpdateState, tag: Option<&str>) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let path = Path::new(UPDATE_STATUS);
        let file = tempfile::NamedTempFile::new_in(path.parent().ok_or("invalid status path")?)?;
        serde_json::to_writer(
            &file,
            &HostUpdateStatus {
                state,
                tag: tag.map(str::to_owned),
            },
        )?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }

    /// `running` with no maintenance lock held is left over from a run that was killed.
    fn clear_stale_running() -> Result<()> {
        if let Some(status) = read_status()
            && matches!(status.state, HostUpdateState::Running)
        {
            publish(HostUpdateState::Failed, status.tag.as_deref())?;
        }
        Ok(())
    }

    /// Runs `systemctl`. A trait so that the stop/start sequence is tested without systemd.
    trait Systemctl {
        /// `Ok(true)` when it exited successfully.
        async fn run(&self, args: &[&str]) -> Result<bool>;
        /// `systemctl show` of the daemon: `Key=Value` lines.
        async fn show(&self, properties: &[&str]) -> Result<String>;
    }

    struct System;

    impl Systemctl for System {
        async fn run(&self, args: &[&str]) -> Result<bool> {
            Ok(tokio::process::Command::new("/usr/bin/systemctl")
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await?
                .success())
        }

        async fn show(&self, properties: &[&str]) -> Result<String> {
            let output = tokio::process::Command::new("/usr/bin/systemctl")
                .arg("show")
                .arg(format!("--property={}", properties.join(",")))
                .arg(UNIT)
                .stdin(Stdio::null())
                .output()
                .await?;
            Ok(String::from_utf8(output.stdout)?)
        }
    }

    fn property(show: &str, key: &str) -> Option<u32> {
        show.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))?
            .trim()
            .parse()
            .ok()
    }

    async fn main_pid(sys: &impl Systemctl) -> Result<u32> {
        match property(&sys.show(&["MainPID"]).await?, "MainPID") {
            Some(pid) if pid != 0 => Ok(pid),
            _ => Err("the managed daemon is not running".into()),
        }
    }

    /// Stop the daemon so that its files are quiet. `display` receives the display units that were
    /// running (also when stopping fails half way), so that they can be started again.
    async fn stop_stack(sys: &impl Systemctl, display: &mut Vec<&'static str>) -> Result<()> {
        let mut running = Vec::new();
        for unit in DISPLAY_UNITS {
            if sys.run(&["is-active", "--quiet", unit]).await? {
                running.push(unit);
            }
        }
        if !running.is_empty() {
            display.clone_from(&running);
        }
        for unit in &running {
            if !sys.run(&["stop", unit]).await? {
                return Err(format!("systemctl stop {unit} failed").into());
            }
        }
        if !sys.run(&["stop", UNIT]).await? {
            return Err(format!("systemctl stop {UNIT} failed").into());
        }
        if sys.run(&["is-active", "--quiet", UNIT]).await? {
            return Err("the daemon did not stop".into());
        }
        Ok(())
    }

    async fn start_stack(sys: &impl Systemctl, display: &[&'static str]) -> Result<()> {
        // The display units are best effort: a desktop that fails must not fail the daemon.
        for unit in display {
            let _ = sys.run(&["start", unit]).await;
        }
        // A bad release may have exhausted Restart=on-failure before the rollback.
        for action in ["reset-failed", "start"] {
            if !sys.run(&[action, UNIT]).await? {
                return Err(format!("systemctl {action} {UNIT} failed").into());
            }
        }
        Ok(())
    }

    async fn health_status(client: &reqwest::Client, url: &str) -> Option<serde_json::Value> {
        async fn health(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
            let response = client.get(url).send().await?.error_for_status()?;
            Ok(serde_json::from_slice(
                &super::super::bounded_body(response, 64 * 1024).await?,
            )?)
        }
        health(client, url).await.ok()
    }

    /// Wait until the process that systemd started runs `executable` and reports ready twice in a
    /// row. Gives up early when the daemon crash-loops or reports a failed startup.
    async fn wait_ready(sys: &impl Systemctl, executable: &Path, port: u16) -> Result<()> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()?;
        let url = format!("http://127.0.0.1:{port}/v1/health");
        let mut healthy_pid = None;
        for _ in 0..READY_TIMEOUT.as_secs() {
            let shown = sys
                .show(&["MainPID", "NRestarts"])
                .await
                .unwrap_or_default();
            if property(&shown, "NRestarts").is_some_and(|restarts| restarts >= MAX_RESTARTS) {
                return Err("the daemon keeps crashing".into());
            }
            let pid = property(&shown, "MainPID").filter(|pid| *pid != 0);
            let mut ready = false;
            if let Some(pid) = pid
                && std::fs::read_link(format!("/proc/{pid}/exe"))
                    .as_deref()
                    .is_ok_and(|exe| exe == executable)
                && let Some(body) = health_status(&client, &url).await
            {
                match readiness(&body) {
                    Readiness::Ready => ready = true,
                    Readiness::Failed => {
                        return Err("the daemon started without its components".into());
                    }
                    Readiness::Waiting => {}
                }
            }
            if ready && healthy_pid == pid {
                return Ok(());
            }
            healthy_pid = if ready { pid } else { None };
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err("daemon failed its readiness check".into())
    }

    /// The live host: systemd, the install prefix and the data directory.
    struct Live<'a> {
        prefix: &'a Path,
        target: &'a Path,
        port: u16,
        current: PathBuf,
        previous: Option<PathBuf>,
        display: Vec<&'static str>,
        data: PathBuf,
        snapshot_dir: PathBuf,
        snapshot_taken: bool,
    }

    impl Steps for Live<'_> {
        fn preflight(&mut self) -> Result<()> {
            db_snapshot::preflight(&self.data, &self.snapshot_dir)
        }

        async fn stop(&mut self) -> Result<()> {
            stop_stack(&System, &mut self.display).await
        }

        fn snapshot(&mut self) -> Result<()> {
            db_snapshot::take(&self.data, &self.snapshot_dir)?;
            self.snapshot_taken = true;
            Ok(())
        }

        fn switch(&mut self) -> Result<()> {
            super::super::switch(self.prefix, self.target)
        }

        async fn start(&mut self) -> Result<()> {
            let executable = self.prefix.join("bin/bot").canonicalize()?;
            start_stack(&System, &self.display).await?;
            wait_ready(&System, &executable, self.port)
                .await
                .inspect_err(|error| eprintln!("Praxi update: {error}"))
        }

        fn restore_programs(&mut self) -> Result<()> {
            use super::super::{remove_link, replace_link};
            replace_link(self.prefix, "current", &self.current)?;
            // If history cannot be restored, still try starting the restored daemon.
            let history = match self.previous.as_deref() {
                Some(previous) => replace_link(self.prefix, "previous", previous),
                None => remove_link(self.prefix, "previous"),
            };
            if let Err(error) = history {
                eprintln!("Praxi rollback: could not restore version history: {error}");
            }
            Ok(())
        }

        fn restore_data(&mut self) -> Result<()> {
            if !self.snapshot_taken {
                return Err("no database snapshot to restore".into());
            }
            db_snapshot::restore(&self.data, &self.snapshot_dir)
                .inspect_err(|error| eprintln!("Praxi rollback: {error}"))
        }
    }

    async fn activate(prefix: &Path, target: &Path, port: u16) -> Result<HostUpdateState> {
        use super::super::read_version_link;
        let current = read_version_link(prefix, "current")?.ok_or("missing rollback baseline")?;
        let previous = read_version_link(prefix, "previous")?;
        let mut live = Live {
            prefix,
            target,
            port,
            current,
            previous,
            display: Vec::new(),
            data: PathBuf::from(db_snapshot::DATA_DIR),
            snapshot_dir: PathBuf::from(db_snapshot::SNAPSHOT_DIR),
            snapshot_taken: false,
        };
        let state = transact(&mut live).await?;
        if !matches!(state, HostUpdateState::RollbackFailed) {
            // Whatever neither `current` nor `previous` names (a release that was rolled back, the
            // repair copies of earlier updates) only costs disk.
            super::super::prune_versions(prefix);
        }
        Ok(state)
    }

    async fn requested_tag() -> Result<String> {
        // systemd passes the accepted Unix socket as stdin.
        let stdin = std::io::stdin();
        let peer = rustix::net::sockopt::socket_peercred(&stdin)?;
        let peer = Peer {
            pid: u32::try_from(peer.pid.as_raw_pid())?,
            uid: peer.uid.as_raw(),
        };
        let daemon_pid = main_pid(&System).await?;
        let daemon_uid = status_uid(&std::fs::read_to_string(format!(
            "/proc/{daemon_pid}/status"
        ))?)
        .ok_or("cannot tell which user runs the daemon")?;
        let cgroups =
            std::fs::read_to_string(format!("/proc/{}/cgroup", peer.pid)).unwrap_or_default();
        if !peer_is_daemon(&peer, daemon_pid, daemon_uid, &cgroups) {
            return Err("update requests must come from the managed daemon process".into());
        }
        // A peer that connects and says nothing must not hold one of the few socket slots.
        rustix::net::sockopt::set_socket_timeout(
            &stdin,
            rustix::net::sockopt::Timeout::Recv,
            Some(Duration::from_secs(10)),
        )?;
        let mut tag = String::new();
        stdin
            .lock()
            .take(102)
            .read_line(&mut tag)
            .map_err(|_| "no update request arrived in time")?;
        if tag.len() > 101 || !tag.ends_with('\n') {
            return Err("invalid update request".into());
        }
        tag.pop();
        release_version(&tag).ok_or("invalid release tag")?;
        Ok(tag)
    }

    async fn latest(
        client: &reqwest::Client,
        channel: Channel,
    ) -> Result<Option<super::super::Release>> {
        use super::super::{API, Release, assets, bounded_body};
        let preview = channel == Channel::PreRelease;
        let endpoint = if preview {
            "releases?per_page=30"
        } else {
            "releases/latest"
        };
        let response = client.get(format!("{API}/{endpoint}")).send().await?;
        if !preview && response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = bounded_body(response.error_for_status()?, 4 * 1024 * 1024).await?;
        let releases: Vec<Release> = if preview {
            serde_json::from_slice(&bytes)?
        } else {
            vec![serde_json::from_slice(&bytes)?]
        };
        Ok(releases
            .into_iter()
            .filter_map(|release| {
                let version = release_version(&release.tag_name)?;
                (!release.draft
                    && channel_accepts(channel, &version, release.prerelease)
                    && assets(&release).is_ok())
                .then_some((version, release))
            })
            .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
            .map(|(_, release)| release))
    }

    /// `ExecStopPost` of the updater units. An updater that was killed (time limit, crash, OOM)
    /// leaves `running` behind, which keeps the desktop's update button off, and can leave the daemon
    /// stopped. Holding the maintenance lock proves that no other update is running.
    pub(in crate::update) async fn recover() -> Result<()> {
        let Ok(_maintenance) = super::super::maintenance_lock() else {
            return Ok(());
        };
        let Some(status) = read_status().filter(|s| matches!(s.state, HostUpdateState::Running))
        else {
            return Ok(());
        };
        eprintln!("Praxi update: the previous update run did not finish");
        publish(HostUpdateState::Failed, status.tag.as_deref())?;
        if let Err(error) = start_stack(&System, &[]).await {
            eprintln!("Praxi update: could not start the daemon again: {error}");
        }
        Ok(())
    }

    pub(in crate::update) async fn run(port: u16, scheduled: bool) -> Result<()> {
        use super::super::{
            API, installed_tag, lock, maintenance_lock, prepare, release, release_client,
            trusted_root_path,
        };
        let prefix = Path::new(PREFIX);
        trusted_root_path(prefix)?;
        let requested = if scheduled {
            None
        } else {
            Some(requested_tag().await?)
        };
        let _maintenance = maintenance_lock()?;
        let _lock = lock(prefix)?;
        // Nothing else runs now: an update still marked as running was killed.
        clear_stale_running()?;
        let installed = installed_tag(prefix)?;
        let settings = settings()?;
        let channel = settings.channel(&installed);
        let client = release_client()?;
        if let Some(tag) = requested.as_deref() {
            if let Err(reason) = admit_request(tag, &installed, channel) {
                return Err(reason.into());
            }
            publish(HostUpdateState::Running, Some(tag))?;
            let mut reply = std::io::stdout().lock();
            if let Err(error) = reply.write_all(b"1").and_then(|()| reply.flush()) {
                publish(HostUpdateState::Failed, Some(tag))?;
                return Err(error.into());
            }
        }
        let mut attempted = requested.clone();
        let result: Result<()> = async {
            let release = if let Some(tag) = requested.as_deref() {
                let release = release(&client, API, Some(tag)).await?;
                // The tag was admitted before the release (and its pre-release flag) was known.
                let version = release_version(&release.tag_name).ok_or("invalid release tag")?;
                if !channel_accepts(channel, &version, release.prerelease) {
                    return Err(
                        "the server follows the stable channel; this release is a pre-release"
                            .into(),
                    );
                }
                release
            } else {
                let status = read_status();
                if matches!(settings.mode, super::Mode::Off) || main_pid(&System).await.is_err() {
                    if status
                        .is_some_and(|status| matches!(status.state, HostUpdateState::Available))
                    {
                        publish(HostUpdateState::Idle, None)?;
                    }
                    return Ok(());
                }
                let Some(candidate) = latest(&client, channel)
                    .await?
                    .filter(|candidate| newer_release(&candidate.tag_name, &installed))
                else {
                    if status
                        .is_some_and(|status| matches!(status.state, HostUpdateState::Available))
                    {
                        publish(HostUpdateState::Idle, None)?;
                    }
                    return Ok(());
                };
                if matches!(settings.mode, super::Mode::Notify) {
                    return publish(HostUpdateState::Available, Some(&candidate.tag_name));
                }
                if status.is_some_and(|status| {
                    matches!(status.state, HostUpdateState::RollbackFailed)
                        || (matches!(status.state, HostUpdateState::RolledBack)
                            && status.tag.as_deref() == Some(&candidate.tag_name))
                }) {
                    return Ok(());
                }
                attempted = Some(candidate.tag_name.clone());
                publish(HostUpdateState::Running, attempted.as_deref())?;
                candidate
            };
            let target = prepare(prefix, &client, &release).await?;
            main_pid(&System).await?;
            let state = activate(prefix, &target, port).await?;
            publish(state, Some(&release.tag_name))?;
            Ok(())
        }
        .await;
        if result.is_err() && attempted.is_some() {
            publish(HostUpdateState::Failed, attempted.as_deref())?;
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::RefCell;
        use std::os::unix::fs::PermissionsExt;

        fn me() -> u32 {
            rustix::process::getuid().as_raw()
        }

        fn config(dir: &Path, text: &str) -> PathBuf {
            let path = dir.join("updates.toml");
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            path
        }

        #[test]
        fn settings_are_read_from_a_regular_file_of_the_right_owner_only() {
            let dir = tempfile::tempdir().unwrap();
            // Missing: the defaults.
            let missing = read_settings(&dir.path().join("none.toml"), me()).unwrap();
            assert_eq!(missing.mode, super::super::Mode::Off);
            assert_eq!(missing.channel, None);
            let path = config(dir.path(), "mode = 'auto'\nchannel = 'pre-release'\n");
            let parsed = read_settings(&path, me()).unwrap();
            assert_eq!(parsed.mode, super::super::Mode::Auto);
            assert_eq!(parsed.channel, Some(Channel::PreRelease));
            // Another owner than the trusted one.
            assert!(read_settings(&path, me() + 1).is_err());
            // A link, even to a perfectly good file.
            let link = dir.path().join("link.toml");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            let error = read_settings(&link, me()).unwrap_err().to_string();
            assert!(error.contains("not a link"), "{error}");
            // A directory, a pipe, group- or world-writable and oversized files.
            let directory = dir.path().join("dir.toml");
            std::fs::create_dir(&directory).unwrap();
            assert!(read_settings(&directory, me()).is_err());
            let pipe = dir.path().join("pipe.toml");
            rustix::fs::mknodat(
                rustix::fs::CWD,
                &pipe,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o644),
                0,
            )
            .unwrap();
            assert!(
                read_settings(&pipe, me()).is_err(),
                "a pipe must not block or be read"
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(read_settings(&path, me()).is_err());
            let big = config(
                dir.path(),
                &"#".repeat(usize::try_from(super::super::MAX_SETTINGS).unwrap() + 1),
            );
            assert!(
                read_settings(&big, me())
                    .unwrap_err()
                    .to_string()
                    .contains("too large")
            );
            // Unknown keys and values are still refused.
            let odd = config(dir.path(), "mode = 'sometimes'");
            assert!(read_settings(&odd, me()).is_err());
            let extra = config(dir.path(), "channel = 'stable'\nunknown = 1");
            assert!(read_settings(&extra, me()).is_err());
        }

        struct FakeSystemctl {
            calls: RefCell<Vec<String>>,
            /// Units that are active; `stop` removes them, `start` adds them.
            active: RefCell<Vec<&'static str>>,
            /// `stop praxi.service` does not take effect (something upholds it).
            stubborn: bool,
        }

        impl FakeSystemctl {
            fn new(active: &[&'static str]) -> Self {
                Self {
                    calls: RefCell::default(),
                    active: RefCell::new(active.to_vec()),
                    stubborn: false,
                }
            }
        }

        impl Systemctl for FakeSystemctl {
            async fn run(&self, args: &[&str]) -> Result<bool> {
                self.calls.borrow_mut().push(args.join(" "));
                let unit = args.last().copied().unwrap_or_default();
                match args[0] {
                    "is-active" => Ok(self.active.borrow().contains(&unit)),
                    "stop" => {
                        if !(self.stubborn && unit == UNIT) {
                            self.active.borrow_mut().retain(|active| *active != unit);
                            // BindsTo: the daemon goes down with its display.
                            if unit == "praxi-xvfb.service" {
                                self.active.borrow_mut().retain(|active| *active != UNIT);
                            }
                        }
                        Ok(true)
                    }
                    _ => Ok(true),
                }
            }

            async fn show(&self, _: &[&str]) -> Result<String> {
                Ok(String::new())
            }
        }

        #[tokio::test]
        async fn stopping_stops_what_upholds_the_daemon_first() {
            let sys = FakeSystemctl::new(&[UNIT, "praxi-xvfb.service", "praxi-desktop.service"]);
            let mut running = Vec::new();
            stop_stack(&sys, &mut running).await.unwrap();
            assert_eq!(running, ["praxi-xvfb.service", "praxi-desktop.service"]);
            let calls = sys.calls.borrow();
            let at = |call: &str| calls.iter().position(|c| c == call).unwrap();
            assert!(at("stop praxi-xvfb.service") < at("stop praxi.service"));
            assert!(at("stop praxi-desktop.service") < at("stop praxi.service"));
            assert!(sys.active.borrow().is_empty());
        }

        #[tokio::test]
        async fn a_headless_host_without_a_display_just_stops_the_daemon() {
            let sys = FakeSystemctl::new(&[UNIT]);
            let mut running = Vec::new();
            stop_stack(&sys, &mut running).await.unwrap();
            assert!(running.is_empty());
            assert!(
                !sys.calls
                    .borrow()
                    .iter()
                    .any(|c| c.starts_with("stop praxi-") && c.contains("xvfb"))
            );
        }

        #[tokio::test]
        async fn a_daemon_that_does_not_stay_down_is_never_copied() {
            let mut sys = FakeSystemctl::new(&[UNIT]);
            sys.stubborn = true;
            let error = stop_stack(&sys, &mut Vec::new()).await.unwrap_err();
            assert!(error.to_string().contains("did not stop"));
        }

        #[tokio::test]
        async fn starting_resets_a_failed_unit_and_starts_the_display_first() {
            let sys = FakeSystemctl::new(&[]);
            start_stack(&sys, &["praxi-xvfb.service"]).await.unwrap();
            assert_eq!(
                *sys.calls.borrow(),
                [
                    "start praxi-xvfb.service",
                    "reset-failed praxi.service",
                    "start praxi.service"
                ]
            );
        }

        #[test]
        fn systemctl_show_output_is_read_by_key() {
            // The properties read from `systemctl show` are plain Key=Value lines in any order.
            let shown = "NRestarts=2\nMainPID=4242\n";
            assert_eq!(property(shown, "MainPID"), Some(4242));
            assert_eq!(property(shown, "NRestarts"), Some(2));
            assert_eq!(property("MainPID=\n", "MainPID"), None);
            assert_eq!(property("", "NRestarts"), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn semver(tag: &str) -> Version {
        release_version(tag).unwrap()
    }

    #[test]
    fn managed_update_accepts_only_newer_release_precedence() {
        assert!(newer_release("v0.1.0-alpha.10", "v0.1.0-alpha.5"));
        assert!(newer_release("v0.1.0", "v0.1.0-alpha.10"));
        for tag in [
            "v0.1.0-alpha.4",
            "v0.1.0-alpha.5",
            "v0.1.0-alpha.5+other",
            "latest",
            "v0.1.0/../",
        ] {
            assert!(!newer_release(tag, "v0.1.0-alpha.5"));
        }
    }

    #[test]
    fn requests_never_downgrade_and_follow_the_channel() {
        let ok = |tag, installed, channel| admit_request(tag, installed, channel).is_ok();
        // Forward and same-tag repair are fine on the pre-release channel.
        assert!(ok("v0.1.0-alpha.6", "v0.1.0-alpha.5", Channel::PreRelease));
        assert!(ok("v0.1.0-alpha.5", "v0.1.0-alpha.5", Channel::PreRelease));
        // Backwards, whatever the channel: also an intermediate tag of an older line.
        for channel in [Channel::Stable, Channel::PreRelease] {
            assert!(!ok("v0.1.0-alpha.4", "v0.1.0-alpha.5", channel));
            assert!(!ok("v0.0.9", "v0.1.0", channel));
            assert!(!ok("v0.1.0-alpha.9", "v0.1.0", channel));
            assert!(!ok("latest", "v0.1.0", channel));
            // Build metadata never makes a release newer.
            assert!(!ok("v0.1.0-alpha.5+other", "v0.1.0-alpha.5+x", channel));
        }
        // The stable channel never moves a server to a pre-release; a final release is fine.
        assert!(!ok("v0.2.0-rc.1", "v0.1.0", Channel::Stable));
        assert!(ok("v0.2.0", "v0.1.0", Channel::Stable));
        assert!(ok("v0.2.0-rc.1", "v0.1.0", Channel::PreRelease));
        // A published release flagged as pre-release is not stable even without a suffix.
        assert!(!channel_accepts(Channel::Stable, &semver("v0.2.0"), true));
        assert!(channel_accepts(Channel::Stable, &semver("v0.2.0"), false));
        assert!(channel_accepts(
            Channel::PreRelease,
            &semver("v0.2.0-rc.1"),
            true
        ));
    }

    #[test]
    fn an_explicit_channel_wins_and_an_unset_one_follows_the_installed_release() {
        let unset = parse_settings("mode = 'notify'").unwrap();
        assert_eq!(unset.channel("v0.1.0-alpha.5"), Channel::PreRelease);
        assert_eq!(unset.channel("v0.1.0"), Channel::Stable);
        let explicit = parse_settings("channel = 'stable'").unwrap();
        assert_eq!(explicit.channel("v0.1.0-alpha.5"), Channel::Stable);
        let explicit = parse_settings("channel = 'pre-release'").unwrap();
        assert_eq!(explicit.channel("v0.1.0"), Channel::PreRelease);
        // A stable-by-config server running a pre-release is not switched to a newer one.
        assert!(
            admit_request(
                "v0.1.0-alpha.6",
                "v0.1.0-alpha.5",
                parse_settings("channel = 'stable'")
                    .unwrap()
                    .channel("v0.1.0-alpha.5")
            )
            .is_err()
        );
    }

    #[test]
    fn only_the_daemons_main_process_in_its_own_service_may_request_an_update() {
        let unified = "0::/system.slice/praxi.service\n";
        let peer = |pid, uid| Peer { pid, uid };
        assert!(peer_is_daemon(&peer(100, 999), 100, 999, unified));
        assert!(peer_is_daemon(
            &peer(100, 999),
            100,
            999,
            "0::/praxi.slice/praxi.service"
        ));
        assert!(peer_is_daemon(
            &peer(100, 999),
            100,
            999,
            "12:cpu:/\n1:name=systemd:/system.slice/praxi.service\n"
        ));
        // A subprocess of the daemon (same user, same service, other pid).
        assert!(!peer_is_daemon(&peer(101, 999), 100, 999, unified));
        // The daemon's pid with another user; or root, which the daemon never is.
        assert!(!peer_is_daemon(&peer(100, 1000), 100, 999, unified));
        assert!(!peer_is_daemon(&peer(100, 0), 100, 0, unified));
        // Same pid and user in another service, in a sub-group of it, or with no group at all.
        for cgroup in [
            "0::/system.slice/other.service",
            "0::/system.slice/praxi.service/child",
            "0::/user.slice/user-999.slice/session-3.scope",
            "0::/system.slice/praxi.service.d\n",
            "",
            "garbage",
        ] {
            assert!(
                !peer_is_daemon(&peer(100, 999), 100, 999, cgroup),
                "{cgroup}"
            );
        }
    }

    #[test]
    fn the_daemons_user_is_read_from_its_status() {
        let status =
            "Name:\tbot\nUmask:\t0022\nUid:\t999\t998\t999\t998\nGid:\t999\t999\t999\t999\n";
        assert_eq!(status_uid(status), Some(998));
        assert_eq!(status_uid("Name:\tbot\n"), None);
        assert_eq!(status_uid("Uid:\tx\ty\n"), None);
    }

    #[test]
    fn readiness_waits_fails_fast_on_a_degraded_start_and_needs_host_mode() {
        let health = |status: &str, mode: &str, startup: &str| serde_json::json!({"status": status, "host": {"mode": mode}, "startup": startup});
        assert_eq!(readiness(&health("ok", "host", "ready")), Readiness::Ready);
        assert_eq!(
            readiness(&health("setup_required", "host", "ready")),
            Readiness::Ready
        );
        assert_eq!(
            readiness(&health("ok", "host", "starting")),
            Readiness::Waiting
        );
        assert_eq!(
            readiness(&health("ok", "standalone", "ready")),
            Readiness::Waiting
        );
        assert_eq!(
            readiness(&health("error", "host", "ready")),
            Readiness::Waiting
        );
        assert_eq!(
            readiness(&health("ok", "host", "degraded")),
            Readiness::Failed
        );
        assert_eq!(readiness(&serde_json::json!({})), Readiness::Waiting);
    }

    /// Records every step in order and fails the ones it is told to.
    #[derive(Default)]
    struct Fake {
        events: RefCell<Vec<String>>,
        fail: Vec<&'static str>,
        /// Results of `start` and `stop`, in order; absent entries succeed.
        starts: RefCell<Vec<bool>>,
        stops: RefCell<Vec<bool>>,
    }

    impl Fake {
        fn step(&self, name: &'static str) -> Result<()> {
            self.events.borrow_mut().push(name.to_owned());
            if self.fail.contains(&name) {
                Err(format!("{name} failed").into())
            } else {
                Ok(())
            }
        }
        fn scripted(&self, name: &str, results: &RefCell<Vec<bool>>) -> Result<()> {
            self.events.borrow_mut().push(name.to_owned());
            let ok = if self.fail.contains(&name) {
                false
            } else if results.borrow().is_empty() {
                true
            } else {
                results.borrow_mut().remove(0)
            };
            if ok {
                Ok(())
            } else {
                Err(format!("{name} failed").into())
            }
        }
        fn events(&self) -> Vec<String> {
            self.events.borrow().clone()
        }
    }

    impl Steps for Fake {
        fn preflight(&mut self) -> Result<()> {
            self.step("preflight")
        }
        async fn stop(&mut self) -> Result<()> {
            self.scripted("stop", &self.stops)
        }
        fn snapshot(&mut self) -> Result<()> {
            self.step("snapshot")
        }
        fn switch(&mut self) -> Result<()> {
            self.step("switch")
        }
        async fn start(&mut self) -> Result<()> {
            self.scripted("start", &self.starts)
        }
        fn restore_programs(&mut self) -> Result<()> {
            self.step("restore_programs")
        }
        fn restore_data(&mut self) -> Result<()> {
            self.step("restore_data")
        }
    }

    #[tokio::test]
    async fn a_healthy_update_snapshots_before_the_switch_and_restores_nothing() {
        let mut steps = Fake::default();
        let state = transact(&mut steps).await.unwrap();
        assert!(matches!(state, HostUpdateState::Updated));
        assert_eq!(
            steps.events(),
            ["preflight", "stop", "snapshot", "switch", "start"]
        );
    }

    #[tokio::test]
    async fn a_release_that_never_becomes_ready_is_rolled_back_with_its_data() {
        let mut steps = Fake {
            starts: RefCell::new(vec![false, true]),
            ..Fake::default()
        };
        let state = transact(&mut steps).await.unwrap();
        assert!(matches!(state, HostUpdateState::RolledBack));
        // The failed release is stopped before the data comes back, and the old release starts last.
        assert_eq!(
            steps.events(),
            [
                "preflight",
                "stop",
                "snapshot",
                "switch",
                "start",
                "stop",
                "restore_data",
                "restore_programs",
                "start"
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_rollback_is_reported_and_never_starts_on_data_it_could_not_restore() {
        // (failing step, results of `stop`, last events expected)
        let cases: [(&str, Vec<bool>, &[&str]); 3] = [
            ("restore_data", vec![], &["stop", "restore_data"]),
            (
                "restore_programs",
                vec![],
                &["stop", "restore_data", "restore_programs"],
            ),
            // The failed release cannot be stopped: nothing is restored under a running process.
            ("", vec![true, false], &["stop"]),
        ];
        for (fail, stops, tail) in cases {
            let mut steps = Fake {
                fail: if fail.is_empty() { vec![] } else { vec![fail] },
                starts: RefCell::new(vec![false]),
                stops: RefCell::new(stops),
                ..Fake::default()
            };
            let state = transact(&mut steps).await.unwrap();
            assert!(matches!(state, HostUpdateState::RollbackFailed), "{fail}");
            let events = steps.events();
            assert_eq!(&events[events.len() - tail.len()..], tail, "{fail}");
            // Only the new release was started: the old one is not started on unrestored files.
            assert_eq!(events.iter().filter(|e| *e == "start").count(), 1, "{fail}");
        }
        // Restored, but the old release does not come up either.
        let mut steps = Fake {
            starts: RefCell::new(vec![false, false]),
            ..Fake::default()
        };
        assert!(matches!(
            transact(&mut steps).await.unwrap(),
            HostUpdateState::RollbackFailed
        ));
    }

    #[tokio::test]
    async fn trouble_before_the_switch_puts_the_service_back_and_changes_nothing() {
        for fail in ["preflight", "stop", "snapshot"] {
            let mut steps = Fake {
                fail: vec![fail],
                ..Fake::default()
            };
            assert!(transact(&mut steps).await.is_err(), "{fail}");
            let events = steps.events();
            // Never switched, never restored; the service is started again unless nothing was stopped.
            assert!(
                !events
                    .iter()
                    .any(|e| e == "switch" || e.starts_with("restore")),
                "{fail}"
            );
            assert_eq!(
                events.iter().any(|e| e == "start"),
                fail != "preflight",
                "{fail}"
            );
            // A database that was not copied is never followed by a switch.
            assert!(!events.contains(&"switch".to_owned()), "{fail}");
        }
    }

    #[tokio::test]
    async fn a_failed_switch_restores_the_programs_but_not_the_untouched_data() {
        let mut steps = Fake {
            fail: vec!["switch"],
            ..Fake::default()
        };
        assert!(transact(&mut steps).await.is_err());
        assert_eq!(
            steps.events(),
            [
                "preflight",
                "stop",
                "snapshot",
                "switch",
                "restore_programs",
                "start"
            ]
        );
    }
}
