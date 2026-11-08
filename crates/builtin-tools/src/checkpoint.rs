//! Edit checkpoints: copies of files taken before a destructive file tool runs.
//!
//! Layout: `<root>/<run>/<checkpoint id>/manifest.json` plus `files/<n>` holding
//! the backed up content of each path. Restoring puts every recorded path back
//! to its state at checkpoint time (paths that did not exist are removed) and
//! first takes a checkpoint of the current state so a restore can be undone.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One path recorded in a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointEntry {
    /// Absolute path that was backed up.
    pub path: String,
    /// Whether the path existed when the checkpoint was taken.
    pub existed: bool,
    /// Whether the path was a directory.
    pub is_dir: bool,
    /// Backup location relative to the checkpoint directory.
    pub backup: Option<String>,
    /// Total bytes in the copied payload (absent in legacy manifests).
    #[serde(default)]
    pub size: Option<u64>,
    /// SHA-256 integrity digest of the copied payload, not an authenticity proof.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Original tool-input path when this entry names its effective target.
    #[serde(default)]
    pub via_link_of: Option<String>,
}

/// Manifest describing one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Checkpoint id (`cp-...`).
    pub id: String,
    /// Run that created it.
    pub run_id: String,
    /// Creation time in unix milliseconds.
    pub created_ms: u64,
    /// Tool/operation that triggered it (e.g. `fs_write`).
    pub op: String,
    /// Recorded paths.
    pub entries: Vec<CheckpointEntry>,
    /// Payloads that could not be copied. Such checkpoints cannot be restored.
    #[serde(default)]
    pub skipped: Vec<String>,
    /// Integrity of the link metadata sidecar when links were recorded.
    #[serde(default)]
    pub links_sha256: Option<String>,
}

/// Checkpoint storage rooted at `<data_dir>/checkpoints`.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    root: PathBuf,
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() { "no-run".into() } else { t }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A symlink or junction met while backing up. Links are never followed (that
/// would copy whatever they point at, possibly forever); their target is
/// recorded and the link is recreated on restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LinkRecord {
    /// Backup-relative path the link occupies (`files/<n>/...`).
    rel: String,
    /// Link target as read from the filesystem.
    target: String,
    /// Whether it points at a directory (matters for Windows).
    dir: bool,
}

const LINKS_FILE: &str = "links.json";

/// Largest link target that may be copied when the link itself cannot be
/// recreated (same bound as `fs_read`).
const MAX_LINK_COPY_BYTES: u64 = 16 * 1024 * 1024;

fn is_link(m: &fs::Metadata) -> bool {
    // On Windows `is_symlink` is true for symlinks and junctions alike.
    m.file_type().is_symlink()
}

/// Stop a checkpoint that walks into its own output or a huge tree.
const MAX_CHECKPOINT_ENTRIES: usize = 10_000;
const MAX_CHECKPOINT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CHECKPOINT_DEPTH: usize = 32;

struct CopyBudget {
    entries: usize,
    bytes: u64,
    skipped: Vec<String>,
}

fn budget_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl CopyBudget {
    fn charge(&mut self, bytes: u64) -> io::Result<()> {
        self.entries = self.entries.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        if self.entries > MAX_CHECKPOINT_ENTRIES {
            return Err(budget_err(&format!(
                "checkpoint file budget exceeded ({MAX_CHECKPOINT_ENTRIES} entries)"
            )));
        }
        if self.bytes > MAX_CHECKPOINT_BYTES {
            return Err(budget_err(&format!(
                "checkpoint byte budget exceeded ({MAX_CHECKPOINT_BYTES} bytes)"
            )));
        }
        Ok(())
    }
}

/// A dependency-free SHA-256 used only to detect damaged checkpoint payloads.
/// The manifest is writable local data, so this is not an authenticity boundary.
struct PayloadSha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    bytes: u64,
}

