//! The pinned downloads. Data only; the numbers were read from the release pages
//! (GitHub release `digest` fields, Hugging Face `X-Linked-ETag`/`X-Linked-Size`) and the
//! model files are pinned to a commit, not to `main`.

use crate::{local_advice::Candidate, model_metadata::safe_model_id};

/// llama.cpp release the runtime archives below belong to (MIT licence, ggml-org/llama.cpp).
pub const LLAMA_CPP_TAG: &str = "b12633";

/// How a runtime archive is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

/// One downloadable `llama-server` build for an OS and CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeAsset {
    pub file_name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size_bytes: u64,
    pub kind: ArchiveKind,
    /// File name of the server program inside the archive.
    pub binary: &'static str,
}

const WIN_X64: RuntimeAsset = RuntimeAsset {
    file_name: "llama-b12633-bin-win-cpu-x64.zip",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-win-cpu-x64.zip",
    sha256: "bad9fe0c1b554bcb1ce5630d66ad34a2285a8b390400513cc02f56feafd62c8e",
    size_bytes: 25_038_006,
    kind: ArchiveKind::Zip,
    binary: "llama-server.exe",
};
const WIN_ARM64: RuntimeAsset = RuntimeAsset {
    file_name: "llama-b12633-bin-win-cpu-arm64.zip",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-win-cpu-arm64.zip",
    sha256: "c37f444b153e43a9b27c5d00b78c4c300bdb2878de76ab49112f4558a245e784",
    size_bytes: 13_149_546,
    kind: ArchiveKind::Zip,
    binary: "llama-server.exe",
};
const LINUX_X64: RuntimeAsset = RuntimeAsset {
    file_name: "llama-b12633-bin-ubuntu-x64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-ubuntu-x64.tar.gz",
    sha256: "9c4a2c1bd1437f22831e0b56cc75329b321bac87225ffb5c83cd8d13b1f44fa2",
    size_bytes: 23_225_608,
    kind: ArchiveKind::TarGz,
    binary: "llama-server",
};
const MAC_ARM64: RuntimeAsset = RuntimeAsset {
    file_name: "llama-b12633-bin-macos-arm64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-macos-arm64.tar.gz",
    sha256: "99657876053df9004753d92075813bbb3b484e79f8a402809b51d00c8c540062",
    size_bytes: 16_600_069,
    kind: ArchiveKind::TarGz,
    binary: "llama-server",
};
const MAC_X64: RuntimeAsset = RuntimeAsset {
    file_name: "llama-b12633-bin-macos-x64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-macos-x64.tar.gz",
    sha256: "42e69b4d89531197ec0551f288347a71856c4613e52030196486b888c82b8892",
    size_bytes: 12_382_012,
    kind: ArchiveKind::TarGz,
    binary: "llama-server",
};

/// The CPU build for this machine (`std::env::consts` spelling), or `None` when no pinned
/// build exists (for example Linux on ARM). GPU builds are deliberately not offered yet.
#[must_use]
pub fn runtime_asset(os: &str, arch: &str) -> Option<RuntimeAsset> {
    match (os, arch) {
        ("windows", "x86_64") => Some(WIN_X64),
        ("windows", "aarch64") => Some(WIN_ARM64),
        ("linux", "x86_64") => Some(LINUX_X64),
        ("macos", "aarch64") => Some(MAC_ARM64),
        ("macos", "x86_64") => Some(MAC_X64),
        _ => None,
    }
}

/// A curated, pinned GGUF model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedModel {
    /// Id with a size tag so the fit rules can read the parameter count (`qwen3:4b-q4`).
    pub id: &'static str,
    pub display_name: &'static str,
    pub file_name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size_bytes: u64,
    /// SPDX id of the weights' licence, shown in the picker.
    pub licence: &'static str,
    /// Least system RAM (GiB) the model is expected to need on a CPU-only run.
    pub min_ram_gib: u32,
}

const MODELS: [ManagedModel; 3] = [
    ManagedModel {
        id: "qwen3:1.7b-q4",
        display_name: "Qwen3 1.7B (small, fast)",
        file_name: "Qwen3-1.7B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3-1.7B-GGUF/resolve/d7f544eead698dbd1f15126ef60b45a1e1933222/Qwen3-1.7B-Q4_K_M.gguf",
        sha256: "b139949c5bd74937ad8ed8c8cf3d9ffb1e99c866c823204dc42c0d91fa181897",
        size_bytes: 1_107_409_472,
        licence: "Apache-2.0",
        min_ram_gib: 4,
    },
    ManagedModel {
        id: "qwen3:4b-q4",
        display_name: "Qwen3 4B Instruct (balanced)",
        file_name: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/a06e946bb6b655725eafa393f4a9745d460374c9/Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        sha256: "3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597",
        size_bytes: 2_497_281_120,
        licence: "Apache-2.0",
        min_ram_gib: 8,
    },
    ManagedModel {
        id: "qwen3:8b-q4",
        display_name: "Qwen3 8B (smarter, needs a strong computer)",
        file_name: "Qwen3-8B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3-8B-GGUF/resolve/a6adef130ffb23ddaf1a62fec9dced968c9bc482/Qwen3-8B-Q4_K_M.gguf",
        sha256: "120307ba529eb2439d6c430d94104dabd578497bc7bfe7e322b5d9933b449bd4",
        size_bytes: 5_027_784_512,
        licence: "Apache-2.0",
        min_ram_gib: 16,
    },
];

/// The curated list.
#[must_use]
pub fn managed_models() -> &'static [ManagedModel] {
    &MODELS
}

/// Looks a model up by exact id.
#[must_use]
pub fn managed_model(id: &str) -> Option<&'static ManagedModel> {
    MODELS.iter().find(|m| m.id == id)
}

/// The list as fit-rule candidates (see [`crate::local_advice::recommend`]).
#[must_use]
pub fn candidates() -> Vec<Candidate> {
    MODELS
        .iter()
        .filter(|m| safe_model_id(m.id))
        .map(|m| Candidate {
            id: m.id.to_owned(),
            display_name: m.display_name.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_advice::params_billions;

    #[test]
    fn every_pin_is_complete_and_https() {
        for m in managed_models() {
            assert!(m.url.starts_with("https://"), "{}", m.id);
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(m.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(m.size_bytes > 0 && m.url.ends_with(m.file_name));
            assert!(params_billions(m.id).is_some(), "{} needs a size tag", m.id);
        }
        for (os, arch) in [
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("linux", "x86_64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
        ] {
            let a = runtime_asset(os, arch).expect("asset");
            assert!(a.url.contains(LLAMA_CPP_TAG) && a.url.ends_with(a.file_name));
            assert_eq!(a.sha256.len(), 64);
        }
        assert!(runtime_asset("linux", "aarch64").is_none());
    }
}
