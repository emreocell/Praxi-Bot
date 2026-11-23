//! The desktop app's "Add server" wizard (`/v1/hosts/setup`).
//!
//! Runs the SAME engine as `bot host add` (`host-provision`) on a background
//! thread inside the daemon, so there is one provisioner, not two. The daemon
//! was chosen over the Tauri side because it already owns the iroh identity,
//! the pairing store and the `/v1/hosts` links the engine pairs through, and
//! the wizard then also works from `bot serve` without the app.
//!
//! What the job holds: step texts the engine already sanitised, the host key
//! fingerprint (public), the paired host id (public). It never holds the key
//! file's content (only its path goes to `ssh -i`), an invite ticket or a
//! confirmation code: those stay inside the engine and the local daemon.
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use daemon_api::{
    host_links::{HostLinkError, HostLinks},
    host_setup::{
        HostJobKind, HostKeyPrompt, HostSetup, HostSetupError, HostSetupJob, HostSetupState,
        HostSetupStep, HostSetupStepStatus, SshKeyHint, StartHostSetupInput, StartHostUpdateInput,
    },
};
use host_provision::{
    Engine, HostHealth, HostRecord, KnownHosts, LocalHost, Mode, Output, PairedHost, Params,
    RemoteCmd, Status, StepResult, Store, Sub, Transport, TransportError, Ui, validate,
};
use parking_lot::Mutex;
use reqwest::Method;
use serde_json::Value;

use crate::{
    client::Client,
    host_links::CliHostLinks,
    host_provision::{resolve_parts, transport},
};

const KEEP_JOBS: usize = 8;
const MAX_NOTES: usize = 40;
/// How long the wizard waits for the user to confirm a first-use host key.
const KEY_WAIT: Duration = Duration::from_secs(300);

/// Can the engine download and install Praxi on a server? That needs a
/// published release (the server fetches the tag itself and verifies its
/// attestation); none exists yet, so the wizard only adds servers that
/// already run Praxi. `PRAXI_HOST_INSTALL=1` switches the install path on for
/// whoever publishes a release. No download address is built here.
fn install_available() -> bool {
    std::env::var("PRAXI_HOST_INSTALL").is_ok_and(|v| v == "1")
}

struct Slot {
    job: HostSetupJob,
    cancel: Arc<AtomicBool>,
    answer: mpsc::Sender<bool>,
}

type Jobs = Arc<Mutex<VecDeque<Slot>>>;

fn update(jobs: &Jobs, id: &str, f: impl FnOnce(&mut Slot)) {
    if let Some(slot) = jobs.lock().iter_mut().find(|s| s.job.id == id) {
        f(slot);
    }
}

/// Builds the SSH transport of one job (the real OpenSSH one; tests inject a fake).
type TransportFactory =
    Arc<dyn Fn(&Store, &HostRecord) -> Result<Box<dyn Transport>, String> + Send + Sync>;

pub struct CliHostSetup {
    data: PathBuf,
    links: Arc<CliHostLinks>,
    jobs: Jobs,
    transport: TransportFactory,
}

impl CliHostSetup {
    pub fn new(data: PathBuf, links: Arc<CliHostLinks>) -> Self {
        Self {
            data,
            links,
            jobs: Arc::new(Mutex::new(VecDeque::new())),
            transport: Arc::new(|store, record| {
                transport(store, record)
                    .map(|t| Box::new(t) as Box<dyn Transport>)
                    .map_err(|e| e.to_string())
            }),
        }
    }

