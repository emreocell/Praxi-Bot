//! In-app updates: check, release notes, one-click install, rollback.
//!
//! Everything that downloads or installs goes through `tauri-plugin-updater`,
//! which verifies the minisign signature of the installer against the public
//! key built into this app BEFORE anything is installed. This module adds:
//!
//! * a config flag: with no public key (`plugins.updater.pubkey` in
//!   `tauri.conf.json`, or `PRAXI_UPDATER_PUBKEY` set at build time) and no
//!   https endpoint the whole feature is off and no network request is made -
//!   an update is never applied unsigned;
//! * language-aware release notes (plain text, never HTML);
//! * start-up bookkeeping so a new version that keeps failing to start can be
//!   rolled back to the last good, signed installer from a second feed.
//!
//! The pure parts (settings, notes, state machine) are unit tested below; the
//! plugin glue is covered by `tests/update_server.rs` against a mock server.

#![allow(clippy::missing_errors_doc)] // errors are short codes, listed in `describe` and `enabled`

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State, Url, Window};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::window_gate::{MAIN_ONLY, require_window};

/// Starts of a freshly installed version without a healthy run after which the
/// app offers to go back.
pub const ADVISE_ROLLBACK_AFTER_STARTS: u32 = 2;
/// Starts after which rollback advice is urgent. Still requires a user click.
pub const URGENT_ROLLBACK_AFTER_STARTS: u32 = 3;
/// A start counts as healthy once the app has been running this long.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(60);
/// Release notes are shown as plain text; anything longer is cut.
const MAX_NOTES_CHARS: usize = 8000;
const STATE_FILE: &str = "update-state.json";

// ---------------------------------------------------------------- settings

/// Where updates come from. `None` from [`UpdateSettings::resolve`] means
/// "updates are not configured": nothing is checked, nothing is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSettings {
    /// Contents of the minisign public key file (what `tauri signer generate` prints).
    pub pubkey: String,
    /// Feed in Tauri `latest.json` format for the newest release.
    pub endpoint: Url,
    /// Feed in the same format pointing at the last known-good release.
    pub rollback_endpoint: Option<Url>,
    /// Separate feed, contacted only after the user chooses Preview.
    pub preview_endpoint: Option<Url>,
}

impl UpdateSettings {
    /// Settings from the app configuration (`plugins.updater` in
    /// `tauri.conf.json`, compiled into the app), a build-time override
    /// (`PRAXI_UPDATER_*` set while compiling) and, in debug builds only,
    /// environment overrides so tests and developers can point the app at a mock
    /// server. Release builds ignore the environment: nobody can redirect the feed.
    #[must_use]
    pub fn from_build(config: Option<&serde_json::Value>) -> Option<Self> {
        let configured = ConfiguredFeeds::read(config);
        let built = |value: Option<&'static str>, fallback: &Option<String>| {
            value
                .filter(|v| !v.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| fallback.clone())
        };
        let pubkey = built(option_env!("PRAXI_UPDATER_PUBKEY"), &configured.pubkey);
        let endpoint = built(option_env!("PRAXI_UPDATER_ENDPOINT"), &configured.endpoint);
        let rollback = built(
            option_env!("PRAXI_UPDATER_ROLLBACK_ENDPOINT"),
            &configured.rollback_endpoint,
        );
        let preview = built(
            option_env!("PRAXI_UPDATER_PREVIEW_ENDPOINT"),
            &configured.preview_endpoint,
        );
        Self::resolve(
            pubkey.as_deref(),
            endpoint.as_deref(),
            rollback.as_deref(),
            preview.as_deref(),
            |name| {
                if cfg!(debug_assertions) {
                    std::env::var(name).ok()
                } else {
                    None
                }
            },
            cfg!(debug_assertions),
        )
    }

    /// Pure resolver (unit tested). An environment value wins over the build
    /// value. Endpoints must be https; plain http is accepted only for a
    /// loopback host and only when `allow_loopback_http` (debug builds).
    #[must_use]
    pub fn resolve(
        build_pubkey: Option<&str>,
        build_endpoint: Option<&str>,
        build_rollback: Option<&str>,
        build_preview: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
        allow_loopback_http: bool,
    ) -> Option<Self> {
        let pick = |name: &str, built: Option<&str>| {
            env(name)
                .or_else(|| built.map(str::to_owned))
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let pubkey = pick("PRAXI_UPDATER_PUBKEY", build_pubkey)?;
        let endpoint = parse_endpoint(
            &pick("PRAXI_UPDATER_ENDPOINT", build_endpoint)?,
            allow_loopback_http,
        )?;
        let rollback_endpoint = pick("PRAXI_UPDATER_ROLLBACK_ENDPOINT", build_rollback)
            .and_then(|v| parse_endpoint(&v, allow_loopback_http));
        let preview_endpoint = pick("PRAXI_UPDATER_PREVIEW_ENDPOINT", build_preview)
            .and_then(|v| parse_endpoint(&v, allow_loopback_http));
        Some(Self {
            pubkey,
            endpoint,
            rollback_endpoint,
            preview_endpoint,
        })
    }
}

