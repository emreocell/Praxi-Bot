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
    time::{Duration, Instant},
};

use daemon_api::{
    host_links::{HostLinkError, HostLinks},
    host_packs::{
        HostPackInfo, HostPacks, HostToolFact, PackAction, PackApplyInput, PackPlan, PackPlanInput,
        PackPlanItem,
    },
    host_setup::{
        BootstrapCommand, HostJobKind, HostKeyPrompt, HostSetup, HostSetupError, HostSetupJob,
        HostSetupState, HostSetupStep, HostSetupStepStatus, SshKeyHint, StartHostSetupInput,
        StartHostUpdateInput,
    },
};
use host_provision::{
    Engine, HostHealth, HostRecord, KnownHosts, LocalHost, Mode, Output, PairedHost, Params,
    RemoteCmd, Status, StepResult, Store, Sub, Transport, TransportError, Ui,
    packs::{self, Action, Blocked, PackError, PackStatus, Packs},
    validate,
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

struct Registered {
    job: HostSetupJob,
    id: String,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<bool>,
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

/// A dry-run preview the owner has been shown; `packs_apply` runs exactly this.
struct StoredPlan {
    id: String,
    host_id: String,
    action: Action,
    /// Pack ids whose licence notice the owner acknowledged in this plan.
    acknowledged: Vec<String>,
    /// Only the items that can run: catalog id and the hash of the exact package list the
    /// owner saw in the plan (the server refuses a different list).
    packs: Vec<(String, String)>,
    made: Instant,
}

/// How long a preview stays valid: long enough to read it, short enough that a stale one (the
/// server may have changed) is never run.
const PLAN_TTL: Duration = Duration::from_secs(15 * 60);
const KEEP_PLANS: usize = 8;

pub struct CliHostSetup {
    data: PathBuf,
    links: Arc<CliHostLinks>,
    jobs: Jobs,
    plans: Mutex<Vec<StoredPlan>>,
    transport: TransportFactory,
}

impl CliHostSetup {
    pub fn new(data: PathBuf, links: Arc<CliHostLinks>) -> Self {
        Self {
            data,
            links,
            jobs: Arc::new(Mutex::new(VecDeque::new())),
            plans: Mutex::new(Vec::new()),
            transport: Arc::new(|store, record| {
                transport(store, record)
                    .map(|t| Box::new(t) as Box<dyn Transport>)
                    .map_err(|e| e.to_string())
            }),
        }
    }

    /// Reserve the one job slot (only one server job runs at a time).
    fn register(
        &self,
        record: &HostRecord,
        kind: HostJobKind,
    ) -> Result<Registered, HostSetupError> {
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
        Ok(Registered {
            job,
            id,
            cancel,
            rx,
        })
    }

    fn launch(
        &self,
        record: HostRecord,
        mode: Mode,
        kind: HostJobKind,
        base_packs: bool,
    ) -> Result<HostSetupJob, HostSetupError> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| HostSetupError::Conflict("the daemon is shutting down".into()))?;
        let Registered {
            job,
            id,
            cancel,
            rx,
        } = self.register(&record, kind)?;
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
            base_packs,
        };
        std::thread::Builder::new()
            .name("host-setup".into())
            .spawn(move || worker.run())
            .map_err(|e| HostSetupError::Conflict(format!("could not start the setup: {e}")))?;
        Ok(job)
    }

    /// The SSH details the "Add server" wizard stored for a paired server (nothing new is saved).
    /// The stored file is plain JSON on disk: every field that reaches ssh's argv is re-checked.
    fn saved_record(&self, host_id: &str) -> Result<HostRecord, HostSetupError> {
        let store = Store::from_data_dir(&self.data);
        let mut record = store
            .list()
            .into_iter()
            .find(|r| r.paired_host_id.as_deref() == Some(host_id))
            .ok_or_else(|| {
                HostSetupError::Invalid(
                    "no saved login details for this server on this computer".into(),
                )
            })?;
        validate::host(&record.host).map_err(invalid)?;
        validate::user(&record.user).map_err(invalid)?;
        validate::port(u32::from(record.port)).map_err(invalid)?;
        if let Some(key) = record.key_path.take() {
            record.key_path = Some(validate::key_path(&key).map_err(invalid)?);
        }
        Ok(record)
    }

    fn ssh_for(&self, record: &HostRecord) -> Result<Box<dyn Transport>, HostSetupError> {
        let store = Store::from_data_dir(&self.data);
        (self.transport)(&store, record)
            .map_err(|e| HostSetupError::Invalid(format!("the SSH client is not available: {e}")))
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
        self.launch(record, Mode::Add, HostJobKind::Setup, input.base_packs)
    }

    fn start_update(&self, input: StartHostUpdateInput) -> Result<HostSetupJob, HostSetupError> {
        let record = self.saved_record(&input.host_id)?;
        self.launch(record, Mode::Upgrade, HostJobKind::Update, false)
    }

    fn packs(&self, host_id: &str) -> Result<HostPacks, HostSetupError> {
        let record = self.saved_record(host_id)?;
        let ssh = self.ssh_for(&record)?;
        let cat = packs::catalog();
        let mut view = HostPacks {
            host_id: host_id.to_owned(),
            supported: false,
            can_admin: false,
            packs: cat
                .iter()
                .map(|p| pack_info(p, PackStatus::Missing))
                .collect(),
            tools: Vec::new(),
            error: None,
            error_detail: None,
        };
        match (Packs { transport: &*ssh }).facts() {
            Ok(f) => {
                view.supported = f.supported;
                view.can_admin = f.can_admin;
                view.packs = cat.iter().map(|p| pack_info(p, f.status(&p.id))).collect();
                view.tools = f
                    .tools
                    .into_iter()
                    .map(|(name, present)| HostToolFact { name, present })
                    .collect();
                if !f.supported {
                    view.error = Some("unsupported".into());
                }
            }
            Err(e) => {
                view.error = Some(e.code().into());
                view.error_detail = Some(e.detail());
            }
        }
        Ok(view)
    }

    fn packs_plan(&self, input: PackPlanInput) -> Result<PackPlan, HostSetupError> {
        let record = self.saved_record(&input.host_id)?;
        let catalog = packs::catalog();
        let mut wanted: Vec<&packs::Pack> = Vec::new();
        for id in &input.packs {
            let pack = catalog
                .iter()
                .find(|p| &p.id == id)
                .ok_or_else(|| HostSetupError::Invalid("unknown tool pack".into()))?;
            if !wanted.iter().any(|p| p.id == pack.id) {
                wanted.push(pack);
            }
        }
        let action = match input.action {
            PackAction::Install => Action::Install,
            PackAction::Remove => Action::Remove,
        };
        let ssh = self.ssh_for(&record)?;
        let runner = Packs { transport: &*ssh };
        // One look at the server first: no apt, or no way to run administrator commands, makes
        // every item fail the same way, so say that once instead of failing pack by pack.
        let server = runner.facts();
        let items: Vec<PackPlanItem> = wanted
            .iter()
            .map(|pack| {
                let blocked = |code: &str| PackPlanItem {
                    pack: pack.id.clone(),
                    packages: Vec::new(),
                    total_packages: 0,
                    size_mb: pack.size_mb,
                    noop: false,
                    blocked: Some(code.to_owned()),
                };
                let acked = input.acknowledged.contains(&pack.id);
                match &server {
                    _ if action == Action::Install && pack.installable(acked).is_err() => {
                        blocked(PackError::NeedsAcknowledgement.code())
                    }
                    Err(e) => blocked(e.code()),
                    Ok(f) if !f.supported => blocked("unsupported"),
                    Ok(f) if action == Action::Install && !f.can_admin => blocked("needs_admin"),
                    Ok(_) => match runner.plan(&pack.id, action) {
                        Ok(plan) => PackPlanItem {
                            pack: plan.pack.clone(),
                            noop: plan.is_noop(),
                            blocked: plan.blocked.map(|b| blocked_code(b).to_owned()),
                            packages: plan.packages,
                            total_packages: plan.total_packages,
                            size_mb: plan.size_mb,
                        },
                        Err(e) => blocked(e.code()),
                    },
                }
            })
            .collect();
        let runnable: Vec<(String, String)> = items
            .iter()
            .filter(|i| !i.noop && i.blocked.is_none())
            .filter_map(|i| Some((i.pack.clone(), packs::list_sha(&i.pack, action)?)))
            .collect();
        let size_mb = if action == Action::Install {
            items
                .iter()
                .filter(|i| runnable.iter().any(|(id, _)| *id == i.pack))
                .map(|i| i.size_mb)
                .sum()
        } else {
            0
        };
        let plan_id = uuid::Uuid::new_v4().to_string();
        if !runnable.is_empty() {
            let mut plans = self.plans.lock();
            plans.retain(|s| s.made.elapsed() < PLAN_TTL);
            while plans.len() >= KEEP_PLANS {
                plans.remove(0);
            }
            plans.push(StoredPlan {
                id: plan_id.clone(),
                host_id: input.host_id.clone(),
                action,
                acknowledged: input.acknowledged.clone(),
                packs: runnable.clone(),
                made: Instant::now(),
            });
        }
        Ok(PackPlan {
            plan_id,
            host_id: input.host_id,
            action: input.action,
            runnable: !runnable.is_empty(),
            items,
            size_mb,
            expires_in_secs: u32::try_from(PLAN_TTL.as_secs()).unwrap_or(900),
        })
    }

    fn packs_apply(&self, input: PackApplyInput) -> Result<HostSetupJob, HostSetupError> {
        // Single use: the plan is taken out whether or not the job then starts, so the owner
        // always approves what runs.
        let plan = {
            let mut plans = self.plans.lock();
            let at = plans
                .iter()
                .position(|s| s.id == input.plan_id)
                .ok_or_else(|| {
                    HostSetupError::Invalid(
                        "this preview is no longer valid; look at what would change again".into(),
                    )
                })?;
            plans.remove(at)
        };
        if plan.made.elapsed() >= PLAN_TTL {
            return Err(HostSetupError::Invalid(
                "this preview has expired; look at what would change again".into(),
            ));
        }
        let record = self.saved_record(&plan.host_id)?;
        let Registered {
            job,
            id,
            cancel,
            rx: _,
        } = self.register(&record, HostJobKind::Packs)?;
        let jobs = self.jobs.clone();
        let factory = self.transport.clone();
        let store = Store::from_data_dir(&self.data);
        let spawned = std::thread::Builder::new()
            .name("host-packs".into())
            .spawn(move || {
                // The SSH transport is built on this thread (it is not `Send`).
                let ok = match factory(&store, &record) {
                    Ok(inner) => run_packs(
                        &jobs,
                        &id,
                        &Cancellable {
                            inner,
                            cancel: cancel.clone(),
                        },
                        &plan.packs,
                        &plan.acknowledged,
                        plan.action,
                        &cancel,
                    ),
                    Err(e) => {
                        let step = HostSetupStep {
                            id: "pack.ssh".into(),
                            title: "SSH client".into(),
                            status: HostSetupStepStatus::Fail,
                            detail: "connection".into(),
                            hint: Some(e),
                        };
                        update(&jobs, &id, |s| s.job.steps.push(step));
                        false
                    }
                };
                let cancelled = cancel.load(Ordering::SeqCst);
                update(&jobs, &id, |s| {
                    s.job.state = if cancelled {
                        HostSetupState::Cancelled
                    } else if ok {
                        HostSetupState::Succeeded
                    } else {
                        HostSetupState::Failed
                    };
                });
            });
        if let Err(e) = spawned {
            update(&self.jobs, &job.id, |s| {
                s.job.state = HostSetupState::Failed;
            });
            return Err(HostSetupError::Conflict(format!(
                "could not start the install: {e}"
            )));
        }
        Ok(job)
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

    fn bootstrap(&self) -> Result<BootstrapCommand, HostSetupError> {
        bootstrap_for(option_env!("PRAXI_RELEASE_TAG"), env!("CARGO_PKG_VERSION"))
    }
}