    fn launch(
        &self,
        record: HostRecord,
        mode: Mode,
        kind: HostJobKind,
    ) -> Result<HostSetupJob, HostSetupError> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| HostSetupError::Conflict("the daemon is shutting down".into()))?;

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = uuid::Uuid::new_v4().to_string();
        let job = HostSetupJob {
            id: id.clone(),
            kind,
            state: HostSetupState::Running,
            target: format!("{}@{}:{}", record.user, record.host, record.port),
            steps: Vec::new(),
            notes: Vec::new(),
            host_key: None,
            host_id: None,
            host_name: None,
            view_targets: None,
            install_available: install_available(),
        };
        {
            let mut jobs = self.jobs.lock();
            if jobs.iter().any(|s| busy(s.job.state)) {
                return Err(HostSetupError::Conflict(
                    "another server setup is still running".into(),
                ));
            }
            while jobs.len() >= KEEP_JOBS {
                jobs.pop_front();
            }
            jobs.push_back(Slot {
                job: job.clone(),
                cancel: cancel.clone(),
                answer: tx,
            });
        }

        let worker = Worker {
            data: self.data.clone(),
            links: self.links.clone(),
            jobs: self.jobs.clone(),
            handle,
            id,
            cancel,
            rx,
            record,
            transport: self.transport.clone(),
            mode,
        };
        std::thread::Builder::new()
            .name("host-setup".into())
            .spawn(move || worker.run())
            .map_err(|e| HostSetupError::Conflict(format!("could not start the setup: {e}")))?;
        Ok(job)
    }

    #[cfg(test)]
    fn with_transport(mut self, factory: TransportFactory) -> Self {
        self.transport = factory;
        self
    }
}

fn busy(state: HostSetupState) -> bool {
    matches!(
        state,
        HostSetupState::Running | HostSetupState::AwaitingHostKey
    )
}

fn invalid(e: impl std::fmt::Display) -> HostSetupError {
    HostSetupError::Invalid(e.to_string())
}

impl HostSetup for CliHostSetup {
    fn start(&self, input: StartHostSetupInput) -> Result<HostSetupJob, HostSetupError> {
        let host = validate::host(input.host.trim()).map_err(invalid)?;
        let user =
            validate::user(input.user.as_deref().map_or("ubuntu", str::trim)).map_err(invalid)?;
        let port = validate::port(input.port.unwrap_or(22)).map_err(invalid)?;
        let key = match input.key_path.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(p) => Some(validate::key_path(std::path::Path::new(p)).map_err(invalid)?),
        };
        let store = Store::from_data_dir(&self.data);
        let record = resolve_parts(
            &store,
            &format!("{user}@{host}"),
            Some(u32::from(port)),
            key.as_deref(),
            input
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty()),
        )
        .map_err(invalid)?;
        // What the user typed now wins over a stale record from an earlier
        // attempt: the key (or its absence: ssh-agent) is never inherited.
        let mut record = record;
        record.key_path.clone_from(&key);
        record.port = port;
        self.launch(record, Mode::Add, HostJobKind::Setup)
    }

    fn start_update(&self, input: StartHostUpdateInput) -> Result<HostSetupJob, HostSetupError> {
        let store = Store::from_data_dir(&self.data);
        // The SSH details the "Add server" wizard stored for this server: nothing new is saved.
        let record = store
            .list()
            .into_iter()
            .find(|r| r.paired_host_id.as_deref() == Some(input.host_id.as_str()))
            .ok_or_else(|| {
                HostSetupError::Invalid(
                    "no saved login details for this server on this computer".into(),
                )
            })?;
        // The stored file is plain JSON on disk: re-check every field that reaches ssh's argv.
        let mut record = record;
        validate::host(&record.host).map_err(invalid)?;
        validate::user(&record.user).map_err(invalid)?;
        validate::port(u32::from(record.port)).map_err(invalid)?;
        if let Some(key) = record.key_path.take() {
            record.key_path = Some(validate::key_path(&key).map_err(invalid)?);
        }
        self.launch(record, Mode::Upgrade, HostJobKind::Update)
    }

    fn job(&self, id: &str) -> Option<HostSetupJob> {
        self.jobs
            .lock()
            .iter()
            .find(|s| s.job.id == id)
            .map(|s| s.job.clone())
    }

    fn answer_host_key(&self, id: &str, accept: bool) -> Result<HostSetupJob, HostSetupError> {
        let jobs = self.jobs.lock();
        let slot = jobs
            .iter()
            .find(|s| s.job.id == id)
            .ok_or(HostSetupError::NotFound)?;
        if slot.job.state != HostSetupState::AwaitingHostKey {
            return Err(HostSetupError::Conflict(
                "this setup is not waiting for a host key answer".into(),
            ));
        }
        let _ = slot.answer.send(accept);
        Ok(slot.job.clone())
    }

    fn cancel(&self, id: &str) -> Result<HostSetupJob, HostSetupError> {
        let mut jobs = self.jobs.lock();
        let slot = jobs
            .iter_mut()
            .find(|s| s.job.id == id)
            .ok_or(HostSetupError::NotFound)?;
        if busy(slot.job.state) {
            // The step that is running on the server finishes first (an SSH
            // command cannot be interrupted safely); nothing new starts.
            slot.cancel.store(true, Ordering::SeqCst);
            let _ = slot.answer.send(false);
            slot.job.state = HostSetupState::Cancelled;
            slot.job.host_key = None;
        }
        Ok(slot.job.clone())
    }

    fn ssh_keys(&self) -> Vec<SshKeyHint> {
        ssh_key_hints()
    }
}