/// The update values of the app configuration: the standard `pubkey` and
/// `endpoints` of the updater plugin (the first endpoint is the Stable feed)
/// plus Praxi's `praxiPreviewEndpoint` and `praxiRollbackEndpoint`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ConfiguredFeeds {
    pubkey: Option<String>,
    endpoint: Option<String>,
    preview_endpoint: Option<String>,
    rollback_endpoint: Option<String>,
}

impl ConfiguredFeeds {
    fn read(config: Option<&serde_json::Value>) -> Self {
        let text = |value: Option<&serde_json::Value>| {
            value
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let get = |key: &str| config.and_then(|c| c.get(key));
        Self {
            pubkey: text(get("pubkey")),
            endpoint: text(get("endpoints").and_then(|e| e.get(0))),
            preview_endpoint: text(get("praxiPreviewEndpoint")),
            rollback_endpoint: text(get("praxiRollbackEndpoint")),
        }
    }
}

fn parse_endpoint(raw: &str, allow_loopback_http: bool) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    match url.scheme() {
        "https" => Some(url),
        "http" if allow_loopback_http => {
            let loopback = matches!(
                url.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            );
            loopback.then_some(url)
        }
        _ => None,
    }
}

// ------------------------------------------------------------ release notes

/// Picks the release notes for `lang` ("tr", "de-DE", ...).
///
/// A release body may hold one block per language, each opened by a line
/// `<!-- lang: tr -->`. Text before the first marker is the default. Order of
/// preference: the user's language, English, the unmarked text, the first
/// block. The result is plain text, trimmed and length-limited.
#[must_use]
pub fn pick_notes(body: &str, lang: &str) -> String {
    let mut default = String::new();
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        if let Some(code) = lang_marker(line) {
            blocks.push((code, String::new()));
        } else if let Some((_, text)) = blocks.last_mut() {
            text.push_str(line);
            text.push('\n');
        } else {
            default.push_str(line);
            default.push('\n');
        }
    }
    let wanted = lang.split(['-', '_']).next().unwrap_or("").to_lowercase();
    let find = |code: &str| {
        blocks
            .iter()
            .find(|(c, text)| c == code && !text.trim().is_empty())
            .map(|(_, text)| text.as_str())
    };
    let chosen = find(&wanted)
        .or_else(|| find("en"))
        .or_else(|| (!default.trim().is_empty()).then_some(default.as_str()))
        .or_else(|| {
            blocks
                .iter()
                .map(|(_, text)| text.as_str())
                .find(|text| !text.trim().is_empty())
        })
        .unwrap_or("");
    chosen.trim().chars().take(MAX_NOTES_CHARS).collect()
}