/// The paste-on-the-server command for the release this app was built from.
/// A build without a release tag still gets a command (the tag derived from
/// its version), flagged as not published so the app can say so.
fn bootstrap_for(
    release_tag: Option<&str>,
    app_version: &str,
) -> Result<BootstrapCommand, HostSetupError> {
    let published = release_tag.is_some_and(|t| !t.is_empty());
    let tag = release_tag.filter(|t| !t.is_empty()).unwrap_or(app_version);
    let tag = host_provision::validate::version_tag(tag)
        .map_err(|e| HostSetupError::Invalid(e.to_string()))?;
    let command = host_provision::remote::bootstrap_command(&tag)
        .map_err(|e| HostSetupError::Invalid(e.to_string()))?;
    Ok(BootstrapCommand {
        release_tag: tag,
        command,
        ticket_minutes: 30,
        published,
    })
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

fn pack_info(p: &packs::Pack, status: PackStatus) -> HostPackInfo {
    let lic = p.license.as_ref();
    HostPackInfo {
        id: p.id.clone(),
        status: match status {
            PackStatus::Installed => "installed",
            PackStatus::Partial => "partial",
            PackStatus::Missing => "missing",
        }
        .into(),
        size_mb: p.size_mb,
        base: p.base,
        removable: p.removable,
        license: lic.map(|l| l.spdx.clone()).unwrap_or_default(),
        copyleft: lic.is_some_and(|l| l.copyleft),
        distro: lic.is_some_and(|l| l.distro),
        needs_ack: p.needs_acknowledgement(),
        source_url: lic.map(|l| l.source_url.clone()).unwrap_or_default(),
        notice_url: lic.map(|l| l.notice_url.clone()).unwrap_or_default(),
        version: lic.map(|l| l.version.clone()).unwrap_or_default(),
        lock_sha256: (p.id == packs::SCRAPE).then(packs::scrape_lock_sha),
    }
}

fn blocked_code(b: Blocked) -> &'static str {
    match b {
        Blocked::Shared => "shared",
        Blocked::Dependants => "dependants",
        Blocked::Unavailable => "unavailable",
        Blocked::NeedsPython => "needs_python",
    }
}