/// Private key files in `~/.ssh` (name and path only; at most the first line
/// of a candidate is read to tell a key from other files).
fn ssh_key_hints() -> Vec<SshKeyHint> {
    use std::io::{BufRead, BufReader};
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Vec::new();
    };
    let dir = PathBuf::from(home).join(".ssh");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut keys: Vec<SshKeyHint> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?.to_owned();
            let meta = e.metadata().ok()?;
            if !meta.is_file()
                || meta.len() > 16 * 1024
                || name.to_ascii_lowercase().ends_with(".pub")
            {
                return None;
            }
            let mut first = String::new();
            let mut reader = BufReader::new(std::fs::File::open(&path).ok()?);
            let _ = reader.read_line(&mut first);
            (first.starts_with("-----BEGIN") && first.contains("PRIVATE KEY")).then(|| SshKeyHint {
                name,
                path: path.to_string_lossy().into_owned(),
            })
        })
        .collect();
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    keys.truncate(20);
    keys
}

fn describe(e: HostLinkError) -> String {
    match e {
        HostLinkError::NotFound => "unknown host".into(),
        HostLinkError::Invalid(m) | HostLinkError::Failed(m) => m,
    }
}

struct Worker {
    data: PathBuf,
    links: Arc<CliHostLinks>,
    jobs: Jobs,
    handle: tokio::runtime::Handle,
    id: String,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<bool>,
    record: HostRecord,
    transport: TransportFactory,
    mode: Mode,
}

impl Worker {
    fn run(self) {
        let store = Store::from_data_dir(&self.data);
        let known_hosts = KnownHosts::new(store.known_hosts_path());
        let outcome = self.provision(&store, &known_hosts);
        let cancelled = self.cancel.load(Ordering::SeqCst);
        update(&self.jobs, &self.id, |s| {
            s.job.state = if cancelled {
                HostSetupState::Cancelled
            } else if outcome {
                HostSetupState::Succeeded
            } else {
                HostSetupState::Failed
            };
            s.job.host_key = None;
        });
    }

    /// `true` when every step passed.
    fn provision(&self, store: &Store, known_hosts: &KnownHosts) -> bool {
        let mut ui = JobUi {
            jobs: self.jobs.clone(),
            id: self.id.clone(),
            cancel: self.cancel.clone(),
            rx: &self.rx,
        };
        if let Err(e) = store.save(&self.record) {
            ui.progress(&StepResult::fail(
                "prepare",
                "Prepare",
                e.to_string(),
                "Praxi could not write its local server record.",
            ));
            return false;
        }
        let ssh = match (self.transport)(store, &self.record) {
            Ok(t) => t,
            Err(e) => {
                ui.progress(&StepResult::fail(
                    "prepare",
                    "SSH client",
                    e,
                    "Install the OpenSSH client (the `ssh` program) and try again.",
                ));
                return false;
            }
        };
        let guarded = Cancellable {
            inner: ssh,
            cancel: self.cancel.clone(),
        };
        let local = SetupLocal {
            links: self.links.clone(),
            handle: self.handle.clone(),
            jobs: self.jobs.clone(),
            id: self.id.clone(),
        };
        let mut engine = Engine {
            transport: &guarded,
            local: &local,
            ui: &mut ui,
            store,
            known_hosts,
            params: Params {
                record: self.record.clone(),
                mode: self.mode,
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                release_tag: option_env!("PRAXI_RELEASE_TAG").map(str::to_owned),
                desktop: true,
                retry_delay: Duration::from_secs(5),
                // An update is the owner's explicit click and the installer verifies the
                // release attestation itself; adding a new server stays behind the flag.
                allow_install: self.mode == Mode::Upgrade || install_available(),
            },
        };
        let report = engine.run();
        report.ok()
    }
}

