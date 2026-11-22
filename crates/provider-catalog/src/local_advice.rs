//! Maps detected hardware to the local models that will realistically run.
//!
//! Pure and data-driven: candidates come from the curated catalog table
//! (`model_metadata.json`, provider `ollama`); no model id is written here. A model's size class is
//! the parameter count in its own id tag (`qwen3:8b` is 8 billion). A model whose size cannot be
//! read is never recommended, and an empty catalog yields "no recommendation".
//!
//! The numbers are estimates for 4-bit quantised weights plus context overhead; they are
//! deliberately conservative and always presented as best effort.

use std::{ffi::OsStr, path::Path};

use serde::Serialize;

use crate::{
    hardware::HardwareInfo,
    model_metadata::{CuratedModels, embedded_metadata, safe_model_id},
};

const GIB: f64 = 1_073_741_824.0;
/// Approximate weight size per billion parameters at 4-bit quantisation, in GiB.
const GIB_PER_BILLION: f64 = 0.65;
/// Fixed room for the context cache and runtime, in GiB.
const OVERHEAD_GIB: f64 = 1.5;
/// Share of system RAM a CPU-only run may use before the machine starts swapping.
const CPU_RAM_SHARE: f64 = 0.6;
/// Share of Apple unified memory the GPU may wire for a model.
const APPLE_RAM_SHARE: f64 = 0.7;
/// CPU-only speed is acceptable up to about this many billion parameters.
const CPU_COMFORTABLE_BILLIONS: f64 = 4.0;
/// Catalog provider the local pull commands come from.
const OLLAMA_PROVIDER: &str = "ollama";

/// How a model is expected to behave here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Fits in the fast memory (video memory, unified memory, or a small model in RAM).
    Comfortable,
    /// Fits in system RAM only: it will run, but slowly.
    Slow,
    /// Does not fit.
    TooLarge,
}

/// Which memory the verdicts are measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBasis {
    /// A discrete GPU's video memory (system RAM is the slow fallback).
    Vram,
    /// Apple silicon unified memory.
    Unified,
    /// No usable GPU found: CPU and system RAM.
    CpuRam,
    /// Memory could not be read; nothing can be recommended.
    Unknown,
}

/// One candidate with its verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recommendation {
    /// Exact id from the catalog (`qwen3:8b`).
    pub model_id: String,
    pub display_name: String,
    /// Size class in billions of parameters.
    pub params_billions: f64,
    pub fit: Fit,
    /// Estimated memory the model needs, in GiB (rounded to one decimal).
    pub needs_gib: f64,
    /// `ollama pull <id>`; shown for the user to copy, never run by Praxi.
    pub pull_command: String,
    /// `Some(false)` when the free disk space is smaller than the estimated download.
    pub disk_ok: Option<bool>,
}

/// The advice for one machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalAdvice {
    pub basis: MemoryBasis,
    /// Fast-memory budget in GiB the verdicts used, when known.
    pub budget_gib: Option<f64>,
    /// Largest size class (billions) that is `Comfortable`; the guidance for runtimes without a
    /// curated list (LM Studio). `None` when nothing fits comfortably.
    pub max_comfortable_billions: Option<f64>,
    /// Best first: comfortable (largest first), then slow, then too large.
    pub models: Vec<Recommendation>,
    /// Catalog id of the model to suggest; `None` means "no recommendation".
    pub recommended: Option<String>,
}

/// A catalog candidate: id and display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub display_name: String,
}

/// Curated local candidates from the embedded catalog table.
#[must_use]
pub fn catalog_candidates() -> Vec<Candidate> {
    candidates_from(embedded_metadata())
}

/// Curated local candidates from a given table (injectable for tests).
#[must_use]
pub fn candidates_from(table: &CuratedModels) -> Vec<Candidate> {
    let Some(provider) = table.find_provider(OLLAMA_PROVIDER) else {
        return Vec::new();
    };
    provider
        .models
        .iter()
        .filter(|m| m.recommended && !m.deprecated && m.kind.is_chat())
        .filter(|m| safe_command_id(&m.id))
        .map(|m| Candidate {
            id: m.id.clone(),
            display_name: m.display_name.clone(),
        })
        .collect()
}

/// Parameter count in billions from an id tag such as `qwen3:8b` or `llama3.3:70b-instruct`.
#[must_use]
pub fn params_billions(id: &str) -> Option<f64> {
    let tag = id.rsplit_once(':')?.1.to_ascii_lowercase();
    let digits_end = tag
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(tag.len());
    let (number, rest) = tag.split_at(digits_end);
    if !(rest == "b" || rest.starts_with("b-") || rest.starts_with("b_")) {
        return None;
    }
    number
        .parse::<f64>()
        .ok()
        .filter(|b| *b > 0.0 && *b < 2000.0)
}

/// An id is only ever put into a copyable shell command when it is plain and cannot be read as a flag.
fn safe_command_id(id: &str) -> bool {
    safe_model_id(id) && !id.starts_with('-')
}