/// Install or remove packs one after the other, one finished step each (`pack.<id>`: `detail` is
/// the result code the app turns into a sentence, `hint` the technical detail). Stops at the first
/// failure: apt problems (lock, offline, disk) would repeat for every following pack. Only
/// [`CliHostSetup::packs_apply`] calls this, with the packs and list hashes of an approved plan.
fn run_packs(
    jobs: &Jobs,
    job_id: &str,
    ssh: &dyn Transport,
    approved: &[(String, String)],
    acknowledged: &[String],
    action: Action,
    cancel: &AtomicBool,
) -> bool {
    let runner = Packs { transport: ssh };
    let (verb, title) = match action {
        Action::Install => ("installing", "Install"),
        Action::Remove => ("removing", "Remove"),
    };
    for (pack, list_sha) in approved {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        update(jobs, job_id, |s| {
            if s.job.notes.len() < MAX_NOTES {
                s.job.notes.push(format!("{verb} {pack}"));
            }
        });
        let result = runner.apply_acknowledged(
            pack,
            action,
            list_sha,
            acknowledged.iter().any(|a| a == pack),
        );
        let ok = result.is_ok();
        let step = HostSetupStep {
            id: format!("pack.{pack}"),
            title: format!("{title} {pack}"),
            status: if ok {
                HostSetupStepStatus::Pass
            } else {
                HostSetupStepStatus::Fail
            },
            detail: match &result {
                Ok(()) => "done".into(),
                Err(e) => e.code().into(),
            },
            hint: result.err().map(|e: PackError| e.detail()),
        };
        update(jobs, job_id, |s| s.job.steps.push(step));
        if !ok {
            return false;
        }
    }
    true
}

