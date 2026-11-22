//! `GET/POST/DELETE /v1/local-install`: one-click local model.
//!
//! The owner picks one of a short curated list; the daemon downloads a pinned `llama-server`
//! build and the model file (sha256-checked, resumable, with a free-disk check), unpacks it into
//! `<data dir>/local-models/`, and starts the server on `127.0.0.1:8080`, where the existing local
//! server detection finds it. Local owner only; the files come from fixed https URLs in
//! `provider_catalog::local_runtime::manifest`, never from the request.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{Json, extract::State};
use provider_catalog::{
    hardware::HardwareInfo,
    local_advice::{self, Fit},
    local_runtime::{
        self, DownloadError, DownloadSpec, LLAMA_PORT, ManagedModel, ServeError, ServerHandle,
        download, install_verified, managed_model, managed_models, runtime_asset, verified_binary,
    },
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, live_view::Actor};

const GIB: f64 = 1_073_741_824.0;
/// Free space to keep after everything is unpacked.
const DISK_MARGIN_BYTES: u64 = 1 << 30;

/// How a model is expected to run here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedFitDto {
    Comfortable,
    Slow,
    TooLarge,
    /// The computer's memory could not be read.
    Unknown,
}

/// One model the owner can install.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ManagedModelDto {
    pub id: String,
    pub display_name: String,
    /// Download size of the model file.
    pub size_bytes: u64,
    /// SPDX licence id of the model weights.
    pub licence: String,
    pub min_ram_gib: u32,
    pub fit: ManagedFitDto,
    /// False when free disk space is smaller than runtime plus model plus a safety margin.
    pub disk_ok: Option<bool>,
    /// The one suggested for this computer.
    pub recommended: bool,
}

/// Where the install is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhaseDto {
    #[default]
    Idle,
    DownloadingRuntime,
    Unpacking,
    DownloadingModel,
    Starting,
    Ready,
    Failed,
    Cancelled,
}

/// Why it failed, as a stable kind the UI turns into a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallErrorKindDto {
    Unsupported,
    Disk,
    Network,
    Checksum,
    PortBusy,
    StartFailed,
    Other,
}

/// Progress of the current or last install.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct InstallStateDto {
    pub phase: InstallPhaseDto,
    pub model_id: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub error: Option<InstallErrorKindDto>,
}

/// Response of `GET /v1/local-install`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LocalInstallDto {
    /// False when no pinned runtime exists for this operating system / processor.
    pub supported: bool,
    /// Download size of the runtime program.
    pub runtime_size_bytes: u64,
    pub models: Vec<ManagedModelDto>,
    pub state: InstallStateDto,
}

/// Body of `POST /v1/local-install`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StartInstallRequest {
    /// One of the ids from `GET /v1/local-install`.
    pub model_id: String,
}

/// Ranks the curated list for this machine (pure; tests inject the hardware).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn plan(hw: &HardwareInfo) -> LocalInstallDto {
    let asset = runtime_asset(&hw.os, &hw.arch);
    let advice = local_advice::recommend(hw, &local_runtime::candidates());
    let ram_gib = hw.ram_bytes.map(|b| b as f64 / GIB);
    let runtime_size = asset.map_or(0, |a| a.size_bytes);
    let models = managed_models()
        .iter()
        .map(|m| {
            let verdict = advice.models.iter().find(|r| r.model_id == m.id);
            let fit = match (verdict, ram_gib) {
                (_, Some(ram)) if ram < f64::from(m.min_ram_gib) => ManagedFitDto::TooLarge,
                (Some(r), _) => match r.fit {
                    Fit::Comfortable => ManagedFitDto::Comfortable,
                    Fit::Slow => ManagedFitDto::Slow,
                    Fit::TooLarge => ManagedFitDto::TooLarge,
                },
                (None, _) => ManagedFitDto::Unknown,
            };
            ManagedModelDto {
                id: m.id.to_owned(),
                display_name: m.display_name.to_owned(),
                size_bytes: m.size_bytes,
                licence: m.licence.to_owned(),
                min_ram_gib: m.min_ram_gib,
                fit,
                disk_ok: hw
                    .free_disk_bytes
                    .map(|free| free >= runtime_size + m.size_bytes + DISK_MARGIN_BYTES),
                recommended: false,
            }
        })
        .collect::<Vec<_>>();
    let mut models = models;
    // Largest model that runs comfortably, else the largest that runs slowly.
    let pick = |want: ManagedFitDto| {
        models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.fit == want && m.disk_ok != Some(false))
            .max_by_key(|(_, m)| m.size_bytes)
            .map(|(i, _)| i)
    };
    if let Some(i) = pick(ManagedFitDto::Comfortable).or_else(|| pick(ManagedFitDto::Slow)) {
        models[i].recommended = true;
    }
    LocalInstallDto {
        supported: asset.is_some(),
        runtime_size_bytes: runtime_size,
        models,
        state: InstallStateDto::default(),
    }
}