/// Stops sending new commands once the user cancelled.
struct Cancellable {
    inner: Box<dyn Transport>,
    cancel: Arc<AtomicBool>,
}

impl Transport for Cancellable {
    fn scan_host_keys(&self) -> Result<Vec<host_provision::ScannedKey>, TransportError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(TransportError::Other("cancelled".into()));
        }
        self.inner.scan_host_keys()
    }

    fn run(
        &self,
        cmd: RemoteCmd,
        stdin: &[u8],
        timeout: Duration,
    ) -> Result<Output, TransportError> {
        // Cleanup and the rollback of a pairing still run after cancel: they
        // only remove the staged scripts and take authority back.
        let allowed = matches!(
            cmd,
            RemoteCmd::Cleanup
                | RemoteCmd::Provision(Sub::Devices | Sub::Revoke | Sub::ScreenRevoke)
        );
        if self.cancel.load(Ordering::SeqCst) && !allowed {
            return Err(TransportError::Other("cancelled".into()));
        }
        self.inner.run(cmd, stdin, timeout)
    }
}

struct JobUi<'a> {
    jobs: Jobs,
    id: String,
    cancel: Arc<AtomicBool>,
    rx: &'a mpsc::Receiver<bool>,
}

fn map_status(s: Status) -> HostSetupStepStatus {
    match s {
        Status::Pass => HostSetupStepStatus::Pass,
        Status::Warn => HostSetupStepStatus::Warn,
        Status::Fail => HostSetupStepStatus::Fail,
        Status::Skip => HostSetupStepStatus::Skip,
    }
}

impl Ui for JobUi<'_> {
    fn progress(&mut self, r: &StepResult) {
        let step = HostSetupStep {
            id: r.id.to_owned(),
            title: r.title.clone(),
            status: map_status(r.status),
            detail: r.detail.clone(),
            hint: r.remediation.clone(),
        };
        update(&self.jobs, &self.id, |s| s.job.steps.push(step));
    }

    fn note(&mut self, line: &str) {
        let line = line.to_owned();
        update(&self.jobs, &self.id, |s| {
            if s.job.notes.len() < MAX_NOTES {
                s.job.notes.push(line);
            }
        });
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn confirm_host_key(&mut self, host: &str, port: u16, fingerprint: &str) -> bool {
        if self.cancel.load(Ordering::SeqCst) {
            return false;
        }
        while self.rx.try_recv().is_ok() {}
        let prompt = HostKeyPrompt {
            host: host.to_owned(),
            port,
            fingerprint: fingerprint.to_owned(),
        };
        update(&self.jobs, &self.id, |s| {
            s.job.state = HostSetupState::AwaitingHostKey;
            s.job.host_key = Some(prompt);
        });
        let answer = self.rx.recv_timeout(KEY_WAIT);
        let accepted = answer.unwrap_or(false);
        let cancelled = self.cancel.load(Ordering::SeqCst);
        if answer.is_err() {
            self.note("no answer to the host key question within 5 minutes");
        } else if !accepted && !cancelled {
            self.note("the host key was declined");
        }
        update(&self.jobs, &self.id, |s| {
            if s.job.state == HostSetupState::AwaitingHostKey {
                s.job.state = HostSetupState::Running;
            }
            s.job.host_key = None;
        });
        accepted && !cancelled
    }
}

/// This computer's side of the engine: the local daemon's host links, called
/// directly (no loopback HTTP round trip) from the worker thread.
struct SetupLocal {
    links: Arc<CliHostLinks>,
    handle: tokio::runtime::Handle,
    jobs: Jobs,
    id: String,
}

impl LocalHost for SetupLocal {
    fn pair(&self, ticket: &str) -> Result<PairedHost, String> {
        let info = self
            .handle
            .block_on(self.links.pair(ticket))
            .map_err(describe)?;
        let (host_id, host_name) = (info.id.clone(), info.name.clone());
        update(&self.jobs, &self.id, |s| {
            s.job.host_id = Some(host_id);
            s.job.host_name = Some(host_name);
        });
        Ok(PairedHost {
            id: info.id,
            name: info.name,
        })
    }