fn describe(e: HostLinkError) -> String {
    match e {
        HostLinkError::NotFound => "unknown host".into(),
        HostLinkError::ProtocolMismatch => {
            "install the same Praxi release on both devices, then make a new pairing invite".into()
        }
        HostLinkError::Incompatible => {
            "the server runs a different Praxi release; update it first".into()
        }
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
    /// The owner ticked the basic tool set. Only an intent: the app opens the tools plan after the
    /// server is added; nothing is installed from here.
    base_packs: bool,
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
        let ok = report.ok();
        drop(engine);
        if ok && self.base_packs {
            record_base_set_intent(&self.jobs, &self.id);
        }
        ok
    }
}

/// The wizard never installs anything: a basic-set request only leaves this finished step
/// (`packs.plan`, detail `plan_required`) and the app then opens the tools plan, where the owner
/// sees the dry run and approves it (`packs_plan` and `packs_apply`). A scripted
/// `POST /v1/hosts/setup` with `base_packs: true` therefore installs nothing either.
fn record_base_set_intent(jobs: &Jobs, job_id: &str) {
    let step = HostSetupStep {
        id: "packs.plan".into(),
        title: "Tools".into(),
        status: HostSetupStepStatus::Warn,
        detail: "plan_required".into(),
        hint: Some(packs::base_ids().join(",")),
    };
    update(jobs, job_id, |s| s.job.steps.push(step));
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

    #[test]
    fn bootstrap_uses_the_release_tag_and_flags_a_development_build() {
        let rel = bootstrap_for(Some("v1.2.3"), "9.9.9").unwrap();
        assert_eq!((rel.release_tag.as_str(), rel.published), ("v1.2.3", true));
        assert!(rel.command.contains(
            "PRAXI_TAG=v1.2.3
"
        ));
        let dev = bootstrap_for(None, "0.1.0").unwrap();
        assert_eq!((dev.release_tag.as_str(), dev.published), ("v0.1.0", false));
        assert!(bootstrap_for(Some("v1; rm -rf /"), "0.1.0").is_err());
    }

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
                base_packs: false,
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

    /// A server with apt for the pack tests; records every command it was asked to run.
    struct PackFake {
        admin: bool,
        install_exit: i32,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Transport for PackFake {
        fn scan_host_keys(&self) -> Result<Vec<ScannedKey>, TransportError> {
            Fake.scan_host_keys()
        }
        fn run(
            &self,
            cmd: RemoteCmd,
            stdin: &[u8],
            _t: Duration,
        ) -> Result<Output, TransportError> {
            // Uploads carry whole scripts on stdin; only what actually runs is recorded.
            if matches!(cmd, RemoteCmd::Provision(_)) {
                self.log.lock().push(format!(
                    "{} {}",
                    cmd.command_line(),
                    String::from_utf8_lossy(stdin).replace('\n', " ")
                ));
            }
            let (code, stdout, stderr) = match cmd {
                RemoteCmd::Probe => (0, "praxi-ok\n".to_owned(), String::new()),
                RemoteCmd::Provision(Sub::PacksFacts) => (
                    0,
                    format!(
                        "apt=1\nadmin={}\nos_id=ubuntu\npack_office=missing\npack_docs=installed\npack_ocr=missing\npack_media=missing\npack_dev=installed\ntool_pandoc=1\n",
                        u8::from(self.admin)
                    ),
                    String::new(),
                ),
                RemoteCmd::Provision(Sub::PacksPlan) => {
                    let text = String::from_utf8_lossy(stdin);
                    if text.contains("pack=docs") {
                        (0, "missing=\nunavailable=\n".to_owned(), String::new())
                    } else {
                        (
                            0,
                            "missing=libreoffice-writer,fonts-noto-core\nunavailable=\nnew_packages=240\n"
                                .to_owned(),
                            String::new(),
                        )
                    }
                }
                RemoteCmd::Provision(Sub::PackInstall) => (
                    self.install_exit,
                    String::new(),
                    if self.install_exit == 0 {
                        String::new()
                    } else {
                        "E: Could not get lock /var/lib/dpkg/lock".to_owned()
                    },
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

    fn packs_setup(
        data: &std::path::Path,
        admin: bool,
        install_exit: i32,
    ) -> (CliHostSetup, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
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
                desktop_key: None,
                installed_version: None,
            })
            .unwrap();
        let links = Arc::new(CliHostLinks::new(data.to_path_buf(), None));
        let setup =
            CliHostSetup::new(data.to_path_buf(), links).with_transport(Arc::new(move |_, _| {
                Ok(Box::new(PackFake {
                    admin,
                    install_exit,
                    log: l.clone(),
                }) as Box<dyn Transport>)
            }));
        (setup, log)
    }

    fn plan_input(action: PackAction, packs: &[&str]) -> PackPlanInput {
        PackPlanInput {
            host_id: "hostid1".into(),
            action,
            packs: packs.iter().map(|p| (*p).to_owned()).collect(),
            acknowledged: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pack_install_runs_only_the_previewed_runnable_items_once_and_only_as_root_via_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, log) = packs_setup(dir.path(), true, 0);
        let facts = setup.packs("hostid1").unwrap();
        assert!(facts.supported && facts.can_admin && facts.error.is_none());
        assert!(facts.tools.iter().any(|t| t.name == "pandoc" && t.present));
        // docs is already installed: a no-op, so it is previewed but never run
        let plan = setup
            .packs_plan(plan_input(PackAction::Install, &["office", "docs"]))
            .unwrap();
        assert!(plan.runnable);
        assert_eq!(plan.items.iter().filter(|i| i.noop).count(), 1);
        assert_eq!(plan.size_mb, 700);
        assert!(
            !log.lock().iter().any(|l| l.contains("pack-install")),
            "a dry run must not install anything"
        );
        let job = setup
            .packs_apply(PackApplyInput {
                plan_id: plan.plan_id.clone(),
            })
            .unwrap();
        assert_eq!(job.kind, HostJobKind::Packs);
        let done = wait(&setup, &job.id, |j| j.state == HostSetupState::Succeeded);
        assert_eq!(done.steps.len(), 1, "{:?}", done.steps);
        assert_eq!(done.steps[0].id, "pack.office");
        let installs: Vec<String> = log
            .lock()
            .iter()
            .filter(|l| l.contains("pack-install"))
            .cloned()
            .collect();
        assert_eq!(installs.len(), 1, "{installs:?}");
        assert!(installs[0].contains("sudo -n") && installs[0].contains("pack=office"));
        let sha = packs::list_sha("office", Action::Install).unwrap();
        assert!(
            installs[0].contains(&format!("plan={sha}")),
            "the install carries the hash of the previewed list: {installs:?}"
        );
        // a plan is single use
        let again = setup.packs_apply(PackApplyInput {
            plan_id: plan.plan_id,
        });
        assert!(matches!(again, Err(HostSetupError::Invalid(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_administrator_rights_the_plan_says_so_and_cannot_be_applied() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, log) = packs_setup(dir.path(), false, 0);
        let plan = setup
            .packs_plan(plan_input(PackAction::Install, &["office"]))
            .unwrap();
        assert!(!plan.runnable);
        assert_eq!(plan.items[0].blocked.as_deref(), Some("needs_admin"));
        let tried = setup.packs_apply(PackApplyInput {
            plan_id: plan.plan_id,
        });
        assert!(matches!(tried, Err(HostSetupError::Invalid(_))));
        assert!(!log.lock().iter().any(|l| l.contains("pack-install")));
        // a name that is not in the catalog never reaches the server
        let bad = setup.packs_plan(plan_input(PackAction::Install, &["libreoffice-writer"]));
        assert!(matches!(bad, Err(HostSetupError::Invalid(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_pack_install_is_a_failed_job_with_a_plain_code() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, _log) = packs_setup(dir.path(), true, 100);
        let plan = setup
            .packs_plan(plan_input(PackAction::Install, &["office"]))
            .unwrap();
        let job = setup
            .packs_apply(PackApplyInput {
                plan_id: plan.plan_id,
            })
            .unwrap();
        let done = wait(&setup, &job.id, |j| j.state == HostSetupState::Failed);
        assert_eq!(done.steps[0].status, HostSetupStepStatus::Fail);
        assert_eq!(done.steps[0].detail, "failed");
        assert!(done.steps[0].hint.as_deref().unwrap().contains("dpkg/lock"));
        // a multi-pack run stops at the first failure
        let jobs: Jobs = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, _rx) = mpsc::channel();
        let mut fresh = done.clone();
        fresh.steps.clear();
        jobs.lock().push_back(Slot {
            job: fresh,
            cancel: Arc::new(AtomicBool::new(false)),
            answer: tx,
        });
        let approved: Vec<(String, String)> = ["office", "media"]
            .iter()
            .map(|id| {
                (
                    (*id).to_owned(),
                    packs::list_sha(id, Action::Install).unwrap(),
                )
            })
            .collect();
        let ok = run_packs(
            &jobs,
            &done.id,
            &PackFake {
                admin: true,
                install_exit: 100,
                log: Arc::default(),
            },
            &approved,
            &[],
            Action::Install,
            &AtomicBool::new(false),
        );
        assert!(!ok);
        let steps = jobs.lock()[0].job.steps.clone();
        assert_eq!(steps.len(), 1, "stops at the first failure");
        assert_eq!(steps[0].status, HostSetupStepStatus::Fail);
    }

    #[test]
    fn the_wizard_never_installs_the_basic_set_it_only_asks_for_a_plan() {
        let jobs: Jobs = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, _rx) = mpsc::channel();
        let job = HostSetupJob {
            id: "j1".into(),
            kind: HostJobKind::Setup,
            state: HostSetupState::Running,
            target: "u@h:22".into(),
            steps: Vec::new(),
            notes: Vec::new(),
            host_key: None,
            host_id: None,
            host_name: None,
            view_targets: None,
            install_available: false,
        };
        jobs.lock().push_back(Slot {
            job,
            cancel: Arc::new(AtomicBool::new(false)),
            answer: tx,
        });
        // `record_base_set_intent` takes no transport: it cannot reach a server at all.
        record_base_set_intent(&jobs, "j1");
        let steps = jobs.lock()[0].job.steps.clone();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "packs.plan");
        assert_eq!(steps[0].detail, "plan_required");
        assert_eq!(steps[0].status, HostSetupStepStatus::Warn);
        assert_eq!(steps[0].hint.as_deref(), Some("office,docs,media,dev"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plan_is_mandatory_and_the_server_gets_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, log) = packs_setup(dir.path(), true, 0);
        // no plan, no install: an unknown plan id starts nothing
        let err = setup
            .packs_apply(PackApplyInput {
                plan_id: "not-a-plan".into(),
            })
            .unwrap_err();
        assert!(matches!(err, HostSetupError::Invalid(_)), "{err:?}");
        assert!(
            log.lock().iter().all(|l| !l.contains("pack-install")),
            "{:?}",
            log.lock()
        );
    }
}