fn lang_marker(line: &str) -> Option<String> {
    let inner = line
        .trim()
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim()
        .strip_prefix("lang:")?;
    let code = inner.trim().to_lowercase();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphabetic() || c == '-')).then_some(code)
}

// ------------------------------------------------------ start-up bookkeeping

/// Written just before an installer is launched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    pub from_version: String,
    pub to_version: String,
    /// Starts of `to_version` that did not reach a healthy run yet.
    pub starts: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    pub pending: Option<PendingUpdate>,
    pub channel: UpdateChannel,
    pub skipped_version: Option<String>,
    pub last_checked: Option<u64>,
    pub release_notes: Option<SavedReleaseNotes>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedReleaseNotes {
    pub version: String,
    pub body: String,
}

/// What this start looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartVerdict {
    /// Not a first run after an update.
    Normal,
    /// First run(s) of a new version; not yet confirmed healthy.
    Probation,
    /// The new version failed to reach a healthy run repeatedly: offer to go back.
    RollbackAdvised,
    /// Failed again: urgently recommend going back, with the user's click.
    RollbackUrgent,
}

impl UpdateState {
    /// Called once per process start with the running version.
    pub fn begin_start(&mut self, current: &str) -> StartVerdict {
        match &mut self.pending {
            Some(p) if p.to_version == current => {
                p.starts += 1;
                if p.starts >= URGENT_ROLLBACK_AFTER_STARTS {
                    StartVerdict::RollbackUrgent
                } else if p.starts >= ADVISE_ROLLBACK_AFTER_STARTS {
                    StartVerdict::RollbackAdvised
                } else {
                    StartVerdict::Probation
                }
            }
            Some(_) => {
                // The installer did not run, or we are already back on the old version.
                self.pending = None;
                StartVerdict::Normal
            }
            None => StartVerdict::Normal,
        }
    }

    /// The running version stayed up long enough: the update is good.
    pub fn confirm_healthy(&mut self, current: &str) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.to_version == current)
        {
            self.pending = None;
        }
    }

    pub fn record_install(&mut self, from: &str, to: &str) {
        self.pending = Some(PendingUpdate {
            from_version: from.to_owned(),
            to_version: to.to_owned(),
            starts: 0,
        });
    }

    fn record_verified_update(&mut self, from: &str, to: &str, body: &str) {
        self.record_install(from, to);
        self.release_notes = Some(SavedReleaseNotes {
            version: to.to_owned(),
            body: body.chars().take(MAX_NOTES_CHARS * 4).collect(),
        });
    }

    /// Previous version of an update that is still on probation.
    #[must_use]
    pub fn previous_version(&self, current: &str) -> Option<String> {
        self.pending
            .as_ref()
            .filter(|p| p.to_version == current)
            .map(|p| p.from_version.clone())
    }

    #[must_use]
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join(STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// # Errors
    /// When the state file cannot be written.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, dir.join(STATE_FILE))
    }
}

// -------------------------------------------------------------- tauri glue

/// Shared state of the update commands.
pub struct UpdateRuntime {
    pub settings: Option<UpdateSettings>,
    pub verdict: StartVerdict,
    pub current_version: String,
    state_dir: Option<PathBuf>,
    state: Mutex<UpdateState>,
    found: Mutex<Option<Update>>,
    busy: tokio::sync::Mutex<()>,
}

