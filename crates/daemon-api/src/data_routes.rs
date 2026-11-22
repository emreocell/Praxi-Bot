//! Settings > Data: how big the data folder is per category (`GET /v1/data-usage`) and a
//! safe "Clear cache" (`POST /v1/data-usage/clear-cache`).
//!
//! Sizes come from directory metadata only: no file is ever opened or read. Clear cache
//! removes a fixed, short list of regenerable things (the model-list cache and tool-output
//! spill files older than a day). It never touches chats, memory, knowledge, backups, secrets
//! or settings: the targets are hard-coded names below, nothing comes from the request.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use axum::{Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{ApiError, AppState, error::ErrorBody};

/// Spill directories newer than this may belong to a run that is still going.
const SPILL_MIN_AGE: Duration = Duration::from_secs(24 * 3600);

/// Category ids and the data-folder entries they cover, in the order the UI shows them.
const CATEGORIES: &[(&str, &[&str])] = &[
    (
        "chats",
        &[
            "bot.db",
            "bot.db-wal",
            "bot.db-shm",
            "attachments",
            "runs",
            "screenshots",
        ],
    ),
    ("memory", &["memory.db", "memory.db-wal", "memory.db-shm"]),
    ("knowledge", &["bots", "browsers"]),
    ("backups", &["backups"]),
    ("caches", &["cache", "spill", "router"]),
];

/// One slice of the data folder.
#[derive(Debug, Serialize, ToSchema, PartialEq, Eq)]
pub struct DataCategoryDto {
    /// `chats`, `memory`, `knowledge`, `backups`, `caches` or `other`.
    pub id: String,
    /// Bytes used on disk by this category.
    pub bytes: u64,
}

/// Data folder sizes.
#[derive(Debug, Serialize, ToSchema)]
pub struct DataUsageDto {
    pub total_bytes: u64,
    pub categories: Vec<DataCategoryDto>,
    /// Bytes "Clear cache" would free right now.
    pub clearable_bytes: u64,
}

/// Result of "Clear cache".
#[derive(Debug, Serialize, ToSchema)]
pub struct ClearCacheDto {
    pub freed_bytes: u64,
}

/// Size of a file or directory tree. Symlinks are not followed.
fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return if meta.is_file() { meta.len() } else { 0 };
    }
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    rd.flatten().map(|e| tree_size(&e.path())).sum()
}

/// What "Clear cache" removes: everything inside `cache/`, plus spill entries not modified
/// for [`SPILL_MIN_AGE`].
fn clearable(data_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(data_dir.join("cache")) {
        out.extend(rd.flatten().map(|e| e.path()));
    }
    if let Ok(rd) = std::fs::read_dir(data_dir.join("spill")) {
        let now = SystemTime::now();
        out.extend(rd.flatten().filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            (now.duration_since(modified).unwrap_or_default() >= SPILL_MIN_AGE).then(|| e.path())
        }));
    }
    out
}

fn usage(data_dir: &Path) -> DataUsageDto {
    let mut named = std::collections::HashSet::new();
    let mut categories: Vec<DataCategoryDto> = CATEGORIES
        .iter()
        .map(|(id, names)| {
            named.extend(names.iter().copied());
            DataCategoryDto {
                id: (*id).into(),
                bytes: names.iter().map(|n| tree_size(&data_dir.join(n))).sum(),
            }
        })
        .collect();
    let other: u64 = std::fs::read_dir(data_dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| !named.contains(e.file_name().to_string_lossy().as_ref()))
            .map(|e| tree_size(&e.path()))
            .sum()
    });
    categories.push(DataCategoryDto {
        id: "other".into(),
        bytes: other,
    });
    DataUsageDto {
        total_bytes: categories.iter().map(|c| c.bytes).sum(),
        categories,
        clearable_bytes: clearable(data_dir).iter().map(|p| tree_size(p)).sum(),
    }
}

fn clear_cache(data_dir: &Path) -> u64 {
    let mut freed = 0;
    for p in clearable(data_dir) {
        let size = tree_size(&p);
        let is_dir = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
        let removed = if is_dir {
            std::fs::remove_dir_all(&p).is_ok()
        } else {
            std::fs::remove_file(&p).is_ok()
        };
        if removed {
            freed += size;
        }
    }
    freed
}

/// How much space each kind of data uses (owner only). Reads folder metadata, never contents.
#[utoipa::path(get, path = "/v1/data-usage", tag = "settings", security(("bearer" = [])),
    responses((status = 200, body = DataUsageDto), (status = 403, body = ErrorBody)))]
pub(crate) async fn get_data_usage(
    State(s): State<AppState>,
) -> Result<Json<DataUsageDto>, ApiError> {
    let dir = s.data_dir();
    let dto = tokio::task::spawn_blocking(move || usage(&dir))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(dto))
}

/// Remove regenerable caches only (owner only). Never touches chats, memory, knowledge,
/// backups, secrets or settings.
#[utoipa::path(post, path = "/v1/data-usage/clear-cache", tag = "settings", security(("bearer" = [])),
    responses((status = 200, body = ClearCacheDto), (status = 403, body = ErrorBody)))]
pub(crate) async fn post_clear_cache(
    State(s): State<AppState>,
) -> Result<Json<ClearCacheDto>, ApiError> {
    let dir = s.data_dir();
    let freed_bytes = tokio::task::spawn_blocking(move || clear_cache(&dir))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(ClearCacheDto { freed_bytes }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, rel: &str, len: usize) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![b'x'; len]).unwrap();
    }

    #[test]
    fn sizes_by_category_and_clear_cache_keeps_everything_else() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        put(r, "bot.db", 100);
        put(r, "memory.db", 20);
        put(r, "backups/bot-v1.db", 30);
        put(r, "bots/b1/knowledge.db", 40);
        put(r, "secrets.enc", 7);
        put(r, "cache/cli-models.json", 5);
        put(r, "router/a.sqlite", 9);
        put(r, "spill/old.txt", 11);
        put(r, "spill/fresh/out.txt", 13);
        // Make the "old" spill file look two days old.
        std::fs::File::options()
            .write(true)
            .open(r.join("spill/old.txt"))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(2 * 24 * 3600))
            .unwrap();

        let u = usage(r);
        let get = |id: &str| u.categories.iter().find(|c| c.id == id).unwrap().bytes;
        assert_eq!(get("chats"), 100);
        assert_eq!(get("memory"), 20);
        assert_eq!(get("backups"), 30);
        assert_eq!(get("knowledge"), 40);
        assert_eq!(get("caches"), 5 + 9 + 11 + 13);
        assert_eq!(get("other"), 7);
        assert_eq!(u.clearable_bytes, 5 + 11);

        assert_eq!(clear_cache(r), 16);
        for kept in [
            "bot.db",
            "memory.db",
            "backups/bot-v1.db",
            "bots/b1/knowledge.db",
            "secrets.enc",
            "router/a.sqlite",
            "spill/fresh/out.txt",
        ] {
            assert!(r.join(kept).exists(), "{kept} must survive Clear cache");
        }
        assert!(!r.join("cache/cli-models.json").exists());
        assert!(!r.join("spill/old.txt").exists());
        assert_eq!(usage(r).clearable_bytes, 0);
    }
}
