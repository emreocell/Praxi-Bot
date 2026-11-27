//! Directory access through file descriptors, for root code that works inside a directory another
//! user can write.
//!
//! A path is only a name: between checking it and using it, whoever owns the directory can swap a
//! component for a link, and root then reads, writes or removes wherever the link points. A [`Dir`]
//! is opened once (never through a link) and everything after that happens relative to its
//! descriptor, so the kernel resolves each name below it and nothing can lead out of the tree. A link
//! anywhere on the way is an error, never a detour.
use rustix::fs::{AtFlags, FileType, Mode, OFlags, ResolveFlags, Stat};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::{Component, Path},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Cleared once the kernel (or a seccomp filter) turned `openat2` down: the walk below does the same.
static OPENAT2: AtomicBool = AtomicBool::new(true);
static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// How deep `remove_tree` goes: the trees it removes are flat or two levels, never this.
const MAX_DEPTH: usize = 8;

pub(super) struct Dir {
    fd: OwnedFd,
}

impl AsFd for Dir {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

/// A single plain file name: not empty, not `.` or `..`, no separator.
fn plain(name: &OsStr) -> io::Result<&OsStr> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(invalid("not a plain file name"));
    }
    Ok(name)
}

/// The error a link (or anything else that is not a plain directory) causes where a real directory
/// is required, so that callers can word it as a refusal.
pub(super) fn is_refusal(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENOTDIR | libc::EXDEV | libc::EMLINK)
    )
}

pub(super) fn is_dir(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Directory
}

pub(super) fn is_symlink(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Symlink
}

pub(super) fn is_regular_file(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
}