impl UpdateRuntime {
    // Save before publishing a change in memory so a failed write is reported honestly.
    fn change_state(&self, change: impl FnOnce(&mut UpdateState)) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "failed".to_owned())?;
        let mut next = state.clone();
        change(&mut next);
        if let Some(dir) = &self.state_dir {
            next.save(dir).map_err(|_| "failed".to_owned())?;
        }
        *state = next;
        Ok(())
    }

    /// Reads the state file and counts this start.
    #[must_use]
    pub fn start(settings: Option<UpdateSettings>, current_version: &str) -> Self {
        let state_dir = storage::data_dir();
        let mut state = state_dir
            .as_deref()
            .map(UpdateState::load)
            .unwrap_or_default();
        let verdict = state.begin_start(current_version);
        let runtime = Self {
            settings,
            verdict,
            current_version: current_version.to_owned(),
            state_dir,
            state: Mutex::new(state),
            found: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
        };
        runtime.persist();
        runtime
    }

    /// A runtime that touches no files (tests with a mock server).
    #[doc(hidden)]
    #[must_use]
    pub fn detached(settings: Option<UpdateSettings>, current_version: &str) -> Self {
        Self {
            settings,
            verdict: StartVerdict::Normal,
            current_version: current_version.to_owned(),
            state_dir: None,
            state: Mutex::new(UpdateState::default()),
            found: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn pending(&self) -> Option<PendingUpdate> {
        self.state.lock().ok().and_then(|s| s.pending.clone())
    }

    fn persist(&self) {
        if let (Some(dir), Ok(state)) = (&self.state_dir, self.state.lock())
            && let Err(e) = state.save(dir)
        {
            eprintln!("bot-desktop: cannot save update state: {e}");
        }
    }

    /// Called by the health timer once the app has run long enough.
    pub fn mark_healthy(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.confirm_healthy(&self.current_version);
        }
        self.persist();
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // independent updater capabilities, not mutually exclusive states
pub struct UpdateStatus {
    /// False when this build has no update feed / signing key configured.
    pub enabled: bool,
    pub current_version: String,
    pub build_id: String,
    pub channel: UpdateChannel,
    pub preview_available: bool,
    pub last_checked: Option<u64>,
    pub skipped_version: Option<String>,
    pub installed_notes: Option<UpdateInfo>,
    /// The version we updated from while the new one is still on probation.
    pub previous_version: Option<String>,
    /// The new version failed to start more than once: show "go back".
    pub rollback_advised: bool,
    pub rollback_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    /// Plain text in the user's language when the release has it.
    pub notes: String,
    pub date: Option<String>,
}

/// Short, stable codes; the UI turns them into plain-language messages.
fn describe(e: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error as E;
    match e {
        E::Minisign(_)
        | E::Base64(_)
        | E::SignatureUtf8(_)
        | E::SignedVersionMismatch { .. }
        | E::MissingSignedVersion => "signature",
        // The feed answered, but with nothing usable (for example the address does not exist
        // yet because no release was published on this channel).
        E::ReleaseNotFound => "no_release",
        E::Reqwest(_) | E::Network(_) => "network",
        _ => "failed",
    }
    .to_owned()
}

fn enabled(rt: &UpdateRuntime) -> Result<&UpdateSettings, String> {
    rt.settings
        .as_ref()
        .ok_or_else(|| "not_configured".to_owned())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // tauri hands the window and state over by value
pub fn update_status(
    window: Window,
    rt: State<'_, UpdateRuntime>,
    lang: Option<String>,
) -> Result<UpdateStatus, String> {
    require_window(&window, MAIN_ONLY)?;
    status(&rt, lang.as_deref().unwrap_or("en"))
}

#[doc(hidden)]
pub fn status(rt: &UpdateRuntime, lang: &str) -> Result<UpdateStatus, String> {
    let enabled = rt.settings.is_some();
    let state = rt.state.lock().map_err(|_| "failed".to_owned())?;
    Ok(UpdateStatus {
        enabled,
        current_version: rt.current_version.clone(),
        build_id: option_env!("PRAXI_BUILD_HASH").unwrap_or("").to_owned(),
        channel: state.channel,
        preview_available: rt
            .settings
            .as_ref()
            .is_some_and(|s| s.preview_endpoint.is_some()),
        last_checked: state.last_checked,
        skipped_version: state.skipped_version.clone(),
        installed_notes: state
            .release_notes
            .as_ref()
            .filter(|n| n.version == rt.current_version)
            .map(|n| UpdateInfo {
                version: n.version.clone(),
                notes: pick_notes(&n.body, lang),
                date: None,
            }),
        previous_version: state.previous_version(&rt.current_version),
        rollback_advised: matches!(
            rt.verdict,
            StartVerdict::RollbackAdvised | StartVerdict::RollbackUrgent
        ),
        rollback_available: rt
            .settings
            .as_ref()
            .is_some_and(|s| s.rollback_endpoint.is_some()),
    })
}

/// Asks the feed for a newer, validly-announced version. Installs nothing.
#[tauri::command]
pub async fn update_check(
    window: Window,
    app: AppHandle,
    rt: State<'_, UpdateRuntime>,
    lang: Option<String>,
) -> Result<Option<UpdateInfo>, String> {
    require_window(&window, MAIN_ONLY)?;
    check_for_update(&app, &rt, lang.as_deref().unwrap_or("en")).await
}

#[doc(hidden)]
pub async fn check_for_update<R: Runtime>(
    app: &AppHandle<R>,
    rt: &UpdateRuntime,
    lang: &str,
) -> Result<Option<UpdateInfo>, String> {
    let _guard = rt.busy.try_lock().map_err(|_| "busy".to_owned())?;
    *rt.found.lock().map_err(|_| "failed".to_owned())? = None;
    let settings = enabled(rt)?;
    let channel = rt.state.lock().map_err(|_| "failed".to_owned())?.channel;
    let endpoint = match channel {
        UpdateChannel::Stable => &settings.endpoint,
        UpdateChannel::Preview => settings
            .preview_endpoint
            .as_ref()
            .ok_or("preview_not_configured")?,
    };
    rt.change_state(|s| {
        s.last_checked = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
    })?;
    let found = check_channel(app, settings, endpoint, false, channel)
        .await
        .map_err(|e| describe(&e))?;
    let skipped = rt
        .state
        .lock()
        .map_err(|_| "failed".to_owned())?
        .skipped_version
        .clone();
    let found = found.filter(|u| skipped.as_deref() != Some(&u.version));
    let info = found.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: pick_notes(u.body.as_deref().unwrap_or(""), lang),
        date: u.date.map(|d| d.to_string()),
    });
    if let Ok(mut slot) = rt.found.lock() {
        *slot = found;
    }
    Ok(info)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri supplies owned window/state handles
pub fn update_set_channel(
    window: Window,
    rt: State<'_, UpdateRuntime>,
    channel: UpdateChannel,
) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    set_channel(&rt, channel)
}

#[doc(hidden)]
pub fn set_channel(rt: &UpdateRuntime, channel: UpdateChannel) -> Result<(), String> {
    let _guard = rt.busy.try_lock().map_err(|_| "busy".to_owned())?;
    if channel == UpdateChannel::Preview && enabled(rt)?.preview_endpoint.is_none() {
        return Err("preview_not_configured".to_owned());
    }
    rt.change_state(|s| {
        if s.channel != channel {
            s.channel = channel;
            s.skipped_version = None;
        }
    })?;
    *rt.found.lock().map_err(|_| "failed".to_owned())? = None;
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri supplies owned window/state handles
pub fn update_skip(window: Window, rt: State<'_, UpdateRuntime>) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    skip_version(&rt)
}

#[doc(hidden)]
pub fn skip_version(rt: &UpdateRuntime) -> Result<(), String> {
    let _guard = rt.busy.try_lock().map_err(|_| "busy".to_owned())?;
    let mut slot = rt.found.lock().map_err(|_| "failed".to_owned())?;
    let version = slot.as_ref().ok_or("nothing_to_install")?.version.clone();
    rt.change_state(|s| s.skipped_version = Some(version))?;
    *slot = None;
    Ok(())
}

/// Downloads the update found by `update_check`, verifies its signature and
/// starts the installer. On Windows the app exits when the installer starts.
#[tauri::command]
pub async fn update_install(window: Window, rt: State<'_, UpdateRuntime>) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    enabled(&rt)?;
    let _guard = rt.busy.try_lock().map_err(|_| "busy".to_owned())?;
    let update = rt
        .found
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .ok_or_else(|| "nothing_to_install".to_owned())?;
    install(&rt, &update).await
}

/// Goes back to the last known-good release from the rollback feed (same
/// signature check, and only to an OLDER version than the running one).
#[tauri::command]
pub async fn update_rollback(
    window: Window,
    app: AppHandle,
    rt: State<'_, UpdateRuntime>,
) -> Result<(), String> {
    require_window(&window, MAIN_ONLY)?;
    rollback(&app, &rt).await
}

#[doc(hidden)]
pub async fn check<R: Runtime>(
    app: &AppHandle<R>,
    settings: &UpdateSettings,
    endpoint: &Url,
    rollback: bool,
) -> Result<Option<Update>, tauri_plugin_updater::Error> {
    check_channel(app, settings, endpoint, rollback, UpdateChannel::Stable).await
}

async fn check_channel<R: Runtime>(
    app: &AppHandle<R>,
    settings: &UpdateSettings,
    endpoint: &Url,
    rollback: bool,
    channel: UpdateChannel,
) -> Result<Option<Update>, tauri_plugin_updater::Error> {
    let mut builder = app
        .updater_builder()
        .pubkey(settings.pubkey.clone())
        .endpoints(vec![endpoint.clone()])?
        .timeout(Duration::from_secs(20));
    if rollback {
        // Only ever downgrade, and never to the version already running.
        builder = builder.version_comparator(|current, remote| remote.version < current);
    } else {
        builder = builder.version_comparator(move |current, remote| {
            remote.version > current
                && (channel == UpdateChannel::Preview || remote.version.pre.is_empty())
        });
    }
    builder.build()?.check().await
}

#[doc(hidden)]
pub async fn install(rt: &UpdateRuntime, update: &Update) -> Result<(), String> {
    // `download` verifies the signature against the built-in key and fails
    // before a single byte is installed.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| describe(&e))?;
    rt.change_state(|state| {
        state.record_verified_update(
            &rt.current_version,
            &update.version,
            update.body.as_deref().unwrap_or(""),
        );
    })?;
    update.install(bytes).map_err(|e| describe(&e))
}

/// Manual rollback, called only by the main-window command.
pub async fn rollback<R: Runtime>(app: &AppHandle<R>, rt: &UpdateRuntime) -> Result<(), String> {
    let settings = enabled(rt)?;
    let endpoint = settings
        .rollback_endpoint
        .as_ref()
        .ok_or_else(|| "no_rollback".to_owned())?;
    let _guard = rt.busy.try_lock().map_err(|_| "busy".to_owned())?;
    let update = check(app, settings, endpoint, true)
        .await
        .map_err(|e| describe(&e))?
        .ok_or_else(|| "no_rollback".to_owned())?;
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| describe(&e))?;
    if let Ok(mut state) = rt.state.lock() {
        state.pending = None; // the old version is not on probation
    }
    rt.persist();
    update.install(bytes).map_err(|e| describe(&e))
}

/// Confirms health after [`HEALTHY_AFTER`]. Even rollback requires a user click.
pub fn spawn_startup_watch<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(rt) = app.try_state::<UpdateRuntime>() else {
            return;
        };
        tokio::time::sleep(HEALTHY_AFTER).await;
        rt.mark_healthy();
    });
}

