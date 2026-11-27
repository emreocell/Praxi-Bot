//! What the embedder decides for the git tools.
//!
//! The tools know only their allowed roots. The daemon also knows the credential and
//! daemon-data denylist, other bots' workspaces and the write budget (the `fs_*` rules), so it
//! hands the tools a [`GitPolicy`]. Without one the tools keep the plain root check.

use std::fmt;
use std::path::Path;
use std::path::PathBuf;

/// Rules from the embedder: where git may run, which files stay hidden, and what a change
/// has to go through.
pub trait GitPolicy: Send + Sync + fmt::Debug {
    /// May git work in `dir`? Called for the repository's top level, its git directory and
    /// its common git directory (a `.git` file or `core.worktree` can point anywhere), with
    /// the paths git reported, made absolute. `mutates` is set for a commit or a branch change.
    ///
    /// # Errors
    /// The reason, shown to the model.
    fn check_dir(&self, dir: &Path, mutates: bool) -> Result<(), String>;

    /// Is `path` (absolute, inside a work tree, maybe deleted) a file the bot may not read or
    /// change: a credential, a tool settings folder, another bot's workspace? Decided on the
    /// name alone, so a file that no longer exists can be judged too.
    fn is_protected(&self, path: &Path) -> bool;

    /// A change in the work tree `repo` is about to run. `control_files` are the git files it
    /// rewrites (index, HEAD, the current branch ref): the embedder takes an undo checkpoint of
    /// them. `added_bytes` is an upper bound of what the change adds to the folder, for the
    /// write budget.
    ///
    /// # Errors
    /// The change must not run (over budget, a checkpoint that cannot be taken).
    fn begin_mutation(
        &self,
        op: &'static str,
        repo: &Path,
        control_files: &[PathBuf],
        added_bytes: u64,
    ) -> Result<Box<dyn MutationScope>, String>;
}

/// Ends the bookkeeping that [`GitPolicy::begin_mutation`] started.
pub trait MutationScope: Send {
    /// The change ended. `work_tree` is set when it rewrote files in that work tree (a branch
    /// switch); the embedder marks them in its provenance ledger when the run holds untrusted
    /// content. Returns what to tell the model (the checkpoint id) when there is something.
    ///
    /// # Errors
    /// The after-state could not be recorded.
    fn finish(
        self: Box<Self>,
        ok: bool,
        work_tree: Option<&Path>,
    ) -> Result<Option<String>, String>;
}
