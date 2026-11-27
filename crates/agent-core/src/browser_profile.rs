//! Whether a bot's persistent managed-browser profile holds state of the OWNER.
//!
//! The per-bot default profile (`<data>/browsers/<bot-id>`) keeps its cookies. Sites the bot
//! signed up to itself (`account_register`) are the bot's own. But once the owner logged in
//! there by hand (a live-view takeover) or a secret the owner stored was typed there, what a
//! page shows may be the owner's mail or bank. A marker file next to the profile records that;
//! while it exists every browser result of that bot counts as PRIVATE and is hashed, so only a
//! copy of its words leaving for a new site asks and normal browsing stays free.

use std::path::{Path, PathBuf};

use secret_store::FillSecrets;

/// File name of the marker, inside `<data>/browsers/<bot-id>/`.
const MARKER: &str = "praxi-owner-state";

fn marker(data_dir: &Path, bot: &str) -> Option<PathBuf> {
    (!bot.is_empty()
        && bot
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| data_dir.join("browsers").join(bot).join(MARKER))
}

/// The bot's browser profile holds state of the owner.
#[must_use]
pub fn is_private(data_dir: &Path, bot: &str) -> bool {
    marker(data_dir, bot).is_some_and(|p| p.exists())
}

/// Record that the owner's accounts may be signed in to the bot's browser profile (a takeover
/// of its browser, a secret of the owner typed there). Never cleared by the daemon.
///
/// # Errors
/// The marker cannot be written.
pub fn mark_private(data_dir: &Path, bot: &str) -> std::io::Result<()> {
    let Some(path) = marker(data_dir, bot) else {
        return Err(std::io::Error::other("invalid bot id"));
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, b"1")
}

/// Typing the secret `name` is the OWNER's login (a secret the owner stored), not a login the
/// bot registered for itself with `account_register`. An unknown name counts as the owner's.
#[must_use]
pub(crate) fn is_owner_secret(secrets: &FillSecrets, bot: &str, name: &str) -> bool {
    // `totp:<account>` types the two-factor code of a bot-made account.
    let name = secret_store::totp_reference(name).unwrap_or(name);
    !secrets
        .accounts(Some(bot))
        .unwrap_or_default()
        .iter()
        .any(|a| a.name == name && a.created_by == "bot")
}

#[cfg(test)]
mod tests {
    use super::*;
    use secret_store::{MemoryStore, NewAccount};
    use std::sync::Arc;

    #[test]
    fn the_marker_is_per_bot_and_refuses_odd_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_private(dir.path(), "bot-1"));
        mark_private(dir.path(), "bot-1").unwrap();
        assert!(is_private(dir.path(), "bot-1"));
        assert!(!is_private(dir.path(), "bot-2"));
        assert!(mark_private(dir.path(), "../x").is_err());
        assert!(!is_private(dir.path(), "../x"));
    }

    #[test]
    fn a_login_the_bot_registered_itself_is_not_owner_state() {
        let secrets = FillSecrets::in_memory(Arc::new(MemoryStore::new()));
        secrets
            .register_account(&NewAccount {
                name: "login/example.com-1",
                site: "https://example.com",
                login_id: "me@example.org",
                email_account: None,
                bot_id: "bot-1",
                note: None,
                password_len: 16,
                symbols: false,
            })
            .unwrap();
        assert!(!is_owner_secret(&secrets, "bot-1", "login/example.com-1"));
        // an owner-stored secret, or a name nobody knows
        secrets
            .upsert("login/owner-made", Some("x"), None, None)
            .unwrap();
        assert!(is_owner_secret(&secrets, "bot-1", "login/owner-made"));
        assert!(is_owner_secret(&secrets, "bot-1", "login/unknown"));
        // A two-factor code is judged by the account it belongs to.
        assert!(!is_owner_secret(
            &secrets,
            "bot-1",
            "totp:login/example.com-1"
        ));
        assert!(is_owner_secret(&secrets, "bot-1", "totp:login/owner-made"));
    }
}
