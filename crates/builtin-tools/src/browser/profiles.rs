//! Housekeeping for the owner, deliberately absent from the model's tool list.

use std::sync::Arc;

use browser_cdp::{Browser, LaunchOptions, saved_sites as profile};

use super::{CdpDriver, Shared, close_session, s};

/// A handle to the same per-bot pool used by browser tools.
#[derive(Default)]
pub struct BrowserProfiles {
    pub(super) root: std::sync::OnceLock<Arc<Shared>>,
}

/// No paths, cookie values or passwords cross the API boundary.
pub struct ProfileStatus {
    /// Domains with saved cookies; not a claim that these sessions are valid.
    pub domains: Vec<String>,
    /// False when Chromium has locked the cookie database or it cannot be read.
    pub sites_available: bool,
    /// Total bytes in this bot's profile, including named profiles.
    pub size_bytes: u64,
    /// Browser tools refuse calls until the owner ends manual sign-in.
    pub signing_in: bool,
    /// Windows EFS, when supported. False does not claim lack of disk encryption.
    pub directory_encrypted: bool,
}

impl BrowserProfiles {
    /// Conservative policy signal, also after a restart. A model's omitted
    /// `profile` argument must never turn a saved account into anonymous browsing.
    ///
    /// True once the owner used the manual sign-in window for this bot (a marker the daemon
    /// writes before launching it; it goes away with "forget all"). Cookies alone do not count:
    /// the persistent profile has a cookie file after the very first browse, and ordinary site
    /// cookies would switch the browsing lanes off for every bot. An invalid bot id is true.
    #[must_use]
    pub fn may_be_signed_in(&self, bot: &str) -> bool {
        let Some(root) = self.root.get().and_then(|r| r.managed.as_ref()) else {
            return false;
        };
        if bot.is_empty()
            || !bot
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return true;
        }
        // Attached to the owner's own browser: everything it shows may be a signed-in session.
        if root
            .peers
            .lock()
            .get(bot)
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|d| d.is_attached())
        {
            return true;
        }
        root.settings
            .profiles_root
            .join(bot)
            .join(".owner-sign-in")
            .exists()
    }
    fn driver(&self, bot: &str) -> Result<Arc<CdpDriver>, String> {
        let root = self.root.get().ok_or("managed browser is unavailable")?;
        let _ = root.for_bot(Some(bot)).map_err(|e| e.to_string())?;
        root.managed
            .as_ref()
            .and_then(|m| m.peers.lock().get(bot).and_then(std::sync::Weak::upgrade))
            .ok_or_else(|| "managed browser is unavailable".into())
    }

    /// Read metadata without starting a browser or connecting to a website.
    ///
    /// # Errors
    /// Returns an error for an unavailable pool, invalid bot identity or unreadable profile.
    pub async fn status(&self, bot: &str) -> Result<ProfileStatus, String> {
        let driver = self.driver(bot)?;
        let _guard = driver.session.lock().await;
        let path = driver
            .settings
            .profile_dir
            .clone()
            .ok_or("missing bot profile")?;
        let mut manual = driver.manual.lock().await;
        if manual.as_ref().is_some_and(Browser::is_closed)
            && let Some(browser) = manual.take()
        {
            browser.close().await.map_err(s)?;
        }
        let signing_in = manual.is_some();
        tokio::task::spawn_blocking(move || {
            let size_bytes = profile::size(&path).map_err(s)?;
            let domains = profile::domains(&path);
            Ok(ProfileStatus {
                sites_available: domains.is_ok(),
                domains: domains.unwrap_or_default(),
                size_bytes,
                signing_in,
                directory_encrypted: profile::directory_encrypted(&path),
            })
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// A headed window without a Page, snapshots, keyboard hooks or a live view.
    /// The owner navigates and types directly into Chromium. Never accept a URL
    /// or credentials from this route, and disable Chromium password saving.
    ///
    /// # Errors
    /// Returns an error when no display is available or the profile/browser cannot be opened.
    pub async fn sign_in(&self, bot: &str) -> Result<(), String> {
        if !browser_cdp::host::has_display() {
            return Err(
                "manual sign-in needs a desktop display on the computer running this bot".into(),
            );
        }
        let driver = self.driver(bot)?;
        let mut guard = driver.session.lock().await;
        let mut manual = driver.manual.lock().await;
        if manual.as_ref().is_some_and(|b| !b.is_closed()) {
            return Ok(());
        }
        if let Some(browser) = manual.take() {
            browser.close().await.map_err(s)?;
        }
        if let Some(session) = guard.take() {
            close_session(driver.view.as_deref(), session).await;
        }
        let path = driver
            .settings
            .profile_dir
            .clone()
            .ok_or("missing bot profile")?;
        let checked = path.clone();
        tokio::task::spawn_blocking(move || profile::prepare(&checked).map_err(s))
            .await
            .map_err(|e| e.to_string())??;
        tokio::fs::write(path.join(".owner-sign-in"), b"")
            .await
            .map_err(|e| e.to_string())?;
        *manual = Some(
            Browser::launch(LaunchOptions {
                executable: driver.settings.executable.clone(),
                headless: false,
                user_data_dir: Some(path),
                ..LaunchOptions::default()
            })
            .await
            .map_err(s)?,
        );
        Ok(())
    }

    /// Close the private window before allowing automation to resume.
    ///
    /// # Errors
    /// Returns an error if the bot's driver is unavailable or its window cannot be closed.
    pub async fn finish_sign_in(&self, bot: &str) -> Result<(), String> {
        let driver = self.driver(bot)?;
        let _guard = driver.session.lock().await;
        if let Some(browser) = driver.manual.lock().await.take() {
            browser.close().await.map_err(s)?;
        }
        Ok(())
    }

    /// Stop the bot's browser, then forget an exact site (and saved subdomains)
    /// or all profile data. No running page may write cookies back afterwards.
    ///
    /// # Errors
    /// Refuses invalid sites and active manual sign-in, and reports browser/filesystem failures.
    pub async fn forget(&self, bot: &str, domain: Option<&str>) -> Result<(), String> {
        if domain.is_some_and(|d| !profile::valid_domain(d)) {
            return Err("choose a site from the saved sites list".into());
        }
        let driver = self.driver(bot)?;
        let mut guard = driver.session.lock().await;
        if driver.manual.lock().await.is_some() {
            return Err("finish manual sign-in before forgetting saved sites".into());
        }
        if let Some(session) = guard.take() {
            close_session(driver.view.as_deref(), session).await;
        }
        let path = driver
            .settings
            .profile_dir
            .clone()
            .ok_or("missing bot profile")?;
        let saved_path = path.clone();
        let read_domains = domain.is_some();
        let saved = tokio::task::spawn_blocking(move || {
            profile::size(&saved_path).map_err(s)?;
            if read_domains {
                profile::domains(&saved_path).map_err(s)
            } else {
                Ok(Vec::new())
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        if let Some(domain) = domain {
            if !saved.iter().any(|d| d == domain) {
                return Err("that site has no saved cookies; refresh the list".into());
            }
            let browser = Browser::launch(LaunchOptions {
                executable: driver.settings.executable.clone(),
                user_data_dir: Some(path),
                ..LaunchOptions::default()
            })
            .await
            .map_err(s)?;
            let result = browser.forget_site(domain, &saved).await.map_err(s);
            let closed = browser.close().await.map_err(s);
            result?;
            closed?;
        } else if path.exists() {
            tokio::fs::remove_dir_all(path)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{BrowserDriver, BrowserSettings, managed_tools_with_profiles};

    #[tokio::test]
    async fn same_pool_persists_modes_and_forgets_only_the_selected_bot_even_with_corrupt_cookies()
    {
        let dir = tempfile::tempdir().unwrap();
        let profiles = Arc::new(BrowserProfiles::default());
        let _tools = managed_tools_with_profiles(
            BrowserSettings::isolated(dir.path()),
            dir.path(),
            None,
            None,
            None,
            None,
            Some(profiles.clone()),
        );
        let a = profiles.driver("bot-a").unwrap();
        let b = profiles.driver("bot-b").unwrap();
        a.for_headless(false).unwrap();
        let alternate = a.alternate_mode.lock().clone().unwrap();
        assert_eq!(a.settings.profile_dir, alternate.settings.profile_dir);
        assert!(Arc::ptr_eq(&a.session, &alternate.session));
        assert!(!Arc::ptr_eq(&a.session, &b.session));
        assert!(!profiles.may_be_signed_in("bot-a"));
        for bot in ["bot-a", "bot-b"] {
            let path = dir.path().join("browsers").join(bot);
            profile::prepare(&path).unwrap();
            std::fs::write(path.join("Default/Cookies"), b"not a valid database").unwrap();
        }
        // ordinary cookies of a browse are not a sign-in; the owner's sign-in marker is
        assert!(!profiles.may_be_signed_in("bot-a"));
        std::fs::write(dir.path().join("browsers/bot-a/.owner-sign-in"), b"").unwrap();
        assert!(profiles.may_be_signed_in("bot-a"));
        profiles.forget("bot-a", None).await.unwrap();
        assert!(!profiles.may_be_signed_in("bot-a"));
        assert!(dir.path().join("browsers/bot-b/Default/Cookies").exists());
        assert!(profiles.driver("../escape").is_err());
    }
}