#[derive(Default)]
struct Job {
    state: InstallStateDto,
    cancel: Arc<AtomicBool>,
}

static JOB: LazyLock<Mutex<Job>> = LazyLock::new(|| Mutex::new(Job::default()));
static SERVER: LazyLock<tokio::sync::Mutex<Option<ServerHandle>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

fn job() -> std::sync::MutexGuard<'static, Job> {
    JOB.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn set_phase(phase: InstallPhaseDto) {
    job().state.phase = phase;
}

fn fail(kind: InstallErrorKindDto) {
    let mut j = job();
    j.state.phase = InstallPhaseDto::Failed;
    j.state.error = Some(kind);
}

fn root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("local-models")
}

fn download_error_kind(e: &DownloadError) -> Option<InstallErrorKindDto> {
    match e {
        DownloadError::Cancelled => None,
        DownloadError::NotEnoughDisk { .. } => Some(InstallErrorKindDto::Disk),
        DownloadError::ChecksumMismatch | DownloadError::TooLarge => {
            Some(InstallErrorKindDto::Checksum)
        }
        DownloadError::Incomplete | DownloadError::Network(_) | DownloadError::InsecureUrl => {
            Some(InstallErrorKindDto::Network)
        }
        DownloadError::Io(_) => Some(InstallErrorKindDto::Other),
    }
}

/// Records a download failure; returns `false` when it was a cancel.
fn record_download_error(e: &DownloadError) {
    match download_error_kind(e) {
        Some(kind) => fail(kind),
        None => set_phase(InstallPhaseDto::Cancelled),
    }
}