fn needs_gib(billions: f64) -> f64 {
    billions * GIB_PER_BILLION + OVERHEAD_GIB
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Hardware plus candidates in, ranked verdicts out.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn recommend(hw: &HardwareInfo, candidates: &[Candidate]) -> LocalAdvice {
    let ram_gib = hw.ram_bytes.map(|b| b as f64 / GIB);
    let vram_gib = hw
        .gpu
        .as_ref()
        .and_then(|g| g.vram_bytes)
        .map(|b| b as f64 / GIB);

    let (basis, fast_budget, slow_budget) = if hw.unified_memory && ram_gib.is_some() {
        let b = ram_gib.map(|r| r * APPLE_RAM_SHARE);
        (MemoryBasis::Unified, b, None)
    } else if let (Some(vram), Some(_)) = (vram_gib, ram_gib) {
        (
            MemoryBasis::Vram,
            Some(vram),
            ram_gib.map(|r| r * CPU_RAM_SHARE),
        )
    } else if ram_gib.is_some() {
        (
            MemoryBasis::CpuRam,
            None,
            ram_gib.map(|r| r * CPU_RAM_SHARE),
        )
    } else {
        (MemoryBasis::Unknown, None, None)
    };

    let mut models: Vec<Recommendation> = candidates
        .iter()
        .filter_map(|c| {
            if !safe_command_id(&c.id) {
                return None;
            }
            let billions = params_billions(&c.id)?;
            let need = needs_gib(billions);
            let fit = match basis {
                MemoryBasis::Unknown => return None,
                MemoryBasis::Unified | MemoryBasis::Vram
                    if fast_budget.is_some_and(|b| need <= b) =>
                {
                    Fit::Comfortable
                }
                MemoryBasis::CpuRam
                    if slow_budget.is_some_and(|b| need <= b)
                        && billions <= CPU_COMFORTABLE_BILLIONS =>
                {
                    Fit::Comfortable
                }
                _ if slow_budget.is_some_and(|b| need <= b) => Fit::Slow,
                _ => Fit::TooLarge,
            };
            let download_gib = billions * GIB_PER_BILLION;
            Some(Recommendation {
                model_id: c.id.clone(),
                display_name: c.display_name.clone(),
                params_billions: billions,
                fit,
                needs_gib: round1(need),
                pull_command: format!("ollama pull {}", c.id),
                disk_ok: hw.free_disk_bytes.map(|d| d as f64 / GIB >= download_gib),
            })
        })
        .collect();

    let rank = |r: &Recommendation| match r.fit {
        Fit::Comfortable => 0,
        Fit::Slow => 1,
        Fit::TooLarge => 2,
    };
    models.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then_with(|| {
            let by_size = a.params_billions.total_cmp(&b.params_billions);
            // Larger first among comfortable; smaller first among the rest.
            if a.fit == Fit::Comfortable {
                by_size.reverse()
            } else {
                by_size
            }
        })
    });

    let max_comfortable_billions = models
        .iter()
        .filter(|m| m.fit == Fit::Comfortable)
        .map(|m| m.params_billions)
        .fold(None, |acc: Option<f64>, b| {
            Some(acc.map_or(b, |a| a.max(b)))
        })
        .or_else(|| comfortable_ceiling(basis, fast_budget, slow_budget));
    // `models` is ranked best first (comfortable largest first), so the first entry that fits and
    // has room on disk is the largest model that works here.
    let recommended = models
        .iter()
        .find(|m| m.fit != Fit::TooLarge && m.disk_ok != Some(false))
        .map(|m| m.model_id.clone());

    LocalAdvice {
        basis,
        budget_gib: fast_budget.or(slow_budget).map(round1),
        max_comfortable_billions,
        models,
        recommended,
    }
}

/// Size ceiling from memory alone, for when the catalog has no candidate to measure.
fn comfortable_ceiling(basis: MemoryBasis, fast: Option<f64>, slow: Option<f64>) -> Option<f64> {
    let max_for = |budget: f64| ((budget - OVERHEAD_GIB) / GIB_PER_BILLION).floor();
    let ceiling = match basis {
        MemoryBasis::Unified | MemoryBasis::Vram => fast.map(max_for),
        MemoryBasis::CpuRam => slow.map(|b| max_for(b).min(CPU_COMFORTABLE_BILLIONS)),
        MemoryBasis::Unknown => None,
    };
    ceiling.filter(|c| *c >= 1.0)
}