    fn knows_host(&self, id: &str) -> bool {
        self.links.list().iter().any(|h| h.id == id)
    }

    fn health(&self, id: &str) -> Result<HostHealth, String> {
        let link = self
            .handle
            .block_on(self.links.connect(id))
            .map_err(describe)?;
        let host = Client::new(&link.base_url, link.token.clone());
        let status = self
            .handle
            .block_on(host.request(Method::GET, "/v1/host", None))
            .map_err(|e| e.to_string());
        let targets = self
            .handle
            .block_on(host.request(Method::GET, "/v1/view/targets", None))
            .ok();
        self.links.disconnect(id);
        let s: Value = status?;
        let view_targets = targets.and_then(|t| t.as_array().map(Vec::len));
        let count = view_targets.and_then(|n| u32::try_from(n).ok());
        update(&self.jobs, &self.id, |slot| slot.job.view_targets = count);
        Ok(HostHealth {
            mode: s["mode"].as_str().unwrap_or_default().to_owned(),
            version: s["version"].as_str().unwrap_or_default().to_owned(),
            has_display: s["has_display"].as_bool().unwrap_or(false),
            has_browser: s["has_browser"].as_bool().unwrap_or(false),
            view_targets,
        })
    }

    fn revoke_host(&self, _id: &str) -> Result<(), String> {
        Err("removing a server is not part of the wizard".into())
    }

    fn host_ids(&self) -> Vec<String> {
        self.links.list().into_iter().map(|h| h.id).collect()
    }