impl Dir {
    /// Open the directory at `path`. The last component must not be a link; the caller vouches for
    /// the ones above it (root-owned and not writable by others in production).
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self { fd })
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            fd: self.fd.try_clone()?,
        })
    }

    /// The directory itself, as the descriptor sees it.
    pub(super) fn stat_self(&self) -> io::Result<Stat> {
        Ok(rustix::fs::fstat(&self.fd)?)
    }

    /// `name` below this directory, without following a link (a link is a stat of the link).
    pub(super) fn stat(&self, name: &OsStr) -> io::Result<Stat> {
        Ok(rustix::fs::statat(
            &self.fd,
            plain(name)?,
            AtFlags::SYMLINK_NOFOLLOW,
        )?)
    }

    pub(super) fn exists(&self, name: &OsStr) -> io::Result<bool> {
        match self.stat(name) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// A directory below this one, reached through real directories only.
    pub(super) fn sub(&self, relative: &Path) -> io::Result<Self> {
        let fd = self.open_beneath(relative, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
        Ok(Self { fd })
    }

    /// `relative` (plain names only) below this directory, with no link on the way, not even as the
    /// last component. Where the kernel has `openat2` that is one atomic call
    /// (`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`); otherwise every component is opened on its own with
    /// `O_NOFOLLOW` and checked with `fstat`.
    pub(super) fn open_beneath(
        &self,
        relative: &Path,
        flags: OFlags,
        mode: Mode,
    ) -> io::Result<OwnedFd> {
        self.open_beneath_with(relative, flags, mode, OPENAT2.load(Ordering::Relaxed))
    }

    fn open_beneath_with(
        &self,
        relative: &Path,
        flags: OFlags,
        mode: Mode,
        use_openat2: bool,
    ) -> io::Result<OwnedFd> {
        let mut names = Vec::new();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(invalid("a database path holds plain names only"));
            };
            names.push(plain(name)?);
        }
        if names.is_empty() {
            return Err(invalid("empty path"));
        }
        let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if use_openat2 {
            match rustix::fs::openat2(
                &self.fd,
                relative,
                flags,
                mode,
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
            ) {
                Ok(fd) => return Ok(fd),
                // Too old a kernel, or a seccomp filter that does not know the call.
                Err(rustix::io::Errno::NOSYS | rustix::io::Errno::PERM) => {
                    OPENAT2.store(false, Ordering::Relaxed);
                }
                Err(e) => return Err(e.into()),
            }
        }
        let (last, parents) = names.split_last().ok_or_else(|| invalid("empty path"))?;
        let mut current = self.fd.try_clone()?;
        for name in parents {
            let next = rustix::fs::openat(
                &current,
                *name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            if !is_dir(&rustix::fs::fstat(&next)?) {
                return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
            }
            current = next;
        }
        Ok(rustix::fs::openat(&current, *last, flags, mode)?)
    }

    /// Open an existing file for reading. What it is must be checked with `File::metadata` (an
    /// `fstat` of this descriptor), never by path: a pipe swapped in neither blocks the open nor
    /// passes for a file.
    pub(super) fn open_read(&self, relative: &Path) -> io::Result<File> {
        self.open_read_with(relative, OPENAT2.load(Ordering::Relaxed))
    }

    fn open_read_with(&self, relative: &Path, use_openat2: bool) -> io::Result<File> {
        let fd = self.open_beneath_with(
            relative,
            OFlags::RDONLY | OFlags::NONBLOCK,
            Mode::empty(),
            use_openat2,
        )?;
        Ok(File::from(fd))
    }

    /// Create a new file (`O_EXCL`: it must not exist, and a link of that name is never followed).
    pub(super) fn create_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        let fd = rustix::fs::openat(
            &self.fd,
            plain(name)?,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode),
        )?;
        Ok(File::from(fd))
    }

    pub(super) fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        Ok(rustix::fs::mkdirat(
            &self.fd,
            plain(name)?,
            Mode::from_raw_mode(mode),
        )?)
    }

    /// A new directory named `<prefix><pid>-<n>`, opened. It stays where it was created.
    pub(super) fn mkdir_unique(&self, prefix: &str, mode: u32) -> io::Result<(OsString, Self)> {
        for _ in 0..100 {
            let name = OsString::from(format!(
                "{prefix}{}-{}",
                std::process::id(),
                UNIQUE.fetch_add(1, Ordering::Relaxed)
            ));
            match self.mkdir(&name, mode) {
                Ok(()) => {
                    let dir = self.sub(Path::new(&name))?;
                    return Ok((name, dir));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::from(io::ErrorKind::AlreadyExists))
    }

    /// Remove a name (never what a link points to). Absent is fine.
    pub(super) fn unlink_if_exists(&self, name: &OsStr) -> io::Result<()> {
        match rustix::fs::unlinkat(&self.fd, plain(name)?, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Move `from` (in this directory) to `to` in `target`; an existing `to` is replaced as a whole.
    pub(super) fn rename(&self, from: &OsStr, target: &Self, to: &OsStr) -> io::Result<()> {
        Ok(rustix::fs::renameat(
            &self.fd,
            plain(from)?,
            &target.fd,
            plain(to)?,
        )?)
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        Ok(rustix::fs::fsync(&self.fd)?)
    }

    /// The names in this directory (not `.` and `..`), in no particular order.
    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        let mut dir = rustix::fs::Dir::read_from(&self.fd)?;
        let mut names = Vec::new();
        while let Some(entry) = dir.read() {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
        Ok(names)
    }

    /// Remove `name` and everything below it. Links are removed, never followed; a name that is
    /// absent is fine.
    pub(super) fn remove_tree(&self, name: &OsStr) -> io::Result<()> {
        self.remove_tree_at(name, 0)
    }

    fn remove_tree_at(&self, name: &OsStr, depth: usize) -> io::Result<()> {
        let stat = match self.stat(name) {
            Ok(stat) => stat,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if !is_dir(&stat) {
            return self.unlink_if_exists(name);
        }
        if depth >= MAX_DEPTH {
            return Err(invalid("directory tree is too deep to remove"));
        }
        let child = self.sub(Path::new(name))?;
        for entry in child.names()? {
            child.remove_tree_at(&entry, depth + 1)?;
        }
        drop(child);
        Ok(rustix::fs::unlinkat(
            &self.fd,
            plain(name)?,
            AtFlags::REMOVEDIR,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read, os::unix::fs::symlink};

    fn tree() -> (tempfile::TempDir, tempfile::TempDir) {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(inside.path().join("bots/b1")).unwrap();
        fs::write(inside.path().join("bots/b1/k.db"), "inside").unwrap();
        fs::write(outside.path().join("secret"), "outside").unwrap();
        (inside, outside)
    }

    fn read(dir: &Dir, path: &str, use_openat2: bool) -> io::Result<String> {
        let mut file = dir.open_read_with(Path::new(path), use_openat2)?;
        assert!(file.metadata()?.is_file());
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    /// Both ways of resolving a name below a directory must agree: the single `openat2` call and the
    /// component walk that older kernels need.
    const WAYS: [bool; 2] = [true, false];

    #[test]
    fn names_below_the_directory_open_and_nothing_climbs_out() {
        let (inside, outside) = tree();
        let dir = Dir::open(inside.path()).unwrap();
        for way in WAYS {
            assert_eq!(read(&dir, "bots/b1/k.db", way).unwrap(), "inside");
            for odd in [
                "",
                ".",
                "..",
                "../x",
                "bots/../../x",
                "/etc/passwd",
                "bots//b1",
            ] {
                // `bots//b1` is two names to `Path`, so it is the one that may legitimately work.
                if odd == "bots//b1" {
                    continue;
                }
                assert!(read(&dir, odd, way).is_err(), "{odd:?} ({way})");
            }
        }
        drop(outside);
    }

    #[test]
    fn a_link_is_refused_at_every_position() {
        for way in WAYS {
            let (inside, outside) = tree();
            symlink(outside.path(), inside.path().join("dirlink")).unwrap();
            symlink(
                outside.path().join("secret"),
                inside.path().join("bots/b1/filelink.db"),
            )
            .unwrap();
            symlink(outside.path(), inside.path().join("bots/b2")).unwrap();
            let dir = Dir::open(inside.path()).unwrap();
            // The last component, a middle one and the first one.
            for path in ["bots/b1/filelink.db", "bots/b2/secret", "dirlink/secret"] {
                let error = read(&dir, path, way).unwrap_err();
                assert!(is_refusal(&error), "{path} ({way}): {error}");
            }
            // And as directories to enter.
            for path in ["dirlink", "bots/b2"] {
                let error = dir
                    .open_beneath_with(
                        Path::new(path),
                        OFlags::RDONLY | OFlags::DIRECTORY,
                        Mode::empty(),
                        way,
                    )
                    .unwrap_err();
                assert!(is_refusal(&error), "{path} ({way}): {error}");
            }
            // `stat` reports the link itself.
            assert!(is_symlink(&dir.stat(OsStr::new("dirlink")).unwrap()));
            assert_eq!(
                fs::read_to_string(outside.path().join("secret")).unwrap(),
                "outside"
            );
        }
    }

    #[test]
    fn the_root_of_a_directory_must_not_be_a_link() {
        let (inside, outside) = tree();
        symlink(outside.path(), inside.path().join("root-link")).unwrap();
        let error = Dir::open(&inside.path().join("root-link"))
            .err()
            .expect("a link is no directory to open");
        assert!(is_refusal(&error), "{error}");
    }

    #[test]
    fn a_pipe_in_place_of_a_file_neither_blocks_nor_looks_like_a_file() {
        let (inside, _outside) = tree();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            inside.path().join("pipe.db"),
            FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let dir = Dir::open(inside.path()).unwrap();
        for way in WAYS {
            let file = dir.open_read_with(Path::new("pipe.db"), way).unwrap();
            assert!(!file.metadata().unwrap().is_file());
        }
    }

    #[test]
    fn plain_names_only_for_everything_that_takes_a_single_name() {
        let (inside, _outside) = tree();
        let dir = Dir::open(inside.path()).unwrap();
        for bad in ["", ".", "..", "a/b", "../a"] {
            let name = OsStr::new(bad);
            assert!(dir.create_file(name, 0o600).is_err(), "{bad:?}");
            assert!(dir.mkdir(name, 0o700).is_err(), "{bad:?}");
            assert!(dir.unlink_if_exists(name).is_err(), "{bad:?}");
            assert!(dir.stat(name).is_err(), "{bad:?}");
            assert!(dir.remove_tree(name).is_err(), "{bad:?}");
            assert!(dir.rename(name, &dir, OsStr::new("x")).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn creating_never_follows_a_link_already_in_the_way() {
        let (inside, outside) = tree();
        symlink(outside.path().join("secret"), inside.path().join("taken")).unwrap();
        symlink(outside.path().join("fresh"), inside.path().join("dangling")).unwrap();
        let dir = Dir::open(inside.path()).unwrap();
        for name in ["taken", "dangling"] {
            let error = dir.create_file(OsStr::new(name), 0o600).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{name}");
        }
        assert_eq!(
            fs::read_to_string(outside.path().join("secret")).unwrap(),
            "outside"
        );
        assert!(!outside.path().join("fresh").exists());
        // Removing the name removes the link, not its target.
        dir.unlink_if_exists(OsStr::new("taken")).unwrap();
        assert!(outside.path().join("secret").exists());
        // And a replaced file is replaced as a whole.
        fs::write(inside.path().join("a"), "a").unwrap();
        fs::write(inside.path().join("b"), "b").unwrap();
        dir.rename(OsStr::new("a"), &dir, OsStr::new("b")).unwrap();
        assert_eq!(fs::read_to_string(inside.path().join("b")).unwrap(), "a");
    }

    #[test]
    fn removing_a_tree_removes_links_and_leaves_what_they_point_to() {
        let (inside, outside) = tree();
        symlink(outside.path(), inside.path().join("bots/b1/out")).unwrap();
        symlink(
            outside.path().join("secret"),
            inside.path().join("bots/file"),
        )
        .unwrap();
        let dir = Dir::open(inside.path()).unwrap();
        dir.remove_tree(OsStr::new("bots")).unwrap();
        assert!(!inside.path().join("bots").exists());
        assert_eq!(
            fs::read_to_string(outside.path().join("secret")).unwrap(),
            "outside"
        );
        // Already gone: fine. A link as the top: the link goes, the target stays.
        dir.remove_tree(OsStr::new("bots")).unwrap();
        symlink(outside.path(), inside.path().join("top")).unwrap();
        dir.remove_tree(OsStr::new("top")).unwrap();
        assert!(!inside.path().join("top").exists() && outside.path().join("secret").exists());
    }

    #[test]
    fn unique_directories_do_not_collide_and_names_are_listed() {
        let (inside, _outside) = tree();
        let dir = Dir::open(inside.path()).unwrap();
        let (first, _) = dir.mkdir_unique(".stage-", 0o700).unwrap();
        let (second, _) = dir.mkdir_unique(".stage-", 0o700).unwrap();
        assert_ne!(first, second);
        let mut names = dir.names().unwrap();
        names.sort();
        assert_eq!(names.len(), 3);
        assert!(names.iter().any(|n| n == "bots"));
    }

    #[test]
    fn the_walk_without_openat2_still_refuses_what_openat2_refuses() {
        // A deep link is the case a per-component walk could get wrong.
        let (inside, outside) = tree();
        fs::remove_dir_all(inside.path().join("bots/b1")).unwrap();
        symlink(outside.path(), inside.path().join("bots/b1")).unwrap();
        let dir = Dir::open(inside.path()).unwrap();
        for way in WAYS {
            assert!(read(&dir, "bots/b1/secret", way).is_err(), "{way}");
        }
    }
}
