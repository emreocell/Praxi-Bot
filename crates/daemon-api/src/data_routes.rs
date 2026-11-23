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
    let Ok(root) = std::fs::canonicalize(data_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(dir) = real_subdir(&root, "cache") {
        out.extend(children(&dir, &root, false));
    }
    if let Some(dir) = real_subdir(&root, "spill") {
        out.extend(children(&dir, &root, true));
    }
    out
}

/// `root/name` when it is a real directory (not a symlink, junction or other reparse point)
/// that stays under `root`. Anything else is skipped, so a `cache` link pointing at
/// `backups/` or outside the data folder is never walked.
fn real_subdir(root: &Path, name: &str) -> Option<PathBuf> {
    let p = root.join(name);
    let meta = std::fs::symlink_metadata(&p).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() || is_reparse_point(&meta) {
        return None;
    }
    let canon = std::fs::canonicalize(&p).ok()?;
    (canon == p && canon.starts_with(root)).then_some(canon)
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Direct children of `dir` that may be deleted. A symlink child is listed as itself (it gets
/// unlinked, its target is left alone); a real child must canonicalize to a path under `root`.
fn children(dir: &Path, root: &Path, spill_age_check: bool) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    rd.flatten()
        .filter_map(|e| {
            let path = dir.join(e.file_name());
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if spill_age_check {
                let modified = meta.modified().ok()?;
                if now.duration_since(modified).unwrap_or_default() < SPILL_MIN_AGE {
                    return None;
                }
            }
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                return Some(path);
            }
            let canon = std::fs::canonicalize(&path).ok()?;
            canon.starts_with(root).then_some(canon)
        })
        .collect()
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
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        let removed = if meta.file_type().is_symlink() || is_reparse_point(&meta) {
            // Unlink only; never touch what it points at.
            std::fs::remove_file(&p).is_ok() || std::fs::remove_dir(&p).is_ok()
        } else if meta.is_dir() {
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

    #[cfg(unix)]
    #[test]
    fn symlinked_cache_or_spill_dir_never_deletes_the_target() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        put(r, "backups/bot-v1.db", 30);
        put(r, "spill-target/old.txt", 3);
        std::os::unix::fs::symlink(r.join("backups"), r.join("cache")).unwrap();
        std::os::unix::fs::symlink(r.join("spill-target"), r.join("spill")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(r.join("spill-target/old.txt"))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3 * 24 * 3600))
            .unwrap();
        assert_eq!(usage(r).clearable_bytes, 0);
        assert_eq!(clear_cache(r), 0);
        assert!(r.join("backups/bot-v1.db").exists());
        assert!(r.join("spill-target/old.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_child_is_unlinked_but_its_target_survives() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        put(r, "backups/bot-v1.db", 30);
        put(r, "cache/real.json", 5);
        std::os::unix::fs::symlink(r.join("backups"), r.join("cache/link")).unwrap();
        clear_cache(r);
        assert!(std::fs::symlink_metadata(r.join("cache/link")).is_err());
        assert!(!r.join("cache/real.json").exists());
        assert!(r.join("backups/bot-v1.db").exists());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs permission to create a directory junction (mklink /J)"]
    fn junctioned_cache_dir_never_deletes_the_target() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        put(r, "backups/bot-v1.db", 30);
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(r.join("cache"))
            .arg(r.join("backups"))
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "could not create the junction");
        assert_eq!(clear_cache(r), 0);
        assert!(r.join("backups/bot-v1.db").exists());
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