    fn forget_host(&self, id: &str) -> Result<(), String> {
        if self.handle.block_on(self.links.forget(id)) {
            Ok(())
        } else {
            Err("no such pairing".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_provision::{ScannedKey, Status as _Status};

    const ED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    /// Answers like a server whose only pending question is the host key.
    struct Fake;

    impl Transport for Fake {
        fn scan_host_keys(&self) -> Result<Vec<ScannedKey>, TransportError> {
            Ok(vec![ScannedKey {
                key_type: "ssh-ed25519".into(),
                key_b64: ED.into(),
            }])
        }
        fn run(
            &self,
            cmd: RemoteCmd,
            _stdin: &[u8],
            _t: Duration,
        ) -> Result<Output, TransportError> {
            Ok(Output {
                code: 0,
                stdout: if cmd == RemoteCmd::Probe {
                    "praxi-ok\n".into()
                } else {
                    String::new()
                },
                ..Output::default()
            })
        }
    }

    fn wait(setup: &CliHostSetup, id: &str, ok: impl Fn(&HostSetupJob) -> bool) -> HostSetupJob {
        for _ in 0..200 {
            let job = setup.job(id).unwrap();
            if ok(&job) {
                return job;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out: {:?}", setup.job(id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accepting_the_host_key_continues_and_pins_it() {
        let dir = tempfile::tempdir().unwrap();
        // A key file under a folder with a space and a non-ASCII letter (OneDrive\Masaüstü).
        let keydir = dir.path().join("Masa\u{fc}st\u{fc} oracle");
        std::fs::create_dir_all(&keydir).unwrap();
        let key = keydir.join("free-a1.key");
        std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        let data = dir.path().join("data");
        let links = Arc::new(CliHostLinks::new(data.clone(), None));
        let setup = CliHostSetup::new(data.clone(), links)
            .with_transport(Arc::new(|_, _| Ok(Box::new(Fake) as Box<dyn Transport>)));
        let job = setup
            .start(StartHostSetupInput {
                host: "203.0.113.7".into(),
                user: None,
                port: None,
                key_path: Some(key.to_string_lossy().into_owned()),
                name: None,
            })
            .unwrap();
        let waiting = wait(&setup, &job.id, |j| {
            j.state == HostSetupState::AwaitingHostKey
        });
        assert!(waiting.host_key.is_some());
        setup.answer_host_key(&job.id, true).unwrap();
        let after = wait(&setup, &job.id, |j| {
            j.steps.iter().any(|s| s.id == "connect")
        });
        let hostkey = after.steps.iter().find(|s| s.id == "hostkey").unwrap();
        assert_eq!(hostkey.status, HostSetupStepStatus::Pass, "{hostkey:?}");
        let pinned = std::fs::read_to_string(data.join("hosts/known_hosts")).unwrap();
        assert!(pinned.contains("203.0.113.7 ssh-ed25519"), "{pinned}");
        setup.cancel(&job.id).unwrap();
        let _ = _Status::Pass;
    }

    /// A server added with the wizard before: its record and a scripted server.
    struct UpdateFake {
        install_exit: i32,
        install_stderr: &'static str,
    }

    impl Transport for UpdateFake {
        fn scan_host_keys(&self) -> Result<Vec<ScannedKey>, TransportError> {
            Fake.scan_host_keys()
        }
        fn run(
            &self,
            cmd: RemoteCmd,
            _stdin: &[u8],
            _t: Duration,
        ) -> Result<Output, TransportError> {
            let (code, stdout, stderr) = match cmd {
                RemoteCmd::Probe => (
                    0,
                    "praxi-ok
"
                    .to_owned(),
                    String::new(),
                ),
                RemoteCmd::Provision(Sub::Preflight) => (
                    0,
                    "uid=1001
os_id=ubuntu
os_version=24.04
os_like=debian
os_name=Ubuntu
arch=aarch64
systemd=1
sudo_n=1
ram_mb=24000
disk_mb=150000
curl=1
https=1
gh_version=
praxi_version=0.0.1
unit_present=1
unit_hostmode=1
unit_active=active
data_dir=1
port=47821
port_busy=0
"
                    .to_owned(),
                    String::new(),
                ),
                RemoteCmd::Provision(Sub::Install) => (
                    self.install_exit,
                    String::new(),
                    self.install_stderr.to_owned(),
                ),
                _ => (0, String::new(), String::new()),
            };
            Ok(Output {
                code,
                stdout,
                stderr,
                ..Output::default()
            })
        }
    }

    fn saved_server(data: &std::path::Path) -> CliHostSetup {
        let store = Store::from_data_dir(data);
        store
            .save(&HostRecord {
                name: "oracle".into(),
                user: "ubuntu".into(),
                host: "203.0.113.7".into(),
                port: 22,
                key_path: None,
                fingerprint: None,
                paired_host_id: Some("hostid1".into()),
                desktop_key: Some("deskkey1".into()),
                installed_version: None,
            })
            .unwrap();
        let links = Arc::new(CliHostLinks::new(data.to_path_buf(), None));
        CliHostSetup::new(data.to_path_buf(), links).with_transport(Arc::new(|_, _| {
            Ok(Box::new(UpdateFake {
                install_exit: 1,
                install_stderr: "attestation verification failed: digest mismatch
[praxi-install] rollback complete; previously active services restarted and active",
            }) as Box<dyn Transport>)
        }))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_without_saved_login_details_is_refused_with_a_plain_reason() {
        let dir = tempfile::tempdir().unwrap();
        let links = Arc::new(CliHostLinks::new(dir.path().to_path_buf(), None));
        let setup = CliHostSetup::new(dir.path().to_path_buf(), links);
        let err = setup
            .start_update(StartHostUpdateInput {
                host_id: "nothere".into(),
            })
            .unwrap_err();
        assert!(matches!(err, HostSetupError::Invalid(m) if m.contains("no saved login")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_update_ends_as_failed_with_the_rollback_note_and_no_health_step() {
        let dir = tempfile::tempdir().unwrap();
        let setup = saved_server(dir.path());
        let job = setup
            .start_update(StartHostUpdateInput {
                host_id: "hostid1".into(),
            })
            .unwrap();
        assert_eq!(job.kind, HostJobKind::Update);
        wait(&setup, &job.id, |j| {
            j.state == HostSetupState::AwaitingHostKey
        });
        setup.answer_host_key(&job.id, true).unwrap();
        let done = wait(&setup, &job.id, |j| j.state == HostSetupState::Failed);
        let install = done.steps.iter().find(|s| s.id == "install.run").unwrap();
        assert_eq!(install.status, HostSetupStepStatus::Fail);
        assert!(install.detail.contains("rollback complete"), "{install:?}");
        assert!(!done.steps.iter().any(|s| s.id.starts_with("health")));
    }
}