impl PayloadSha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            block: [0; 64],
            filled: 0,
            bytes: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.bytes += data.len() as u64;
        while !data.is_empty() {
            let n = (64 - self.filled).min(data.len());
            self.block[self.filled..self.filled + n].copy_from_slice(&data[..n]);
            self.filled += n;
            data = &data[n..];
            if self.filled == 64 {
                self.compress();
                self.filled = 0;
            }
        }
    }

    #[allow(clippy::many_single_char_names)]
    fn compress(&mut self) {
        const K: [u32; 64] = [
            0x428a_2f98,
            0x7137_4491,
            0xb5c0_fbcf,
            0xe9b5_dba5,
            0x3956_c25b,
            0x59f1_11f1,
            0x923f_82a4,
            0xab1c_5ed5,
            0xd807_aa98,
            0x1283_5b01,
            0x2431_85be,
            0x550c_7dc3,
            0x72be_5d74,
            0x80de_b1fe,
            0x9bdc_06a7,
            0xc19b_f174,
            0xe49b_69c1,
            0xefbe_4786,
            0x0fc1_9dc6,
            0x240c_a1cc,
            0x2de9_2c6f,
            0x4a74_84aa,
            0x5cb0_a9dc,
            0x76f9_88da,
            0x983e_5152,
            0xa831_c66d,
            0xb003_27c8,
            0xbf59_7fc7,
            0xc6e0_0bf3,
            0xd5a7_9147,
            0x06ca_6351,
            0x1429_2967,
            0x27b7_0a85,
            0x2e1b_2138,
            0x4d2c_6dfc,
            0x5338_0d13,
            0x650a_7354,
            0x766a_0abb,
            0x81c2_c92e,
            0x9272_2c85,
            0xa2bf_e8a1,
            0xa81a_664b,
            0xc24b_8b70,
            0xc76c_51a3,
            0xd192_e819,
            0xd699_0624,
            0xf40e_3585,
            0x106a_a070,
            0x19a4_c116,
            0x1e37_6c08,
            0x2748_774c,
            0x34b0_bcb5,
            0x391c_0cb3,
            0x4ed8_aa4a,
            0x5b9c_ca4f,
            0x682e_6ff3,
            0x748f_82ee,
            0x78a5_636f,
            0x84c8_7814,
            0x8cc7_0208,
            0x90be_fffa,
            0xa450_6ceb,
            0xbef9_a3f7,
            0xc671_78f2,
        ];
        let mut words = [0u32; 64];
        for (out, bytes) in words.iter_mut().zip(self.block.as_chunks::<4>().0) {
            *out = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let a = words[i - 15];
            let b = words[i - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            words[i] = words[i - 16]
                .wrapping_add(s0)
                .wrapping_add(words[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(words[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, word) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(word);
        }
    }

    fn finish(mut self) -> String {
        let bits = self.bytes * 8;
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        self.state
            .iter()
            .fold(String::with_capacity(64), |mut out, word| {
                let _ = write!(out, "{word:08x}");
                out
            })
    }
}

fn payload_digest(path: &Path) -> io::Result<(u64, String)> {
    let mut budget = CopyBudget {
        entries: 0,
        bytes: 0,
        skipped: Vec::new(),
    };
    digest_node(path, 0, &mut budget)
}

fn digest_node(path: &Path, depth: usize, budget: &mut CopyBudget) -> io::Result<(u64, String)> {
    if depth > MAX_CHECKPOINT_DEPTH {
        return Err(budget_err("checkpoint payload exceeds depth budget"));
    }
    crate::files::vet_checkpoint_path(path)?;
    let meta = fs::symlink_metadata(path)?;
    if is_reparse(&meta) {
        return Err(budget_err("checkpoint payload is a link or reparse point"));
    }
    let mut hash = PayloadSha256::new();
    if meta.is_file() {
        budget.charge(meta.len())?;
        let mut file = fs::File::open(path)?;
        let mut buffer = [0u8; 16 * 1024];
        let mut bytes = 0;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > meta.len() {
                return Err(budget_err("checkpoint payload changed while reading"));
            }
            hash.update(&buffer[..n]);
        }
        if bytes != meta.len() {
            return Err(budget_err("checkpoint payload changed while reading"));
        }
        return Ok((bytes, hash.finish()));
    }
    if !meta.is_dir() {
        return Err(budget_err(
            "checkpoint payload is not a regular file or directory",
        ));
    }
    budget.charge(0)?;
    hash.update(b"praxi-checkpoint-directory\0");
    let mut children = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    let mut size = 0;
    for child in children {
        if crate::files::vet_checkpoint_node(&child.path())?
            == crate::files::CheckpointNode::SkipProtected
        {
            continue;
        }
        let name = child.file_name();
        let name = name.as_encoded_bytes();
        let (bytes, digest) = digest_node(&child.path(), depth + 1, budget)?;
        size += bytes;
        hash.update(&(name.len() as u64).to_be_bytes());
        hash.update(name);
        hash.update(&bytes.to_be_bytes());
        hash.update(digest.as_bytes());
    }
    Ok((size, hash.finish()))
}

/// Resolve parents but preserve the final link object, including dangling links.
fn canonical_entry(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    if fs::symlink_metadata(&abs).is_ok_and(|m| is_link(&m)) {
        let parent = abs
            .parent()
            .ok_or_else(|| budget_err("link has no parent"))?;
        let name = abs
            .file_name()
            .ok_or_else(|| budget_err("link has no name"))?;
        return Ok(canonical_dir(parent)?.join(name));
    }
    canonical_dir(&abs)
}

struct CheckpointSource {
    path: PathBuf,
    via_link_of: Option<String>,
}

fn add_source(sources: &mut Vec<CheckpointSource>, path: PathBuf, via: Option<String>) {
    if let Some(existing) = sources
        .iter_mut()
        .find(|source| conflict_key(&source.path) == conflict_key(&path))
    {
        if existing.via_link_of.is_none() {
            existing.via_link_of = via;
        }
    } else {
        sources.push(CheckpointSource {
            path,
            via_link_of: via,
        });
    }
}

fn collect_link_topology(
    input: &Path,
    depth: usize,
    links: &mut Vec<CheckpointSource>,
) -> io::Result<()> {
    if depth > MAX_CHECKPOINT_DEPTH {
        return Err(budget_err("checkpoint link chain is too deep"));
    }
    let components: Vec<_> = input.components().collect();
    let mut prefix = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if is_link(&meta) => {
                crate::files::vet_checkpoint_path(&prefix)?;
                let at = canonical_entry(&prefix)?;
                let via = (at != prefix).then(|| prefix.display().to_string());
                add_source(links, at, via);
                let target = fs::read_link(&prefix)?;
                let mut next = if target.is_absolute() {
                    target
                } else {
                    prefix
                        .parent()
                        .ok_or_else(|| budget_err("link has no parent"))?
                        .join(target)
                };
                for component in &components[index + 1..] {
                    next.push(component);
                }
                crate::files::vet_checkpoint_path(&next)?;
                return collect_link_topology(&next, depth + 1, links);
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn checkpoint_sources(paths: &[(&Path, &Path)]) -> io::Result<Vec<CheckpointSource>> {
    let mut sources = Vec::new();
    for (requested, effective) in paths {
        // Classify before canonicalize can follow an ancestor. Keep the
        // original input when a link leads to a separate effective target.
        crate::files::vet_checkpoint_path(requested)?;
        crate::files::vet_checkpoint_path(effective)?;
        let input = std::path::absolute(requested)?;
        let actual = canonical_entry(effective)?;
        let mut topology = Vec::new();
        collect_link_topology(&input, 0, &mut topology)?;
        let via = (!topology.is_empty() && input != actual).then(|| input.display().to_string());
        add_source(&mut sources, actual, via);
        for link in topology {
            add_source(&mut sources, link.path, link.via_link_of);
        }
    }
    Ok(sources)
}

/// Removes a half-written checkpoint directory unless disarmed after success.
struct PartialCheckpoint(Option<PathBuf>);

impl Drop for PartialCheckpoint {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

impl PartialCheckpoint {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

/// Absolute path with symlinks resolved for the longest prefix that exists.
/// Missing suffixes stay lexical so a not-yet-created backup can be compared.
fn canonical_dir(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    if abs.symlink_metadata().is_ok() && abs.exists() {
        return fs::canonicalize(&abs);
    }
    let mut base = PathBuf::new();
    let mut rest = Vec::new();
    let mut missing = false;
    for c in abs.components() {
        if missing {
            rest.push(c.as_os_str().to_os_string());
            continue;
        }
        let next = base.join(c);
        if next.symlink_metadata().is_ok() {
            base = next;
        } else {
            missing = true;
            rest.push(c.as_os_str().to_os_string());
        }
    }
    if base.as_os_str().is_empty() {
        return Ok(abs);
    }
    let mut out = fs::canonicalize(&base)?;
    for name in rest {
        out.push(name);
    }
    Ok(out)
}

fn conflict_key(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if cfg!(windows) {
        PathBuf::from(text.to_ascii_lowercase())
    } else {
        PathBuf::from(text)
    }
}

/// True when `a` and `b` are the same path or one contains the other.
fn paths_conflict(a: &Path, b: &Path) -> bool {
    let a = conflict_key(a);
    let b = conflict_key(b);
    a == b || a.starts_with(&b) || b.starts_with(&a)
}

/// Copy `from` to `to` without following links. Links are appended to `links`
/// with `rel` as their backup-relative path; no placeholder is written to `to`.
fn link_dir_nofollow(meta: &fs::Metadata) -> bool {
    // Directory junctions and directory symlinks carry the directory attribute
    // on the link itself. `fs::metadata` would follow the target.
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

fn is_reparse(meta: &fs::Metadata) -> bool {
    if is_link(meta) {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn copy_recursive(
    from: &Path,
    to: &Path,
    rel: &str,
    links: &mut Vec<LinkRecord>,
    depth: usize,
    budget: &mut CopyBudget,
) -> io::Result<()> {
    if depth > MAX_CHECKPOINT_DEPTH {
        return Err(budget_err(&format!(
            "checkpoint depth budget exceeded ({MAX_CHECKPOINT_DEPTH})"
        )));
    }
    // A protected descendant is skipped (not copied, not followed). The
    // top-level path is rejected by `vet_checkpoint_path` in `create` before
    // this runs, so a request whose own path is credential still fails.
    // Any other vet failure (UNC, device, roots) still aborts the checkpoint.
    match crate::files::vet_checkpoint_node(from)? {
        crate::files::CheckpointNode::SkipProtected => return Ok(()),
        crate::files::CheckpointNode::Proceed => {}
    }
    let meta = fs::symlink_metadata(from)?;
    if is_link(&meta) {
        budget.charge(0)?;
        let target = fs::read_link(from)?;
        let dir = link_dir_nofollow(&meta);
        links.push(LinkRecord {
            rel: rel.to_owned(),
            target: target.display().to_string(),
            dir,
        });
        Ok(())
    } else if !meta.is_dir() && crate::files::cloud_placeholder(from, &meta) {
        // Online-only placeholder: `read_link` fails and the logical size can
        // exceed the backup budget. Copy when it fits; otherwise leave it out
        // of the blob. Either way the parent delete or move still proceeds.
        copy_placeholder(from, to, meta.len(), budget, |source, destination| {
            fs::copy(source, destination)
        })
    } else if meta.is_dir() {
        budget.charge(0)?;
        fs::create_dir_all(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            let name = e.file_name();
            copy_recursive(
                &e.path(),
                &to.join(&name),
                &format!("{rel}/{}", name.to_string_lossy()),
                links,
                depth + 1,
                budget,
            )?;
        }
        Ok(())
    } else {
        budget.charge(meta.len())?;
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        let copied = fs::copy(from, to)?;
        if copied != meta.len() {
            return Err(budget_err("source size changed while checkpointing"));
        }
        Ok(())
    }
}

fn copy_placeholder(
    from: &Path,
    to: &Path,
    logical: u64,
    budget: &mut CopyBudget,
    copy: impl FnOnce(&Path, &Path) -> io::Result<u64>,
) -> io::Result<()> {
    let fits = budget.entries < MAX_CHECKPOINT_ENTRIES
        && budget.bytes.saturating_add(logical) <= MAX_CHECKPOINT_BYTES;
    if fits {
        budget.charge(logical)?;
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let copied = copy(from, to).and_then(|bytes| {
            if bytes == logical {
                Ok(bytes)
            } else {
                Err(budget_err("cloud placeholder size changed while copying"))
            }
        });
        if let Err(err) = copied {
            // A failed copy can leave a truncated destination. Never present
            // that partial file as a complete checkpoint payload.
            let _ = fs::remove_file(to);
            budget.skipped.push(format!("{}: {err}", from.display()));
        }
    } else {
        budget.skipped.push(format!(
            "{}: cloud placeholder exceeds checkpoint copy budget",
            from.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn make_link(target: &Path, at: &Path, _dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn make_link(target: &Path, at: &Path, dir: bool) -> io::Result<()> {
    if !dir {
        return std::os::windows::fs::symlink_file(target, at);
    }
    // Directory symlinks need a privilege most users lack; junctions (what
    // pnpm creates) do not, so fall back to one.
    std::os::windows::fs::symlink_dir(target, at).or_else(|e| {
        let t = target.display().to_string();
        let t = t.strip_prefix(r"\\?\").unwrap_or(&t).to_owned();
        // `mklink` is a cmd.exe builtin, so both paths pass through cmd's
        // parser. Both come from the workspace (attacker-influenced): refuse
        // anything cmd would interpret rather than trust argument quoting.
        if cmd_unsafe(&t) || cmd_unsafe(&at.display().to_string()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains shell metacharacters; refusing to create a junction",
            ));
        }
        let ok = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(at)
            .arg(&t)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok { Ok(()) } else { Err(e) }
    })
}

/// True when `s` holds a character that cmd.exe treats specially (command
/// separators, redirection, `%VAR%` / `!VAR!` expansion, quotes, control
/// characters).
#[cfg_attr(not(windows), allow(dead_code))]
fn cmd_unsafe(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() || matches!(c, '&' | '|' | '^' | '<' | '>' | '%' | '"' | '!'))
}

struct LinkRestore {
    applied: bool,
    warning: Option<String>,
}

impl LinkRestore {
    fn skipped(message: String) -> Self {
        Self {
            applied: false,
            warning: Some(message),
        }
    }
}

/// Recreate a link; failed link creation may copy a bounded regular referent.
/// Callers use `applied`, never path existence, to distinguish a complete copy
/// from a failed operation that happened to leave a temporary path behind.
fn restore_link(target: &Path, at: &Path, dir: bool) -> LinkRestore {
    restore_link_with(target, at, dir, make_link)
}

fn restore_link_with(
    target: &Path,
    at: &Path,
    dir: bool,
    create_link: impl FnOnce(&Path, &Path, bool) -> io::Result<()>,
) -> LinkRestore {
    restore_link_with_ops(target, at, dir, create_link, |from, to| fs::copy(from, to))
}

fn restore_link_with_ops(
    target: &Path,
    at: &Path,
    dir: bool,
    create_link: impl FnOnce(&Path, &Path, bool) -> io::Result<()>,
    copy: impl FnOnce(&Path, &Path) -> io::Result<u64>,
) -> LinkRestore {
    if let Err(err) = crate::files::vet_checkpoint_write(at)
        .and_then(|()| crate::files::vet_link_target(at, target))
    {
        return LinkRestore::skipped(format!(
            "link {} -> {} skipped: {err}",
            at.display(),
            target.display()
        ));
    }
    let err = match create_link(target, at, dir) {
        Ok(()) => {
            return LinkRestore {
                applied: true,
                warning: None,
            };
        }
        Err(err) => err,
    };
    let resolved = if target.is_relative() {
        at.parent()
            .map_or_else(|| target.to_path_buf(), |parent| parent.join(target))
    } else {
        target.to_path_buf()
    };
    if let Err(reason) = crate::files::vet_checkpoint_path(&resolved) {
        return LinkRestore::skipped(format!(
            "link {} -> {} could not be recreated ({err}); skipped: {reason}",
            at.display(),
            target.display()
        ));
    }
    let fallback = match fs::symlink_metadata(&resolved) {
        Ok(meta) if is_reparse(&meta) => Err(budget_err("target is a reparse point (not copied)")),
        Ok(meta) if meta.is_file() && meta.len() > MAX_LINK_COPY_BYTES => {
            Err(budget_err(&format!(
                "target is larger than {} MiB",
                MAX_LINK_COPY_BYTES / (1024 * 1024)
            )))
        }
        Ok(meta) if meta.is_file() => fallback_copy(&resolved, at, meta.len(), copy),
        Ok(meta) if meta.is_dir() => Err(budget_err("target is a directory (not copied)")),
        Ok(_) => Err(budget_err("target is not a regular file")),
        Err(err) => Err(err),
    };
    match fallback {
        Ok(cleanup) => LinkRestore {
            applied: true,
            warning: Some(format!(
                "link {} -> {} could not be recreated ({err}); restored a copy of its content instead{}",
                at.display(),
                target.display(),
                if cleanup.is_empty() {
                    String::new()
                } else {
                    format!("; {}", cleanup.join("; "))
                }
            )),
        },
        Err(reason) => LinkRestore::skipped(format!(
            "link {} -> {} could not be recreated ({err}); skipped: {reason}",
            at.display(),
            target.display()
        )),
    }
}

fn fallback_copy(
    source: &Path,
    at: &Path,
    expected_size: u64,
    copy: impl FnOnce(&Path, &Path) -> io::Result<u64>,
) -> io::Result<Vec<String>> {
    match fs::symlink_metadata(at) {
        Ok(meta) if !meta.is_file() || is_reparse(&meta) => {
            return Err(budget_err("copy destination is not a regular file"));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let expected = payload_digest(source)?;
    if expected.0 != expected_size {
        return Err(budget_err("link referent changed while copying"));
    }
    let staged = StagedPath(adjacent_temp(at, "link-copy")?);
    crate::files::vet_checkpoint_temporary(&staged.0, at)?;
    let copied = copy(source, &staged.0)?;
    if copied != expected_size || payload_digest(&staged.0)? != expected {
        return Err(budget_err("incomplete link fallback copy"));
    }
    let mut tx = RestoreTransaction::new();
    if let Err(err) = tx.apply(at, Some(&staged.0)) {
        let rollback = tx.rollback();
        return Err(io::Error::new(
            err.kind(),
            format!("{err}; {}", rollback.join("; ")),
        ));
    }
    let mut warnings = Vec::new();
    tx.finish(&mut warnings);
    Ok(warnings)
}

fn read_links(dir: &Path) -> io::Result<Vec<LinkRecord>> {
    match fs::read(dir.join(LINKS_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| budget_err(&format!("invalid checkpoint links: {err}"))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Copy onto a destination-volume staging path, preserving links without
/// following them. A partial cloud copy aborts the move, leaving its source.
pub(crate) fn copy_move_payload(from: &Path, staged: &Path) -> io::Result<()> {
    let mut links = Vec::new();
    let mut budget = CopyBudget {
        entries: 0,
        bytes: 0,
        skipped: Vec::new(),
    };
    copy_recursive(from, staged, "payload", &mut links, 0, &mut budget)?;
    if !budget.skipped.is_empty() {
        return Err(budget_err(&format!(
            "incomplete move copy: {}",
            budget.skipped.join("; ")
        )));
    }
    for link in links {
        let suffix = crate::files::store_rel_suffix("payload", &link.rel)?
            .ok_or_else(|| budget_err("invalid staged link location"))?;
        let at = crate::files::join_store_parts(staged, &suffix)?;
        crate::files::vet_checkpoint_write(&at)?;
        crate::files::vet_link_target(&at, Path::new(&link.target))?;
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent)?;
        }
        make_link(Path::new(&link.target), &at, link.dir)?;
    }
    Ok(())
}

fn validate_payloads(dir: &Path, cp: &Checkpoint, links: &[LinkRecord]) -> io::Result<()> {
    let mut problems = cp.skipped.clone();
    if let Some(expected) = &cp.links_sha256 {
        match fs::read(dir.join(LINKS_FILE)) {
            Ok(bytes) => {
                let mut hash = PayloadSha256::new();
                hash.update(&bytes);
                if hash.finish() != *expected {
                    problems.push("link metadata sha256 mismatch".into());
                }
            }
            Err(err) => problems.push(format!("link metadata: {err}")),
        }
    }
    for entry in &cp.entries {
        let Some(rel) = &entry.backup else {
            if entry.existed {
                problems.push(format!("{}: missing backup reference", entry.path));
            }
            continue;
        };
        let source = crate::files::join_store_rel(dir, rel)?;
        crate::files::vet_checkpoint_path(&source)?;
        // Before integrity fields existed, root links had metadata only.
        let legacy_link = entry.size.is_none()
            && entry.sha256.is_none()
            && links.iter().any(|link| link.rel == *rel);
        if legacy_link && !source.try_exists()? {
            continue;
        }
        match payload_digest(&source) {
            Ok((size, digest)) => {
                if entry.size.is_some_and(|expected| expected != size) {
                    problems.push(format!("{}: payload size mismatch", entry.path));
                }
                if entry
                    .sha256
                    .as_ref()
                    .is_some_and(|expected| *expected != digest)
                {
                    problems.push(format!("{}: payload sha256 mismatch", entry.path));
                }
            }
            Err(err) => problems.push(format!("{}: {err}", entry.path)),
        }
    }
    if !problems.is_empty() {
        return Err(budget_err(&format!(
            "checkpoint {} is incomplete: {}",
            cp.id,
            problems.join("; ")
        )));
    }
    Ok(())
}

pub(crate) fn adjacent_temp(path: &Path, label: &str) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| budget_err("target has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| budget_err("target has no filename"))?;
    let mut temp = name.to_os_string();
    temp.push(format!(".praxi-{label}-{}", uuid::Uuid::now_v7().simple()));
    Ok(parent.join(temp))
}

/// Cleanup is always no-follow. Never remove a symlink's referent.
pub(crate) fn remove_staged(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if is_reparse(&meta) => fs::remove_file(path).or_else(|_| fs::remove_dir(path)),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

struct StagedPath(PathBuf);

impl Drop for StagedPath {
    fn drop(&mut self) {
        let _ = remove_staged(&self.0);
    }
}

struct RestoreSwap {
    target: PathBuf,
    saved: Option<PathBuf>,
    installed: bool,
}

struct RestoreTransaction {
    swaps: Vec<RestoreSwap>,
    created_dirs: Vec<PathBuf>,
}

impl RestoreTransaction {
    fn new() -> Self {
        Self {
            swaps: Vec::new(),
            created_dirs: Vec::new(),
        }
    }

    fn ensure_parent(&mut self, path: &Path) -> io::Result<()> {
        let Some(parent) = path.parent() else {
            return Err(budget_err("target has no parent"));
        };
        let mut missing = Vec::new();
        let mut cursor = parent;
        while !cursor.try_exists()? {
            missing.push(cursor.to_path_buf());
            cursor = cursor
                .parent()
                .ok_or_else(|| budget_err("target has no existing ancestor"))?;
        }
        for dir in missing.into_iter().rev() {
            fs::create_dir(&dir)?;
            self.created_dirs.push(dir);
        }
        Ok(())
    }

    fn apply(&mut self, target: &Path, staged: Option<&Path>) -> io::Result<()> {
        crate::files::vet_checkpoint_write(target)?;
        if crate::files::checkpoint_has_protected_descendant(target)? {
            // Keep protected names at their original locations. Only their
            // mutable siblings participate in the rename transaction.
            if let Some(staged) = staged
                && !fs::symlink_metadata(staged)?.is_dir()
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "destination still contains a protected file",
                ));
            }
            let mut names = std::collections::BTreeSet::new();
            for child in fs::read_dir(target)? {
                names.insert(child?.file_name());
            }
            if let Some(staged) = staged {
                for child in fs::read_dir(staged)? {
                    names.insert(child?.file_name());
                }
            }
            for name in names {
                let at = target.join(&name);
                if crate::files::checkpoint_entry_is_protected(&at)? {
                    continue;
                }
                let replacement = staged.map(|dir| dir.join(name));
                let replacement = match replacement {
                    Some(path) if fs::symlink_metadata(&path).is_ok() => Some(path),
                    _ => None,
                };
                self.apply(&at, replacement.as_deref())?;
            }
            return Ok(());
        }
        let existing = match fs::symlink_metadata(target) {
            Ok(_) => true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => return Err(err),
        };
        if !existing && staged.is_none() {
            return Ok(());
        }
        self.ensure_parent(target)?;
        let saved = if existing {
            let aside = adjacent_temp(target, "restore-old")?;
            crate::files::vet_checkpoint_temporary(&aside, target)?;
            fs::rename(target, &aside)?;
            Some(aside)
        } else {
            None
        };
        self.swaps.push(RestoreSwap {
            target: target.to_path_buf(),
            saved,
            installed: false,
        });
        if let Some(staged) = staged {
            fs::rename(staged, target)?;
            if let Some(swap) = self.swaps.last_mut() {
                swap.installed = true;
            }
        }
        Ok(())
    }

    fn rollback(&mut self) -> Vec<String> {
        let mut problems = Vec::new();
        for swap in self.swaps.iter().rev() {
            let displaced = if swap.installed {
                let result = (|| {
                    let path = adjacent_temp(&swap.target, "restore-aborted")?;
                    crate::files::vet_checkpoint_temporary(&path, &swap.target)?;
                    fs::rename(&swap.target, &path)?;
                    Ok::<_, io::Error>(path)
                })();
                match result {
                    Ok(path) => Some(path),
                    Err(err) => {
                        problems.push(format!("{}: {err}", swap.target.display()));
                        continue;
                    }
                }
            } else {
                None
            };
            if let Some(saved) = &swap.saved
                && let Err(err) = fs::rename(saved, &swap.target)
            {
                // Even a failed rollback must not leave the visible path
                // empty when the staged replacement can be put back.
                if let Some(path) = &displaced {
                    let _ = fs::rename(path, &swap.target);
                }
                problems.push(format!(
                    "{} (previous bytes at {}): {err}",
                    swap.target.display(),
                    saved.display()
                ));
                continue;
            }
            if let Some(path) = displaced
                && let Err(err) = crate::files::remove_checkpoint_target(&path)
            {
                problems.push(format!(
                    "aborted replacement retained at {}: {err}",
                    path.display()
                ));
            }
        }
        for dir in self.created_dirs.iter().rev() {
            let _ = fs::remove_dir(dir); // only removes an empty directory
        }
        problems
    }

    fn finish(self, warnings: &mut Vec<String>) {
        for swap in self.swaps {
            if let Some(saved) = swap.saved
                && let Err(err) = crate::files::remove_checkpoint_target(&saved)
            {
                warnings.push(format!(
                    "previous bytes retained at {}: {err}",
                    saved.display()
                ));
            }
        }
    }
}

fn root_link<'a>(entry: &CheckpointEntry, links: &'a [LinkRecord]) -> Option<&'a LinkRecord> {
    entry
        .backup
        .as_ref()
        .and_then(|rel| links.iter().find(|link| link.rel == *rel))
}

fn unchanged_link(target: &Path, link: &LinkRecord) -> bool {
    fs::read_link(target).is_ok_and(|current| current == Path::new(&link.target))
        && fs::symlink_metadata(target).is_ok_and(|meta| link_dir_nofollow(&meta) == link.dir)
}

fn stage_restore(
    dir: &Path,
    entry: &CheckpointEntry,
    links: &[LinkRecord],
    strict_links: bool,
    tx: &mut RestoreTransaction,
    warnings: &mut Vec<String>,
    apply_link: &impl Fn(&Path, &Path, bool) -> LinkRestore,
) -> io::Result<Option<StagedPath>> {
    let Some(rel) = &entry.backup else {
        return Ok(None);
    };
    let target = Path::new(&entry.path);
    let staged = StagedPath(adjacent_temp(target, "restore")?);
    crate::files::vet_checkpoint_temporary(&staged.0, target)?;
    tx.ensure_parent(&staged.0)?;
    if let Some(link) = root_link(entry, links) {
        let outcome = apply_link(Path::new(&link.target), &staged.0, link.dir);
        warnings.extend(outcome.warning);
        if !outcome.applied {
            return Ok(None);
        }
        fs::symlink_metadata(&staged.0)?;
        return Ok(Some(staged));
    }
    let source = crate::files::join_store_rel(dir, rel)?;
    let mut ignored = Vec::new();
    let mut budget = CopyBudget {
        entries: 0,
        bytes: 0,
        skipped: Vec::new(),
    };
    copy_recursive(&source, &staged.0, rel, &mut ignored, 0, &mut budget)?;
    if !budget.skipped.is_empty() {
        return Err(budget_err(&format!(
            "staging skipped payloads: {}",
            budget.skipped.join("; ")
        )));
    }
    let (size, digest) = payload_digest(&staged.0)?;
    if entry.size.is_some_and(|expected| expected != size)
        || entry
            .sha256
            .as_ref()
            .is_some_and(|expected| *expected != digest)
    {
        return Err(budget_err("checkpoint payload changed while staging"));
    }
    for link in links {
        let Some(suffix) = crate::files::store_rel_suffix(rel, &link.rel)? else {
            continue;
        };
        let final_at = crate::files::join_store_parts(target, &suffix)?;
        // Check the final name as well as the temporary name. In particular a
        // staged directory must not hide a protected original destination.
        let permitted = crate::files::vet_checkpoint_write(&final_at)
            .and_then(|()| crate::files::vet_link_target(&final_at, Path::new(&link.target)));
        if let Err(err) = permitted {
            if strict_links || fs::symlink_metadata(&final_at).is_ok() {
                return Err(err);
            }
            warnings.push(format!(
                "link {} -> {} skipped: {err}",
                final_at.display(),
                link.target
            ));
            continue;
        }
        let at = crate::files::join_store_parts(&staged.0, &suffix)?;
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent)?;
        }
        let outcome = apply_link(Path::new(&link.target), &at, link.dir);
        if !outcome.applied && (strict_links || fs::symlink_metadata(&final_at).is_ok()) {
            return Err(budget_err(&format!(
                "{}; current nested entry preserved",
                outcome.warning.as_deref().unwrap_or("link restore failed")
            )));
        }
        warnings.extend(outcome.warning);
    }
    Ok(Some(staged))
}

impl CheckpointStore {
    /// Create a store under `<data_dir>/checkpoints`.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("checkpoints"),
        }
    }

    /// Back up `paths` (those that exist) and write a manifest. Returns the id.
    ///
    /// Source and destination are canonicalized first. A source that is the
    /// backup directory, contains it, or sits inside it is rejected before
    /// anything is created. Copying is bounded by file, byte and depth budgets.
    /// A failure deletes the partial checkpoint directory.
    ///
    /// # Errors
    /// Ancestor overlap, a budget exceeded, or I/O errors while copying or
    /// writing the manifest.
    pub fn create(&self, run_id: &str, op: &str, paths: &[&Path]) -> io::Result<String> {
        let paths: Vec<_> = paths.iter().map(|path| (*path, *path)).collect();
        self.create_for_tool(run_id, op, &paths)
    }

    /// Snapshot original input topology together with the effective write
    /// target already approved by the file-tool resolver.
    pub(crate) fn create_for_tool(
        &self,
        run_id: &str,
        op: &str,
        paths: &[(&Path, &Path)],
    ) -> io::Result<String> {
        let id = format!("cp-{}", uuid::Uuid::now_v7().simple());
        let dir = self.root.join(sanitize(run_id)).join(&id);
        let dest = canonical_dir(&dir)?;
        let sources = checkpoint_sources(paths)?;
        for source in &sources {
            if paths_conflict(&source.path, &dest) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "refusing checkpoint: source {} and backup {} are the same path or one is an ancestor of the other",
                        source.path.display(),
                        dest.display()
                    ),
                ));
            }
        }
        fs::create_dir_all(dir.join("files"))?;
        let mut partial = PartialCheckpoint(Some(dir.clone()));
        let mut entries = Vec::new();
        let mut links = Vec::new();
        let mut budget = CopyBudget {
            entries: 0,
            bytes: 0,
            skipped: Vec::new(),
        };
        for (n, source) in sources.iter().enumerate() {
            let abs = &source.path;
            let meta = match fs::symlink_metadata(abs) {
                Ok(meta) => Some(meta),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(err),
            };
            let existed = meta.is_some();
            let is_dir = meta.as_ref().is_some_and(fs::Metadata::is_dir);
            let backup = if existed {
                let rel = format!("files/{n}");
                let payload = dir.join(&rel);
                copy_recursive(abs, &payload, &rel, &mut links, 0, &mut budget)?;
                if meta.as_ref().is_some_and(is_link) {
                    // Root links have a real payload marker in new manifests.
                    // Nested links remain metadata only and are never followed.
                    fs::write(&payload, b"praxi-checkpoint-link\0")?;
                }
                Some(rel)
            } else {
                None
            };
            let integrity = backup
                .as_ref()
                .and_then(|rel| {
                    let path = dir.join(rel);
                    path.exists().then_some(path)
                })
                .map(|path| payload_digest(&path))
                .transpose()?;
            entries.push(CheckpointEntry {
                path: abs.display().to_string(),
                existed,
                is_dir,
                backup,
                size: integrity.as_ref().map(|(size, _)| *size),
                sha256: integrity.map(|(_, hash)| hash),
                via_link_of: source.via_link_of.clone(),
            });
        }
        let link_json = if links.is_empty() {
            None
        } else {
            Some(serde_json::to_vec_pretty(&links).map_err(io::Error::other)?)
        };
        let links_sha256 = link_json.as_ref().map(|bytes| {
            let mut hash = PayloadSha256::new();
            hash.update(bytes);
            hash.finish()
        });
        let cp = Checkpoint {
            id: id.clone(),
            run_id: run_id.to_string(),
            created_ms: now_ms(),
            op: op.to_string(),
            entries,
            skipped: budget.skipped,
            links_sha256,
        };
        let json = serde_json::to_vec_pretty(&cp).map_err(io::Error::other)?;
        if let Some(bytes) = link_json {
            fs::write(dir.join(LINKS_FILE), bytes)?;
        }
        fs::write(dir.join("manifest.json"), json)?;
        partial.disarm();
        Ok(id)
    }

    pub(crate) fn skipped(&self, id: &str) -> io::Result<Vec<String>> {
        self.find(id).map(|(_, cp)| cp.skipped)
    }

    /// List the checkpoints of a run, oldest first.
    ///
    /// # Errors
    /// I/O errors other than a missing directory.
    pub fn list(&self, run_id: &str) -> io::Result<Vec<Checkpoint>> {
        let dir = self.root.join(sanitize(run_id));
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for e in rd {
            let m = e?.path().join("manifest.json");
            if let Ok(bytes) = fs::read(&m)
                && let Ok(cp) = serde_json::from_slice::<Checkpoint>(&bytes)
            {
                out.push(cp);
            }
        }
        out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    fn find(&self, id: &str) -> io::Result<(PathBuf, Checkpoint)> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid checkpoint id",
            ));
        }
        let runs = match fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("checkpoint {id} not found"),
                ));
            }
            Err(e) => return Err(e),
        };
        for run in runs {
            let dir = run?.path().join(id);
            if let Ok(bytes) = fs::read(dir.join("manifest.json")) {
                let cp = serde_json::from_slice::<Checkpoint>(&bytes).map_err(io::Error::other)?;
                return Ok((dir, cp));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("checkpoint {id} not found"),
        ))
    }

    /// Restore a checkpoint. Returns the restored paths and the id of the
    /// safety checkpoint taken of the state that was overwritten.
    ///
    /// # Errors
    /// Unknown id or I/O errors.
    pub fn restore(&self, id: &str) -> io::Result<(Vec<String>, String)> {
        self.restore_with_warnings(id).map(|(p, s, _)| (p, s))
    }

    /// Like [`Self::restore`], also reporting bounded link-copy fallbacks.
    /// A skipped root link leaves its current path intact. A failed nested
    /// link never removes an existing current child; later errors roll back
    /// earlier swaps before returning.
    ///
    /// # Errors
    /// Unknown id or I/O errors.
    pub fn restore_with_warnings(
        &self,
        id: &str,
    ) -> io::Result<(Vec<String>, String, Vec<String>)> {
        self.restore_internal(id, |_| Ok(()))
    }

    fn restore_internal(
        &self,
        id: &str,
        before_entry: impl Fn(usize) -> io::Result<()>,
    ) -> io::Result<(Vec<String>, String, Vec<String>)> {
        self.restore_using(id, before_entry, restore_link)
    }

    fn restore_using(
        &self,
        id: &str,
        before_entry: impl Fn(usize) -> io::Result<()>,
        apply_link: impl Fn(&Path, &Path, bool) -> LinkRestore,
    ) -> io::Result<(Vec<String>, String, Vec<String>)> {
        let (dir, cp) = self.find(id)?;
        let links = read_links(&dir)?;
        // Reject forged relative paths before any join, stat or mutation.
        for entry in &cp.entries {
            if let Some(rel) = &entry.backup {
                crate::files::plain_store_components(rel)?;
            }
            crate::files::vet_checkpoint_write(Path::new(&entry.path))?;
        }
        for link in &links {
            crate::files::plain_store_components(&link.rel)?;
        }
        let current: Vec<&Path> = cp
            .entries
            .iter()
            .map(|entry| Path::new(&entry.path))
            .collect();
        // Keep an undo checkpoint even when payload validation subsequently
        // fails. No user path has been changed at this point.
        let safety = self.create(&cp.run_id, &format!("pre-restore:{id}"), &current)?;
        validate_payloads(&dir, &cp, &links).map_err(|err| io::Error::new(err.kind(), format!(
            "{err}; current state saved as {safety}; restore with checkpoint_restore {safety}"
        )))?;
        let mut tx = RestoreTransaction::new();
        let mut restored = Vec::new();
        let mut warnings = Vec::new();
        let result: io::Result<()> = (|| {
            for (index, entry) in cp.entries.iter().enumerate() {
                before_entry(index)?;
                let target = Path::new(&entry.path);
                if root_link(entry, &links).is_some_and(|link| unchanged_link(target, link)) {
                    // A write through the link changed only the referent;
                    // leave the original link object and its metadata alone.
                    continue;
                }
                let staged = stage_restore(
                    &dir,
                    entry,
                    &links,
                    cp.links_sha256.is_some(),
                    &mut tx,
                    &mut warnings,
                    &apply_link,
                )?;
                if entry.backup.is_some() && staged.is_none() {
                    // Link creation and its bounded fallback both failed.
                    // Preserve the current path and do not claim it restored.
                    continue;
                }
                tx.apply(target, staged.as_ref().map(|path| path.0.as_path()))?;
                restored.push(entry.path.clone());
            }
            Ok(())
        })();
        if let Err(err) = result {
            let rollback = tx.rollback();
            let suffix = if rollback.is_empty() {
                "current files rolled back".to_owned()
            } else {
                format!("rollback incomplete: {}", rollback.join("; "))
            };
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "restore checkpoint {id} failed: {err}; {suffix}; restore with checkpoint_restore {safety}"
                ),
            ));
        }
        tx.finish(&mut warnings);
        Ok((restored, safety, warnings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_sha256_matches_standard_vectors_and_chunk_boundaries() {
        let vectors = [
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
        ];
        for (input, expected) in vectors {
            for chunk in [1, 2, 7, 55, 56, 63, 64, 65, 128] {
                let mut hash = PayloadSha256::new();
                for bytes in input.as_bytes().chunks(chunk) {
                    hash.update(bytes);
                }
                assert_eq!(
                    hash.finish(),
                    expected,
                    "input length {}, chunk {chunk}",
                    input.len()
                );
            }
        }
        let mut hash = PayloadSha256::new();
        for _ in 0..1000 {
            hash.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hash.finish(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn missing_truncated_and_changed_payloads_never_modify_current_files() {
        for damage in ["missing", "truncated", "same-size"] {
            let data = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            let store = CheckpointStore::new(data.path());
            let first = work.path().join("first.txt");
            let second = work.path().join("second.txt");
            fs::write(&first, "original").unwrap();
            fs::write(&second, "another!").unwrap();
            let id = store.create("run", "edit", &[&first, &second]).unwrap();
            fs::write(&first, "current first").unwrap();
            fs::write(&second, "current second").unwrap();
            let (dir, cp) = store.find(&id).unwrap();
            let payload = dir.join(cp.entries[1].backup.as_ref().unwrap());
            match damage {
                "missing" => fs::remove_file(payload).unwrap(),
                "truncated" => fs::write(payload, "short").unwrap(),
                _ => fs::write(payload, "tampered").unwrap(),
            }
            let err = store.restore(&id).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{damage}: {err}");
            assert!(
                err.to_string()
                    .contains(&format!("checkpoint {id} is incomplete")),
                "{err}"
            );
            assert_eq!(fs::read_to_string(&first).unwrap(), "current first");
            assert_eq!(fs::read_to_string(&second).unwrap(), "current second");
            assert!(
                store
                    .list("run")
                    .unwrap()
                    .iter()
                    .any(|cp| cp.op == format!("pre-restore:{id}"))
            );
        }
    }

    #[test]
    fn deleted_nested_payload_is_detected_before_directory_replacement() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let dir = work.path().join("tree");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("empty"), "").unwrap();
        fs::write(dir.join("file"), "before").unwrap();
        let id = store.create("r", "delete", &[&dir]).unwrap();
        fs::write(dir.join("file"), "current").unwrap();
        let (backup, _) = store.find(&id).unwrap();
        fs::remove_file(backup.join("files/0/empty")).unwrap();
        assert_eq!(
            store.restore(&id).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read_to_string(dir.join("file")).unwrap(), "current");
        assert!(dir.join("empty").exists());
    }

    #[test]
    fn later_staging_failure_rolls_back_already_swapped_files() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let first = work.path().join("first.txt");
        let second = work.path().join("second.txt");
        fs::write(&first, "old first").unwrap();
        fs::write(&second, "old second").unwrap();
        let id = store.create("run", "edit", &[&first, &second]).unwrap();
        fs::write(&first, "current first").unwrap();
        fs::write(&second, "current second").unwrap();
        let (backup, _) = store.find(&id).unwrap();
        let err = store
            .restore_internal(&id, |index| {
                if index == 1 {
                    assert_eq!(
                        fs::read_to_string(&first).unwrap(),
                        "old first",
                        "first swap must have occurred"
                    );
                    fs::remove_file(backup.join("files/1"))?;
                }
                Ok(())
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("current files rolled back"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(&first).unwrap(), "current first");
        assert_eq!(fs::read_to_string(&second).unwrap(), "current second");
        assert_eq!(
            fs::read_dir(work.path()).unwrap().count(),
            2,
            "staging files leaked"
        );
    }

    #[test]
    fn skipped_root_link_preserves_current_file_and_is_not_reported_restored() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let file = work.path().join("file");
        fs::write(&file, "original").unwrap();
        let id = store.create("r", "edit", &[&file]).unwrap();
        let (dir, _) = store.find(&id).unwrap();
        let link = LinkRecord {
            rel: "files/0".into(),
            target: r"\\192.0.2.1\share\refused".into(),
            dir: false,
        };
        fs::write(
            dir.join(LINKS_FILE),
            serde_json::to_vec(&vec![link]).unwrap(),
        )
        .unwrap();
        fs::write(&file, "current").unwrap();
        let (restored, _, warnings) = store.restore_with_warnings(&id).unwrap();
        assert_eq!(restored, [] as [String; 0]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("skipped"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "current");
    }

    #[test]
    fn old_manifest_without_integrity_fields_still_restores() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let file = work.path().join("old-format.txt");
        fs::write(&file, "original").unwrap();
        let id = store.create("run", "edit", &[&file]).unwrap();
        let (dir, _) = store.find(&id).unwrap();
        let manifest = dir.join("manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("skipped");
        json.as_object_mut().unwrap().remove("links_sha256");
        for entry in json["entries"].as_array_mut().unwrap() {
            for field in ["size", "sha256", "via_link_of"] {
                entry.as_object_mut().unwrap().remove(field);
            }
        }
        fs::write(&manifest, serde_json::to_vec(&json).unwrap()).unwrap();
        fs::write(&file, "changed").unwrap();
        store.restore(&id).unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), "original");
    }

    #[test]
    fn failed_placeholder_copy_records_skipped_path_and_removes_partial_payload() {
        let work = tempfile::tempdir().unwrap();
        let source = work.path().join("online-only.txt");
        let payload = work.path().join("payload");
        let mut budget = CopyBudget {
            entries: 0,
            bytes: 0,
            skipped: Vec::new(),
        };
        copy_placeholder(&source, &payload, 42, &mut budget, |_, destination| {
            fs::write(destination, "partial")?;
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "offline provider",
            ))
        })
        .unwrap();
        assert!(!payload.exists());
        assert_eq!(budget.skipped.len(), 1);
        assert!(budget.skipped[0].contains("online-only.txt"));
        assert!(budget.skipped[0].contains("offline provider"));
        copy_placeholder(
            &source,
            &payload,
            MAX_CHECKPOINT_BYTES + 1,
            &mut budget,
            |_, _| panic!("over-budget placeholder must not be hydrated"),
        )
        .unwrap();
        assert_eq!(budget.skipped.len(), 2);
        assert!(budget.skipped[1].contains("budget"));
    }

    #[test]
    fn checkpoint_marked_partial_is_not_restored() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let file = work.path().join("file");
        fs::write(&file, "original").unwrap();
        let id = store.create("r", "delete", &[&file]).unwrap();
        let (dir, mut cp) = store.find(&id).unwrap();
        cp.skipped.push("offline.dat: provider unavailable".into());
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&cp).unwrap()).unwrap();
        fs::write(&file, "current").unwrap();
        let err = store.restore(&id).unwrap_err();
        assert!(err.to_string().contains("offline.dat"), "{err}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "current");
        assert_eq!(store.list("r").unwrap()[0].skipped.len(), 1);
    }

    #[test]
    fn partial_fallback_copy_never_replaces_current_file() {
        for failure in ["error", "short", "corrupt"] {
            let work = tempfile::tempdir().unwrap();
            let at = work.path().join("link");
            let source = work.path().join("source");
            fs::write(&source, "expected").unwrap();
            fs::write(&at, "current bytes").unwrap();
            let outcome = restore_link_with_ops(
                &source,
                &at,
                false,
                |_, _, _| {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected symlink failure",
                    ))
                },
                |_, destination| {
                    assert_ne!(destination, at, "fallback must be staged");
                    fs::write(
                        destination,
                        if failure == "corrupt" {
                            "tampered"
                        } else {
                            "part"
                        },
                    )?;
                    match failure {
                        "error" => Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "injected copy failure",
                        )),
                        "short" => Ok(4),
                        _ => Ok(8),
                    }
                },
            );
            assert!(!outcome.applied, "{failure}");
            assert!(outcome.warning.unwrap().contains("skipped"), "{failure}");
            assert_eq!(
                fs::read_to_string(&at).unwrap(),
                "current bytes",
                "{failure}"
            );
            assert_eq!(
                fs::read_dir(work.path()).unwrap().count(),
                2,
                "partial stage leaked"
            );
        }
    }

    #[test]
    fn failed_fallback_copy_does_not_publish_or_count_partial_root_payload() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let current = work.path().join("file");
        let source = work.path().join("source");
        fs::write(&current, "before").unwrap();
        fs::write(&source, "referent bytes").unwrap();
        let id = store.create("r", "edit", &[&current]).unwrap();
        let (dir, _) = store.find(&id).unwrap();
        let link = LinkRecord {
            rel: "files/0".into(),
            target: source.display().to_string(),
            dir: false,
        };
        fs::write(
            dir.join(LINKS_FILE),
            serde_json::to_vec(&vec![link]).unwrap(),
        )
        .unwrap();
        fs::write(&current, "current bytes").unwrap();
        let (restored, _, warnings) = store
            .restore_using(
                &id,
                |_| Ok(()),
                |target, at, is_dir| {
                    restore_link_with_ops(
                        target,
                        at,
                        is_dir,
                        |_, _, _| {
                            Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "injected link failure",
                            ))
                        },
                        |_, destination| {
                            fs::write(destination, "partial")?;
                            Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "injected copy failure",
                            ))
                        },
                    )
                },
            )
            .unwrap();
        assert_eq!(restored, [] as [String; 0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(fs::read_to_string(&current).unwrap(), "current bytes");
        assert_eq!(fs::read_dir(work.path()).unwrap().count(), 2);
    }

    #[test]
    fn skipped_nested_link_never_deletes_an_existing_current_child() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let tree = work.path().join("tree");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("child"), "before").unwrap();
        let id = store.create("r", "delete", &[&tree]).unwrap();
        let (dir, _) = store.find(&id).unwrap();
        let link = LinkRecord {
            rel: "files/0/child".into(),
            target: r"\\192.0.2.1\share\refused".into(),
            dir: false,
        };
        fs::write(
            dir.join(LINKS_FILE),
            serde_json::to_vec(&vec![link]).unwrap(),
        )
        .unwrap();
        fs::write(tree.join("child"), "current child").unwrap();
        assert!(store.restore(&id).is_err());
        assert_eq!(
            fs::read_to_string(tree.join("child")).unwrap(),
            "current child"
        );
    }

    #[test]
    fn missing_recorded_link_metadata_is_detected_before_mutation() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let source = work.path().join("source");
        let link = work.path().join("link");
        fs::write(&source, "original").unwrap();
        if let Err(err) = make_link(&source, &link, false) {
            if cfg!(windows)
                && (err.kind() == io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(1314))
            {
                eprintln!("skipping: Windows symlink privilege unavailable: {err}");
                return;
            }
            panic!("link fixture: {err}");
        }
        let id = store.create("r", "delete", &[&link]).unwrap();
        let (dir, _) = store.find(&id).unwrap();
        fs::remove_file(dir.join(LINKS_FILE)).unwrap();
        let err = store.restore(&id).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("link metadata"), "{err}");
        assert!(
            err.to_string().contains("current state saved as cp-"),
            "{err}"
        );
        assert_eq!(fs::read_link(&link).unwrap(), source);
        assert_eq!(fs::read_to_string(source).unwrap(), "original");
    }

    #[test]
    fn injected_link_failure_copies_or_warns_without_claiming_a_missing_restore() {
        let work = tempfile::tempdir().unwrap();
        let at = work.path().join("link");
        fs::write(work.path().join("source"), "original").unwrap();
        let refused = |_: &Path, _: &Path, _: bool| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "no symlink privilege",
            ))
        };
        let warning = restore_link_with(Path::new("source"), &at, false, refused)
            .warning
            .unwrap();
        assert!(warning.contains("restored a copy"), "{warning}");
        assert_eq!(fs::read_to_string(&at).unwrap(), "original");
        fs::remove_file(&at).unwrap();
        let warning = restore_link_with(Path::new("missing"), &at, false, refused)
            .warning
            .unwrap();
        assert!(warning.contains("skipped"), "{warning}");
        assert!(fs::symlink_metadata(&at).is_err());
    }

    #[test]
    fn round_trip_file_and_created_file() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let existing = work.path().join("a.txt");
        let created = work.path().join("b.txt");
        fs::write(&existing, "original").unwrap();

        let id = store
            .create("run1", "fs_write", &[&existing, &created])
            .unwrap();
        fs::write(&existing, "changed").unwrap();
        fs::write(&created, "new").unwrap();

        let list = store.list("run1").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);

        let (restored, safety) = store.restore(&id).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(fs::read_to_string(&existing).unwrap(), "original");
        assert!(!created.exists());
        // The restore itself is undoable.
        store.restore(&safety).unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "changed");
        assert_eq!(fs::read_to_string(&created).unwrap(), "new");
    }

    #[test]
    fn directories_are_backed_up_recursively() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let d = work.path().join("d");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub").join("f"), "x").unwrap();
        let id = store.create("r", "fs_delete", &[&d]).unwrap();
        fs::remove_dir_all(&d).unwrap();
        store.restore(&id).unwrap();
        assert_eq!(fs::read_to_string(d.join("sub").join("f")).unwrap(), "x");
    }

    #[test]
    fn links_are_not_followed_and_are_restored_as_links() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("huge.bin"), "payload").unwrap();
        let store = CheckpointStore::new(data.path());
        let d = work.path().join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f"), "x").unwrap();
        if make_link(outside.path(), &d.join("ext"), true).is_err() {
            eprintln!("skipping: cannot create links here");
            return;
        }
        // A self-referential link must not recurse.
        let _ = make_link(&d, &d.join("loop"), true);

        let id = store.create("r", "fs_delete", &[&d]).unwrap();
        // The link target was not copied into the backup.
        let backup = store.list("r").unwrap()[0].entries[0]
            .backup
            .clone()
            .unwrap();
        let (cp_dir, _) = store.find(&id).unwrap();
        assert!(!cp_dir.join(&backup).join("ext").exists());

        fs::remove_dir_all(&d).unwrap();
        assert!(outside.path().join("huge.bin").exists());
        store.restore(&id).unwrap();
        assert_eq!(fs::read_to_string(d.join("f")).unwrap(), "x");
        let m = fs::symlink_metadata(d.join("ext")).unwrap();
        assert!(m.file_type().is_symlink(), "link restored as a link");
        assert_eq!(
            fs::read_to_string(d.join("ext").join("huge.bin")).unwrap(),
            "payload"
        );
        // Restoring over it again removes only the link, not the target.
        store.restore(&id).unwrap();
        assert!(outside.path().join("huge.bin").exists());
    }

    #[test]
    fn cmd_metacharacters_are_detected() {
        for bad in [
            r"C:\tmp\a&calc",
            "a|b",
            "a^b",
            "a<b",
            "a>b",
            "%PATH%",
            "a\"b",
            "a!b",
            "a\nb",
        ] {
            assert!(cmd_unsafe(bad), "{bad}");
        }
        assert!(!cmd_unsafe(
            r"C:\Users\me\My Project\node_modules\.pnpm\a-b@1.0"
        ));
    }

    #[test]
    fn failed_link_recreation_does_not_abort_restore() {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        let d = work.path().join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f"), "x").unwrap();
        // `d/bad` is a real non-empty directory in the backup, so it is restored
        // first and the link can then never be created at that path (no
        // symlink privilege or platform quirk involved), nor copied over it.
        fs::create_dir_all(d.join("bad")).unwrap();
        fs::write(d.join("bad").join("keep"), "k").unwrap();
        let id = store.create("r", "fs_delete", &[&d]).unwrap();
        let (cp_dir, _) = store.find(&id).unwrap();
        let backup = store.list("r").unwrap()[0].entries[0]
            .backup
            .clone()
            .unwrap();
        let links = vec![LinkRecord {
            rel: format!("{backup}/bad"),
            target: r"C:\tmp\a&calc\nope".into(),
            dir: true,
        }];
        fs::write(cp_dir.join(LINKS_FILE), serde_json::to_vec(&links).unwrap()).unwrap();
        fs::remove_dir_all(&d).unwrap();
        let (_, _, warnings) = store.restore_with_warnings(&id).unwrap();
        assert_eq!(fs::read_to_string(d.join("f")).unwrap(), "x");
        assert_eq!(fs::read_to_string(d.join("bad").join("keep")).unwrap(), "k");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("skipped"), "{warnings:?}");
    }

    #[test]
    fn link_fallback_copy_is_bounded_relative_and_skips_directories() {
        let w = tempfile::tempdir().unwrap();
        // `at` already exists, so creating the link fails and the fallback runs.
        let at = w.path().join("at");
        fs::write(&at, "old").unwrap();

        // Relative target resolves against the link's directory.
        fs::write(w.path().join("src.txt"), "new").unwrap();
        let msg = restore_link(Path::new("src.txt"), &at, false)
            .warning
            .unwrap();
        assert!(msg.contains("restored a copy"), "{msg}");
        assert_eq!(fs::read_to_string(&at).unwrap(), "new");

        // Oversized targets are skipped with a warning.
        let big = w.path().join("big.bin");
        fs::File::create(&big)
            .unwrap()
            .set_len(MAX_LINK_COPY_BYTES + 1)
            .unwrap();
        fs::write(&at, "old").unwrap();
        let msg = restore_link(&big, &at, false).warning.unwrap();
        assert!(msg.contains("skipped") && msg.contains("larger"), "{msg}");
        assert_eq!(fs::read_to_string(&at).unwrap(), "old");

        // Directories are not copied.
        let dir = w.path().join("dir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f"), "x").unwrap();
        let msg = restore_link(&dir, &at, true).warning.unwrap();
        assert!(
            msg.contains("skipped") && msg.contains("directory"),
            "{msg}"
        );
        assert!(!at.join("f").exists());
    }

    #[test]
    fn bad_ids_are_rejected() {
        let data = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(data.path());
        assert!(store.restore("../x").is_err());
        assert!(store.restore("").is_err());
    }

    fn assert_no_partial(data: &Path) {
        let root = data.join("checkpoints");
        if !root.exists() {
            return;
        }
        for run in fs::read_dir(&root).unwrap() {
            let run = run.unwrap().path();
            if !run.is_dir() {
                continue;
            }
            if let Some(id) = fs::read_dir(&run).unwrap().next() {
                panic!(
                    "partial checkpoint left at {}",
                    id.unwrap().path().display()
                );
            }
        }
    }

    #[test]
    fn checkpoint_of_data_dir_and_checkpoints_dir_leaves_no_partial_backup() {
        let data = tempfile::tempdir().unwrap();
        fs::write(data.path().join("note.txt"), "keep").unwrap();
        fs::create_dir_all(data.path().join("checkpoints")).unwrap();
        fs::write(data.path().join("checkpoints").join("seed.txt"), "seed").unwrap();
        let store = CheckpointStore::new(data.path());

        let err = store
            .create("run", "fs_delete", &[data.path()])
            .unwrap_err();
        assert!(err.to_string().contains("ancestor"), "{err}");
        assert_no_partial(data.path());
        assert_eq!(
            fs::read_to_string(data.path().join("note.txt")).unwrap(),
            "keep"
        );

        let checkpoints = data.path().join("checkpoints");
        let err = store
            .create("run", "fs_delete", &[&checkpoints])
            .unwrap_err();
        assert!(err.to_string().contains("ancestor"), "{err}");
        assert_no_partial(data.path());
        assert_eq!(
            fs::read_to_string(checkpoints.join("seed.txt")).unwrap(),
            "seed"
        );

        let work = tempfile::tempdir().unwrap();
        let mut deep = work.path().to_path_buf();
        for i in 0..=MAX_CHECKPOINT_DEPTH {
            deep.push(i.to_string());
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("f.txt"), "z").unwrap();
        let err = store
            .create("run", "fs_delete", &[work.path()])
            .unwrap_err();
        assert!(err.to_string().contains("depth"), "{err}");
        assert_no_partial(data.path());

        fs::write(work.path().join("a.txt"), "a").unwrap();
        let id = store
            .create("run", "fs_write", &[work.path().join("a.txt").as_path()])
            .unwrap();
        assert_eq!(store.list("run").unwrap()[0].id, id);
    }
}
