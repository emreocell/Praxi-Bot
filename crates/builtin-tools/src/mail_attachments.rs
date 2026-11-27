//! Where `email_download_attachment` saves for a bot: its workspace, under the bot's
//! write quota, with every saved file recorded as coming from a stranger.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use connector_email::AttachmentPlace;

use crate::FsAccessProvider;
use crate::provenance::{self, Provenance};

/// The bot's workspace (`FsAccessProvider::bot_workspace`) as the attachment place.
pub(crate) struct WorkspaceAttachments {
    pub(crate) fs: Arc<dyn FsAccessProvider>,
    pub(crate) ledger: Option<Arc<dyn Provenance>>,
}

impl AttachmentPlace for WorkspaceAttachments {
    fn folder_for(&self, bot: Option<&str>) -> Option<PathBuf> {
        self.fs.bot_workspace(bot)
    }

    fn allow_write(&self, bot: Option<&str>, path: &Path, bytes: u64) -> Result<(), String> {
        self.fs.check_write(bot, path, bytes)
    }

    fn saved(&self, path: &Path) {
        // What a stranger sent is untrusted whatever folder it lies in: a later read of
        // the file is judged by this mark, not by the workspace it sits in.
        if let Some(ledger) = &self.ledger {
            provenance::mark_path(ledger.as_ref(), path, false, "email attachment");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FsScope;
    use std::sync::Mutex;

    struct Fs {
        ws: PathBuf,
        refuse: bool,
    }

    impl FsAccessProvider for Fs {
        fn scope(&self, _bot: Option<&str>) -> FsScope {
            FsScope::Computer
        }
        fn bot_workspace(&self, bot: Option<&str>) -> Option<PathBuf> {
            bot.map(|_| self.ws.clone())
        }
        fn check_write(&self, _: Option<&str>, _: &Path, _: u64) -> Result<(), String> {
            if self.refuse {
                Err("quota".into())
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct Ledger(Mutex<Vec<String>>);

    impl Provenance for Ledger {
        fn is_marked(&self, n: &str) -> bool {
            self.0.lock().unwrap().iter().any(|p| p == n)
        }
        fn mark(&self, n: &str, _tree: bool, _origin: &str) {
            self.0.lock().unwrap().push(n.to_owned());
        }
    }

    #[test]
    fn the_workspace_is_the_place_the_quota_applies_and_the_file_is_marked_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("downloads").join("a.pdf");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let ledger = Arc::new(Ledger::default());
        let place = WorkspaceAttachments {
            fs: Arc::new(Fs {
                ws: dir.path().to_path_buf(),
                refuse: false,
            }),
            ledger: Some(ledger.clone()),
        };
        assert_eq!(
            place.folder_for(Some("bot")),
            Some(dir.path().to_path_buf())
        );
        assert_eq!(
            place.folder_for(None),
            None,
            "not a bot: the daemon's folder"
        );
        assert!(place.allow_write(Some("bot"), &file, 1).is_ok());
        place.saved(&file);
        assert!(ledger.is_marked(&provenance::normalize(&file)));
        assert!(!ledger.is_marked(&provenance::normalize(dir.path())));

        let full = WorkspaceAttachments {
            fs: Arc::new(Fs {
                ws: dir.path().to_path_buf(),
                refuse: true,
            }),
            ledger: None,
        };
        assert_eq!(full.allow_write(Some("bot"), &file, 1), Err("quota".into()));
    }
}