async fn run_install(
    data_dir: PathBuf,
    model: &'static ManagedModel,
    hw: HardwareInfo,
    cancel: Arc<AtomicBool>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let Some(asset) = runtime_asset(&hw.os, &hw.arch) else {
        return fail(InstallErrorKindDto::Unsupported);
    };
    let root = root_dir(&data_dir);
    let runtime_dir = root
        .join("runtime")
        .join(local_runtime::manifest::LLAMA_CPP_TAG);
    let total = asset.size_bytes + model.size_bytes;
    job().state.bytes_total = total;

    // 1. the runtime program
    // Only a binary that passes its install manifest counts; a bare file is never trusted.
    let mut binary = verified_binary(&runtime_dir, asset.binary);
    if binary.is_none() {
        set_phase(InstallPhaseDto::DownloadingRuntime);
        let archive = root.join("downloads").join(asset.file_name);
        let spec = DownloadSpec {
            url: asset.url,
            dest: &archive,
            sha256: asset.sha256,
            size_bytes: asset.size_bytes,
        };
        let progress = |n: u64| job().state.bytes_done = n;
        if let Err(e) = download(&spec, &progress, &cancel).await {
            return record_download_error(&e);
        }
        set_phase(InstallPhaseDto::Unpacking);
        let (kind, a, d, sha, bin) = (
            asset.kind,
            archive.clone(),
            runtime_dir.clone(),
            asset.sha256,
            asset.binary,
        );
        let unpacked =
            tokio::task::spawn_blocking(move || install_verified(kind, &a, sha, &d, bin)).await;
        let _ = tokio::fs::remove_file(&archive).await;
        binary = match unpacked {
            Ok(Ok(path)) => Some(path),
            Ok(Err(local_runtime::TrustError::ArchiveMismatch)) => {
                return fail(InstallErrorKindDto::Checksum);
            }
            _ => return fail(InstallErrorKindDto::Other),
        };
    }
    let Some(binary) = binary else {
        return fail(InstallErrorKindDto::Other);
    };

    // 2. the model file
    set_phase(InstallPhaseDto::DownloadingModel);
    let model_path = root.join("models").join(model.file_name);
    let spec = DownloadSpec {
        url: model.url,
        dest: &model_path,
        sha256: model.sha256,
        size_bytes: model.size_bytes,
    };
    let base = asset.size_bytes;
    let progress = move |n: u64| job().state.bytes_done = base + n;
    if let Err(e) = download(&spec, &progress, &cancel).await {
        return record_download_error(&e);
    }
    job().state.bytes_done = total;

    // 3. start it (replacing an earlier managed server)
    set_phase(InstallPhaseDto::Starting);
    let mut slot = SERVER.lock().await;
    if let Some(old) = slot.as_mut() {
        old.stop().await;
    }
    *slot = None;
    match local_runtime::start_server(&binary, &model_path, LLAMA_PORT, hw.logical_cores).await {
        Ok(handle) => {
            *slot = Some(handle);
            set_phase(InstallPhaseDto::Ready);
            // The server must not outlive the daemon.
            tokio::spawn(async move {
                shutdown.cancelled().await;
                if let Some(h) = SERVER.lock().await.as_mut() {
                    h.stop().await;
                }
            });
        }
        Err(ServeError::PortBusy(_)) => fail(InstallErrorKindDto::PortBusy),
        Err(ServeError::Untrusted) => fail(InstallErrorKindDto::Checksum),
        Err(_) => fail(InstallErrorKindDto::StartFailed),
    }
}

async fn current_state() -> InstallStateDto {
    let mut state = job().state.clone();
    // A server that died after "ready" is not ready any more.
    if state.phase == InstallPhaseDto::Ready {
        let alive = SERVER
            .lock()
            .await
            .as_mut()
            .is_some_and(ServerHandle::is_running);
        if !alive {
            state.phase = InstallPhaseDto::Idle;
        }
    }
    state
}

fn audit(s: &AppState, action: &str, detail: &serde_json::Value) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, "user", action, detail)
        .map_err(ApiError::internal)?;
    Ok(())
}

async fn probe(s: &AppState) -> Result<HardwareInfo, ApiError> {
    let dir = s.data_dir();
    tokio::task::spawn_blocking(move || provider_catalog::probe_hardware(&dir))
        .await
        .map_err(|e| ApiError::internal(format!("hardware probe: {e}")))
}

/// The curated models ranked for this computer, and the install progress.
#[utoipa::path(get, path = "/v1/local-install", tag = "providers", security(("bearer" = [])),
    responses((status = 200, body = LocalInstallDto),
              (status = 403, body = crate::error::ErrorBody, description = "caller is not the local owner")))]
pub(crate) async fn get_local_install(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> Result<Json<LocalInstallDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let mut dto = plan(&probe(&s).await?);
    dto.state = current_state().await;
    Ok(Json(dto))
}

/// Starts downloading and starting the chosen model. Returns at once; poll `GET`.
#[utoipa::path(post, path = "/v1/local-install", tag = "providers", security(("bearer" = [])),
    request_body = StartInstallRequest,
    responses((status = 202, body = InstallStateDto),
              (status = 400, body = crate::error::ErrorBody, description = "unknown model or unsupported computer"),
              (status = 403, body = crate::error::ErrorBody, description = "caller is not the local owner"),
              (status = 409, body = crate::error::ErrorBody, description = "an install is already running")))]
