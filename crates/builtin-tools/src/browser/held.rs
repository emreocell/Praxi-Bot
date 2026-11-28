//! Pages held for the owner: a mission's CAPTCHA waits on its own tab, out of the bot's reach.
//!
//! [`BrowserDriver::hold_page`] moves the page the call is on into [`Session::held`] under the
//! hand-over key and gives the bot (or the subagent lane) a fresh blank tab in its place. A held
//! page is in no run's tab list ([`CdpDriver::tab_pages`]) and no browser tool acts on it: the
//! tools only ever reach `sess.page` and the lanes. The live view is moved onto the held page so
//! the owner can take control of it there. [`HeldTab`] is the only handle to it: the daemon
//! reads it to check that the CAPTCHA is gone, nothing else.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::{BrowserDriver, CdpDriver, Session, Wait, current_lane, s, snap_opts};

const HELD: &str = "this page is held for the owner";

impl CdpDriver {
    pub(super) async fn hold_current_page(
        &self,
        key: &str,
    ) -> Result<Arc<dyn BrowserDriver>, String> {
        let mut guard = self.session.lock().await;
        let sess = guard.as_mut().ok_or("the browser is not running")?;
        if sess.held.iter().any(|(k, _)| k == key) {
            return Err("this page is already held".into());
        }
        let lane = current_lane();
        let lane_at = match &lane {
            Some(l) => Some(
                sess.lanes
                    .iter()
                    .position(|(name, _)| name == l)
                    .ok_or("this run has no tab of its own")?,
            ),
            None => None,
        };
        let fresh = sess.browser.new_page("about:blank").await.map_err(s)?;
        let page = match lane_at {
            Some(i) => std::mem::replace(&mut sess.lanes[i].1, fresh),
            None => std::mem::replace(&mut sess.page, fresh),
        };
        // The live view follows the session's first tab, which is never closed: the fresh tab
        // takes that place, and the view moves onto the held page for the owner.
        if sess.extra.home_tab.as_deref() == Some(page.target_id()) {
            sess.extra.home_tab = Some(sess.page.target_id().to_owned());
        }
        if let Some(view) = &self.view {
            let _ = view.detach_to_idle().await;
            let _ = view.attach_page(&page);
        }
        sess.held.push((key.to_owned(), page));
        Ok(Arc::new(HeldTab {
            session: Arc::clone(&self.session),
            key: key.to_owned(),
            on_desktop: !sess.headless,
        }))
    }

    pub(super) async fn release_held(&self, key: &str, give_back: bool) {
        let mut guard = self.session.lock().await;
        let Some(sess) = guard.as_mut() else {
            return;
        };
        let Some(i) = sess.held.iter().position(|(k, _)| k == key) else {
            return;
        };
        let (_, page) = sess.held.remove(i);
        if let Some(view) = &self.view {
            let next = sess.held.last().map_or(&sess.page, |(_, p)| p);
            let _ = view.detach_to_idle().await;
            let _ = view.attach_page(next);
        }
        if !give_back {
            let _ = page.close().await;
        }
        // Given back, the tab stays open and is listed again like any other tab of the bot.
    }
}

/// The only handle to a held page: read-only for the daemon's check that the CAPTCHA is gone.
pub(super) struct HeldTab {
    session: Arc<Mutex<Option<Session>>>,
    key: String,
    on_desktop: bool,
}

impl HeldTab {
    async fn with_page<T>(
        &self,
        f: impl AsyncFnOnce(&browser_cdp::Page, &browser_cdp::Browser) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = tokio::time::timeout(Duration::from_secs(45), self.session.lock())
            .await
            .map_err(|_| "the browser is busy; try again shortly".to_owned())?;
        let sess = guard.as_ref().ok_or("the browser was closed")?;
        let (_, page) = sess
            .held
            .iter()
            .find(|(k, _)| *k == self.key)
            .ok_or("the held page is gone")?;
        f(page, &sess.browser).await
    }
}

#[async_trait]
impl BrowserDriver for HeldTab {
    fn on_desktop(&self) -> bool {
        self.on_desktop
    }
    async fn open(&self, _url: &str, _profile: Option<&str>) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn snapshot(&self, interactive_only: bool) -> Result<String, String> {
        let opts = snap_opts(interactive_only);
        self.with_page(async |page, _| page.snapshot(&opts).await.map(|s| s.render()).map_err(s))
            .await
    }
    async fn click(&self, _r: &str) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn fill(&self, _r: &str, _text: &str) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn select(&self, _r: &str, _value: &str) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn press(&self, _key: &str) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn scroll(&self, _r: Option<&str>, _dx: f64, _dy: f64) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn wait(&self, _cond: Wait, _timeout: Option<Duration>) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn back(&self) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn tabs(&self, _switch_to: Option<usize>) -> Result<String, String> {
        Err(HELD.into())
    }
    async fn screenshot(
        &self,
        _full_page: bool,
        _element: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Err(HELD.into())
    }
    async fn eval(&self, _js: &str) -> Result<Value, String> {
        Err(HELD.into())
    }
    async fn current_url(&self) -> Option<String> {
        self.with_page(async |page, browser| {
            let id = page.target_id().to_owned();
            let pages = browser.pages().await.map_err(s)?;
            Ok(pages.into_iter().find(|t| t.target_id == id).map(|t| t.url))
        })
        .await
        .ok()
        .flatten()
    }
    async fn capture_epoch(&self) -> Option<String> {
        self.with_page(async |page, _| Ok(page.capture_epoch()))
            .await
            .ok()
    }
}
