//! `GET /v1/local-models`: what this machine can run locally, as best-effort advice.
//!
//! Read-only and local-only. The hardware summary and the advice are computed in-process, never
//! sent to a model or to telemetry, and nothing is installed, pulled or started. The runtime
//! status only touches loopback ports and the file system (a PATH lookup).

use axum::{Json, extract::State};
use provider_catalog::{
    hardware::HardwareInfo,
    local_advice::{self, Fit, LocalAdvice, MemoryBasis, on_path},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ApiError, AppState, live_view::Actor};

/// Detected hardware. Every field is null/false when it could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HardwareDto {
    pub ram_bytes: Option<u64>,
    pub logical_cores: Option<u32>,
    pub gpu_name: Option<String>,
    pub gpu_vram_bytes: Option<u64>,
    /// Apple silicon: the GPU shares system memory.
    pub unified_memory: bool,
    pub free_disk_bytes: Option<u64>,
    pub os: String,
    pub arch: String,
}

/// How a model is expected to run here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FitDto {
    Comfortable,
    Slow,
    TooLarge,
}

/// Which memory the verdicts are measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBasisDto {
    Vram,
    Unified,
    CpuRam,
    Unknown,
}

/// One curated local model with its verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LocalModelDto {
    /// Exact catalog id (for example `qwen3:8b`).
    pub model_id: String,
    pub display_name: String,
    /// Size class in billions of parameters.
    pub params_billions: f64,
    pub fit: FitDto,
    /// Estimated memory need in GiB.
    pub needs_gib: f64,
    /// The command to copy; Praxi never runs it.
    pub pull_command: String,
    /// False when free disk space is smaller than the estimated download.
    pub disk_ok: Option<bool>,
}

/// Advice for this machine. `recommended` null means "no recommendation".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LocalAdviceDto {
    pub basis: MemoryBasisDto,
    pub budget_gib: Option<f64>,
    /// Largest comfortable size class in billions of parameters, for runtimes without a curated
    /// list (LM Studio).
    pub max_comfortable_billions: Option<f64>,
    /// Best first.
    pub models: Vec<LocalModelDto>,
    pub recommended: Option<String>,
}

/// State of one local runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalRuntimeDto {
    /// `ollama` or `lmstudio`.
    pub id: String,
    /// The executable was found on PATH (Ollama only; LM Studio is a desktop app, so null).
    pub installed: Option<bool>,
    /// The server answered on its loopback port.
    pub running: bool,
    /// Models the running server reports.
    pub installed_models: Vec<String>,
}

/// Response of `GET /v1/local-models`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LocalModelsDto {
    pub hardware: HardwareDto,
    pub advice: LocalAdviceDto,
    pub runtimes: Vec<LocalRuntimeDto>,
}

impl From<&HardwareInfo> for HardwareDto {
    fn from(h: &HardwareInfo) -> Self {
        Self {
            ram_bytes: h.ram_bytes,
            logical_cores: h.logical_cores,
            gpu_name: h.gpu.as_ref().map(|g| g.name.clone()),
            gpu_vram_bytes: h.gpu.as_ref().and_then(|g| g.vram_bytes),
            unified_memory: h.unified_memory,
            free_disk_bytes: h.free_disk_bytes,
            os: h.os.clone(),
            arch: h.arch.clone(),
        }
    }
}

impl From<&LocalAdvice> for LocalAdviceDto {
    fn from(a: &LocalAdvice) -> Self {
        Self {
            basis: match a.basis {
                MemoryBasis::Vram => MemoryBasisDto::Vram,
                MemoryBasis::Unified => MemoryBasisDto::Unified,
                MemoryBasis::CpuRam => MemoryBasisDto::CpuRam,
                MemoryBasis::Unknown => MemoryBasisDto::Unknown,
            },
            budget_gib: a.budget_gib,
            max_comfortable_billions: a.max_comfortable_billions,
            models: a
                .models
                .iter()
                .map(|m| LocalModelDto {
                    model_id: m.model_id.clone(),
                    display_name: m.display_name.clone(),
                    params_billions: m.params_billions,
                    fit: match m.fit {
                        Fit::Comfortable => FitDto::Comfortable,
                        Fit::Slow => FitDto::Slow,
                        Fit::TooLarge => FitDto::TooLarge,
                    },
                    needs_gib: m.needs_gib,
                    pull_command: m.pull_command.clone(),
                    disk_ok: m.disk_ok,
                })
                .collect(),
            recommended: a.recommended.clone(),
        }
    }
}

/// Builds the report from already-gathered inputs (pure; tests inject them).
#[must_use]
pub fn build_report(
    hw: &HardwareInfo,
    ollama_on_path: bool,
    servers: &[provider_catalog::LocalServer],
) -> LocalModelsDto {
    let advice = local_advice::recommend(hw, &local_advice::catalog_candidates());
    let runtime = |id: &str, installed: Option<bool>| {
        let server = servers.iter().find(|s| s.id == id);
        LocalRuntimeDto {
            id: id.to_owned(),
            installed,
            running: server.is_some(),
            installed_models: server.map(|s| s.models.clone()).unwrap_or_default(),
        }
    };
    LocalModelsDto {
        hardware: HardwareDto::from(hw),
        advice: LocalAdviceDto::from(&advice),
        runtimes: vec![
            runtime("ollama", Some(ollama_on_path)),
            runtime("lmstudio", None),
        ],
    }
}

/// What this machine can run locally. Local owner only: a hardware summary is a fingerprint.
#[utoipa::path(get, path = "/v1/local-models", tag = "providers", security(("bearer" = [])),
    responses((status = 200, body = LocalModelsDto),
              (status = 403, body = crate::error::ErrorBody, description = "caller is not the local owner")))]
pub(crate) async fn get_local_models(
    State(s): State<AppState>,
    axum::Extension(actor): axum::Extension<Actor>,
) -> Result<Json<LocalModelsDto>, ApiError> {
    crate::cli_engines::local_owner(&actor)?;
    let dir = s.data_dir();
    let hw = tokio::task::spawn_blocking(move || provider_catalog::probe_hardware(&dir))
        .await
        .map_err(|e| ApiError::internal(format!("hardware probe: {e}")))?;
    let found = std::env::var_os("PATH").is_some_and(|p| on_path("ollama", &p));
    let servers = provider_catalog::detect_local_servers().await;
    Ok(Json(build_report(&hw, found, &servers)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_has_runtime_state_and_a_catalog_pull_command() {
        let hw = HardwareInfo {
            ram_bytes: Some(32 << 30),
            logical_cores: Some(16),
            gpu: Some(provider_catalog::GpuInfo {
                name: "Test".into(),
                vram_bytes: Some(24 << 30),
            }),
            unified_memory: false,
            free_disk_bytes: Some(500 << 30),
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let servers = [provider_catalog::LocalServer {
            id: "ollama".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            models: vec!["qwen3:8b".into()],
        }];
        let r = build_report(&hw, true, &servers);
        assert!(r.runtimes[0].running && r.runtimes[0].installed == Some(true));
        assert!(!r.runtimes[1].running);
        let best = r.advice.recommended.expect("recommendation");
        assert_eq!(
            r.advice.models[0].pull_command,
            format!("ollama pull {best}")
        );
    }
}