pub(crate) async fn start_local_install(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
    Json(req): Json<StartInstallRequest>,
) -> Result<(axum::http::StatusCode, Json<InstallStateDto>), ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let model = managed_model(&req.model_id)
        .ok_or_else(|| ApiError::bad_request("this model is not in the list"))?;
    let hw = probe(&s).await?;
    if runtime_asset(&hw.os, &hw.arch).is_none() {
        return Err(ApiError::bad_request(
            "one-click local models are not available for this computer yet",
        ));
    }
    let cancel = {
        let mut j = job();
        if matches!(
            j.state.phase,
            InstallPhaseDto::DownloadingRuntime
                | InstallPhaseDto::Unpacking
                | InstallPhaseDto::DownloadingModel
                | InstallPhaseDto::Starting
        ) {
            return Err(ApiError::conflict(
                "a local model install is already running",
            ));
        }
        j.cancel = Arc::new(AtomicBool::new(false));
        j.state = InstallStateDto {
            phase: InstallPhaseDto::DownloadingRuntime,
            model_id: Some(model.id.to_owned()),
            ..InstallStateDto::default()
        };
        j.cancel.clone()
    };
    audit(
        &s,
        "local_install.start",
        &serde_json::json!({ "model": model.id, "bytes": model.size_bytes }),
    )?;
    tokio::spawn(run_install(
        s.data_dir(),
        model,
        hw,
        cancel,
        s.shutdown_token(),
    ));
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(current_state().await),
    ))
}

/// Stops a running download, or stops the local model program.
#[utoipa::path(delete, path = "/v1/local-install", tag = "providers", security(("bearer" = [])),
    responses((status = 200, body = InstallStateDto),
              (status = 403, body = crate::error::ErrorBody, description = "caller is not the local owner")))]
pub(crate) async fn stop_local_install(
    State(_s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> Result<Json<InstallStateDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let phase = job().state.phase;
    job().cancel.store(true, Ordering::Relaxed);
    if matches!(phase, InstallPhaseDto::Ready | InstallPhaseDto::Starting) {
        if let Some(h) = SERVER.lock().await.as_mut() {
            h.stop().await;
        }
        if phase == InstallPhaseDto::Ready {
            set_phase(InstallPhaseDto::Idle);
        }
    }
    Ok(Json(current_state().await))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(
        ram_gib: u64,
        vram_gib: Option<u64>,
        free_gib: u64,
        os: &str,
        arch: &str,
    ) -> HardwareInfo {
        HardwareInfo {
            ram_bytes: Some(ram_gib << 30),
            logical_cores: Some(8),
            gpu: vram_gib.map(|v| provider_catalog::GpuInfo {
                name: "Test GPU".into(),
                vram_bytes: Some(v << 30),
            }),
            unified_memory: false,
            free_disk_bytes: Some(free_gib << 30),
            os: os.into(),
            arch: arch.into(),
        }
    }

    fn rec(dto: &LocalInstallDto) -> Option<&str> {
        dto.models
            .iter()
            .find(|m| m.recommended)
            .map(|m| m.id.as_str())
    }

    #[test]
    fn a_small_laptop_gets_the_small_model_and_a_big_gpu_box_the_big_one() {
        let small = plan(&hw(6, None, 100, "windows", "x86_64"));
        assert!(small.supported);
        assert_eq!(rec(&small), Some("qwen3:1.7b-q4"));
        let mid = plan(&hw(8, None, 100, "windows", "x86_64"));
        assert_eq!(rec(&mid), Some("qwen3:4b-q4"));
        let big = plan(&hw(32, Some(24), 200, "linux", "x86_64"));
        assert_eq!(rec(&big), Some("qwen3:8b-q4"));
    }

    #[test]
    fn too_little_ram_or_disk_is_never_recommended() {
        let tiny = plan(&hw(2, None, 100, "linux", "x86_64"));
        assert!(tiny.models.iter().all(|m| m.fit == ManagedFitDto::TooLarge));
        assert_eq!(rec(&tiny), None);
        let full = plan(&hw(32, Some(24), 2, "linux", "x86_64"));
        assert!(full.models.iter().all(|m| m.disk_ok == Some(false)));
        assert_eq!(rec(&full), None);
    }

    #[test]
    fn an_unsupported_computer_is_reported_not_guessed() {
        assert!(!plan(&hw(32, None, 100, "linux", "aarch64")).supported);
    }
}
