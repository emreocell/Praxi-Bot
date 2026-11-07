//! Embedding abstraction.

#[cfg(feature = "fastembed")]
use crate::error::MemoryError;
use crate::error::Result;

/// Turns text into fixed-size vectors.  Implementations should return
/// L2-normalised vectors (the store normalises again defensively).
pub trait Embedder: Send + Sync {
    /// Output dimension (constant for the lifetime of the embedder).
    fn dim(&self) -> usize;

    /// Embed a batch of texts.
    ///
    /// # Errors
    /// Fails if the model cannot run.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Deterministic feature-hashing bag-of-words embedder.  No model files, no
/// network: used by the tests and as a degraded fallback (lexical-ish
/// similarity only, no semantics).
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    /// Embedder with `dim` buckets (clamped to at least 8).
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(8) }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dim];
                for tok in t
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                {
                    let h = fnv1a(tok);
                    #[allow(clippy::cast_possible_truncation)]
                    let idx = (h % self.dim as u64) as usize;
                    v[idx] += if (h >> 63) == 0 { 1.0 } else { -1.0 };
                }
                normalize(&mut v);
                v
            })
            .collect())
    }
}

/// Scale `v` to unit length (no-op for the zero vector).
pub(crate) fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v {
            *x /= n;
        }
    }
}

/// Local ONNX embedder backed by fastembed-rs (feature `fastembed`).
///
/// Uses `multilingual-e5-small` (MIT licensed, 384 dims, English + multilingual).
/// Model files live under `<cache_dir>` (use `<data_dir>/models`); they are
/// downloaded only when `allow_download` is true.
#[cfg(feature = "fastembed")]
pub struct FastEmbedder {
    inner: std::sync::Mutex<fastembed::TextEmbedding>,
}

#[cfg(feature = "fastembed")]
impl FastEmbedder {
    /// Model dimension.
    pub const DIM: usize = 384;

    /// Load the model from `cache_dir`.
    ///
    /// # Errors
    /// Fails if the model is not cached and `allow_download` is false, or on
    /// load/download failure.
    pub fn new(cache_dir: impl Into<std::path::PathBuf>, allow_download: bool) -> Result<Self> {
        let cache_dir = cache_dir.into();
        let cached = cache_dir
            .join("models--intfloat--multilingual-e5-small")
            .exists()
            || cache_dir
                .join("models--Qdrant--multilingual-e5-small")
                .exists();
        if !cached && !allow_download {
            return Err(MemoryError::Embed(format!(
                "embedding model not present in {} and download is not enabled",
                cache_dir.display()
            )));
        }
        let opts = fastembed::InitOptions::new(fastembed::EmbeddingModel::MultilingualE5Small)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let model = fastembed::TextEmbedding::try_new(opts)
            .map_err(|e| MemoryError::Embed(e.to_string()))?;
        Ok(Self {
            inner: std::sync::Mutex::new(model),
        })
    }
}

#[cfg(feature = "fastembed")]
impl Embedder for FastEmbedder {
    fn dim(&self) -> usize {
        Self::DIM
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<String> = texts.iter().map(|t| format!("query: {t}")).collect();
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .embed(docs, None)
            .map_err(|e| MemoryError::Embed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_embedder_is_deterministic_and_normalised() {
        let e = HashEmbedder::default();
        let a = e.embed(&["I live in Berlin"]).unwrap();
        let b = e.embed(&["I live in Berlin"]).unwrap();
        assert_eq!(a, b);
        let n: f32 = a[0].iter().map(|x| x * x).sum();
        assert!((n - 1.0).abs() < 1e-5);
    }
}