/// Whether an executable called `name` exists in one of the directories of `path_var`. Only
/// looks at the file system; nothing is executed.
#[must_use]
pub fn on_path(name: &str, path_var: &OsStr) -> bool {
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(path_var).any(|dir| {
        exts.iter()
            .any(|ext| Path::new(&dir).join(format!("{name}{ext}")).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::GpuInfo;

    const GIB_U: u64 = 1 << 30;

    fn hw(ram_gib: u64, vram_gib: Option<u64>, apple: bool) -> HardwareInfo {
        HardwareInfo {
            ram_bytes: Some(ram_gib * GIB_U),
            logical_cores: Some(8),
            gpu: vram_gib.map(|v| GpuInfo {
                name: "Test GPU".into(),
                vram_bytes: Some(v * GIB_U),
            }),
            unified_memory: apple,
            free_disk_bytes: Some(500 * GIB_U),
            os: "test".into(),
            arch: "test".into(),
        }
    }

    fn cands() -> Vec<Candidate> {
        ["m:4b", "m:8b", "m:30b", "m:70b"]
            .iter()
            .map(|id| Candidate {
                id: (*id).into(),
                display_name: (*id).into(),
            })
            .collect()
    }

    fn fits(advice: &LocalAdvice) -> Vec<(String, Fit)> {
        let mut v: Vec<_> = advice
            .models
            .iter()
            .map(|m| (m.model_id.clone(), m.fit))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    #[test]
    fn five_profiles() {
        use Fit::{Comfortable as C, Slow as S, TooLarge as T};
        // (profile, expected fits for 30b/4b/70b/8b in sorted-id order, recommended)
        let cases = [
            ("no gpu 8gb", hw(8, None, false), [T, C, T, T], Some("m:4b")),
            (
                "no gpu 32gb",
                hw(32, None, false),
                [T, C, T, S],
                Some("m:4b"),
            ),
            (
                "8gb vram",
                hw(16, Some(8), false),
                [T, C, T, C],
                Some("m:8b"),
            ),
            (
                "24gb vram",
                hw(32, Some(24), false),
                [C, C, T, C],
                Some("m:30b"),
            ),
            ("apple 16gb", hw(16, None, true), [T, C, T, C], Some("m:8b")),
        ];
        for (name, hw, expect, rec) in cases {
            let advice = recommend(&hw, &cands());
            let got: Vec<Fit> = fits(&advice).into_iter().map(|(_, f)| f).collect();
            assert_eq!(got, expect, "{name}");
            assert_eq!(advice.recommended.as_deref(), rec, "{name}");
        }
    }

    #[test]
    fn unknown_memory_and_empty_catalog_mean_no_recommendation() {
        let mut none = hw(8, None, false);
        none.ram_bytes = None;
        let advice = recommend(&none, &cands());
        assert_eq!(advice.basis, MemoryBasis::Unknown);
        assert!(advice.models.is_empty() && advice.recommended.is_none());
        assert!(recommend(&hw(32, None, false), &[]).recommended.is_none());
    }

    #[test]
    fn full_disk_blocks_the_recommendation() {
        let mut h = hw(16, Some(8), false);
        h.free_disk_bytes = Some(GIB_U);
        let advice = recommend(&h, &cands());
        assert!(advice.models.iter().all(|m| m.disk_ok == Some(false)));
        assert!(advice.recommended.is_none());
    }

    #[test]
    fn recommended_skips_a_model_that_does_not_fit_on_disk() {
        let mut h = hw(32, Some(24), false);
        // 30b needs ~19.5 GiB of download, 8b ~5.2, 4b ~2.6: only the smaller two fit on disk.
        h.free_disk_bytes = Some(8 * GIB_U);
        let advice = recommend(&h, &cands());
        assert_eq!(advice.recommended.as_deref(), Some("m:8b"));
    }

    #[test]
    fn flag_like_ids_never_become_commands() {
        let c = vec![
            Candidate {
                id: "-rf:8b".into(),
                display_name: "x".into(),
            },
            Candidate {
                id: "a b:8b".into(),
                display_name: "x".into(),
            },
        ];
        assert_eq!(recommend(&hw(32, Some(24), false), &c).models, Vec::new());
    }

    #[test]
    fn size_comes_from_the_id_tag_only() {
        assert_eq!(params_billions("qwen3:8b"), Some(8.0));
        assert_eq!(params_billions("llama3.3:70b-instruct-q4"), Some(70.0));
        assert_eq!(params_billions("gemma3:1.5b"), Some(1.5));
        assert_eq!(params_billions("latest"), None);
        assert_eq!(params_billions("model:latest"), None);
    }

    #[test]
    fn real_catalog_yields_ollama_candidates_with_sizes() {
        let c = catalog_candidates();
        assert_ne!(c, Vec::new());
        assert!(c.iter().all(|c| params_billions(&c.id).is_some()));
        let advice = recommend(&hw(32, Some(24), false), &c);
        let best = advice.recommended.expect("a recommendation");
        assert_eq!(advice.models[0].pull_command, format!("ollama pull {best}"));
    }

    #[test]
    fn path_lookup_only_checks_files() {
        let dir = tempfile::tempdir().expect("tmp");
        let name = if cfg!(windows) {
            "ollama.exe"
        } else {
            "ollama"
        };
        std::fs::write(dir.path().join(name), b"").expect("write");
        let var = std::env::join_paths([dir.path()]).expect("join");
        assert!(on_path("ollama", &var));
        assert!(!on_path("nothing-here", &var));
    }
}