/// The updater plugin and the shared state for `run()`. `config` is the
/// `plugins.updater` section of the app configuration. Updates stay off (empty
/// key, no feed) unless a signing key and an https feed were built in.
#[must_use]
pub fn init(
    current_version: &str,
    config: Option<&serde_json::Value>,
) -> (
    tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config>,
    UpdateRuntime,
) {
    let settings = UpdateSettings::from_build(config);
    let pubkey = settings
        .as_ref()
        .map(|s| s.pubkey.clone())
        .unwrap_or_default();
    let runtime = UpdateRuntime::start(settings, current_version);
    (
        tauri_plugin_updater::Builder::new().pubkey(pubkey).build(),
        runtime,
    )
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn without_a_public_key_updates_are_off() {
        let url = Some("https://example.com/latest.json");
        assert_eq!(
            UpdateSettings::resolve(None, url, None, None, no_env, false),
            None
        );
        assert_eq!(
            UpdateSettings::resolve(Some("  "), url, None, None, no_env, false),
            None
        );
        assert!(UpdateSettings::resolve(Some("KEY"), url, None, None, no_env, false).is_some());
    }

    #[test]
    fn the_app_configuration_supplies_the_key_and_the_feeds() {
        let config = serde_json::json!({
            "pubkey": " KEY ",
            "endpoints": ["https://example.com/latest.json", "https://example.com/second.json"],
            "praxiPreviewEndpoint": "https://example.com/preview.json",
            "praxiRollbackEndpoint": "",
            "requireSignedVersion": true
        });
        let feeds = ConfiguredFeeds::read(Some(&config));
        assert_eq!(feeds.pubkey.as_deref(), Some("KEY"));
        assert_eq!(
            feeds.endpoint.as_deref(),
            Some("https://example.com/latest.json")
        );
        assert_eq!(
            feeds.preview_endpoint.as_deref(),
            Some("https://example.com/preview.json")
        );
        assert_eq!(
            feeds.rollback_endpoint, None,
            "an empty value counts as not set"
        );
        assert_eq!(ConfiguredFeeds::read(None), ConfiguredFeeds::default());
        // The checked-in configuration has the feeds but no key until the owner adds one:
        // updates stay off rather than guessing.
        let checked_in: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let checked_in = &checked_in["plugins"]["updater"];
        assert_eq!(checked_in["requireSignedVersion"], true);
        let feeds = ConfiguredFeeds::read(Some(checked_in));
        let (stable, preview) = (
            feeds.endpoint.expect("stable feed"),
            feeds.preview_endpoint.expect("preview feed"),
        );
        for feed in [&stable, &preview] {
            assert!(
                parse_endpoint(feed, false).is_some(),
                "{feed} must be a valid https address"
            );
        }
        let with_key = UpdateSettings::resolve(
            Some("KEY"),
            Some(&stable),
            None,
            Some(&preview),
            no_env,
            false,
        )
        .expect("a key plus the checked-in feeds turns updates on");
        assert_eq!(with_key.endpoint.as_str(), stable);
        assert!(with_key.preview_endpoint.is_some());
    }

    #[test]
    fn a_missing_release_is_told_apart_from_a_network_failure() {
        use tauri_plugin_updater::Error as E;
        assert_eq!(describe(&E::ReleaseNotFound), "no_release");
        assert_eq!(describe(&E::Network("down".into())), "network");
        assert_eq!(describe(&E::EmptyEndpoints), "failed");
        // An installer signed for another version is a failed verification, not a glitch.
        assert_eq!(describe(&E::MissingSignedVersion), "signature");
    }

    #[test]
    fn plain_http_and_foreign_env_are_refused_in_release() {
        let http = Some("http://127.0.0.1:9/latest.json");
        assert_eq!(
            UpdateSettings::resolve(Some("K"), http, None, None, no_env, false),
            None
        );
        assert!(UpdateSettings::resolve(Some("K"), http, None, None, no_env, true).is_some());
        let remote_http = Some("http://example.com/latest.json");
        assert_eq!(
            UpdateSettings::resolve(Some("K"), remote_http, None, None, no_env, true),
            None
        );
        // A bad rollback feed disables only the rollback, not updates.
        let s = UpdateSettings::resolve(
            Some("K"),
            Some("https://a.example/l.json"),
            Some("ftp://x"),
            Some("http://updates.example/preview.json"),
            no_env,
            false,
        )
        .expect("settings");
        assert!(s.rollback_endpoint.is_none());
        assert!(s.preview_endpoint.is_none());
    }

    const BODY: &str = "Default text\n<!-- lang: en -->\nFixed the thing.\n<!-- lang: tr -->\nSorun düzeltildi.\n<!-- lang: de -->\nProblem behoben.\n";

    #[test]
    fn notes_follow_the_users_language_with_fallbacks() {
        assert_eq!(pick_notes(BODY, "tr"), "Sorun düzeltildi.");
        assert_eq!(pick_notes(BODY, "de-DE"), "Problem behoben.");
        assert_eq!(pick_notes(BODY, "fr"), "Fixed the thing.");
        assert_eq!(pick_notes("Only plain notes", "tr"), "Only plain notes");
        assert_eq!(
            pick_notes("<!-- lang: de -->\nNur Deutsch", "fr"),
            "Nur Deutsch"
        );
        assert_eq!(pick_notes("", "en"), "");
        assert_eq!(
            pick_notes(&"x".repeat(MAX_NOTES_CHARS + 50), "en")
                .chars()
                .count(),
            MAX_NOTES_CHARS
        );
    }

    #[test]
    fn a_version_that_keeps_failing_to_start_gets_rollback_advice() {
        let mut s = UpdateState::default();
        assert_eq!(s.begin_start("0.1.0"), StartVerdict::Normal);
        s.record_install("0.1.0", "0.2.0");
        // The installer ran: first start of 0.2.0 crashes before it is healthy.
        assert_eq!(s.begin_start("0.2.0"), StartVerdict::Probation);
        assert_eq!(s.begin_start("0.2.0"), StartVerdict::RollbackAdvised);
        assert_eq!(s.previous_version("0.2.0").as_deref(), Some("0.1.0"));
        assert_eq!(s.begin_start("0.2.0"), StartVerdict::RollbackUrgent);
        // After the rollback the old version starts: the bookkeeping is cleared.
        assert_eq!(s.begin_start("0.1.0"), StartVerdict::Normal);
        assert_eq!(s.pending, None);
    }

    #[test]
    fn installed_notes_survive_health_confirmation_and_old_state_defaults_to_stable() {
        let legacy: UpdateState = serde_json::from_str(r#"{"pending":null}"#).unwrap();
        assert_eq!(legacy.channel, UpdateChannel::Stable);
        let rt = UpdateRuntime::detached(None, "0.2.0");
        rt.change_state(|s| s.record_verified_update("0.1.0", "0.2.0", BODY))
            .unwrap();
        rt.mark_healthy();
        for (lang, expected) in [
            ("en", "Fixed the thing."),
            ("tr", "Sorun düzeltildi."),
            ("de", "Problem behoben."),
        ] {
            assert_eq!(
                status(&rt, lang).unwrap().installed_notes.unwrap().notes,
                expected
            );
        }
        let state = rt.state.lock().unwrap().clone();
        let restored: UpdateState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(restored, state);
        assert!(state.pending.is_none());
    }

    #[test]
    fn a_healthy_run_ends_the_probation() {
        let mut s = UpdateState::default();
        s.record_install("0.1.0", "0.2.0");
        assert_eq!(s.begin_start("0.2.0"), StartVerdict::Probation);
        s.confirm_healthy("0.2.0");
        assert_eq!(s.begin_start("0.2.0"), StartVerdict::Normal);
    }

    #[test]
    fn state_survives_a_restart_and_a_broken_file_is_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "praxi-update-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let mut s = UpdateState::default();
        s.record_verified_update("0.1.0", "0.2.0", BODY);
        s.channel = UpdateChannel::Preview;
        s.skipped_version = Some("0.3.0-beta.1".into());
        s.last_checked = Some(123);
        s.save(&dir).expect("save");
        assert_eq!(UpdateState::load(&dir), s);
        std::fs::write(dir.join(STATE_FILE), b"{not json").expect("write");
        assert_eq!(UpdateState::load(&dir), UpdateState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
