//! `browser_capture_secret`: a value the page shows (an API key, a token, an app password)
//! goes from the page element straight into the secret store, bound to this bot and to the
//! site it was read on. The model gets a NAME back and never the value.
//!
//! The same frame-origin discipline as a blind fill, run the other way: the browser reports
//! the origin of the frame that holds the element and of the top-level page, both must be one
//! verifiable `https://` site (or a loopback address in tests), the daemon reads the element
//! itself and checks that the document did not change in between. The value is stored by
//! [`FillSecrets::capture_secret`] (so the redactor learns it and every later echo of it is
//! masked), the element is masked in later snapshots, and pictures of the page are withheld
//! until it navigates. Typing the value later is `browser_fill {secret}`, which re-checks the
//! live origin against the one stored here.

use std::sync::Arc;

use browser_cdp::ReadTarget;
use secret_store::{FillSecrets, NewCapture, SecretValue, canonical_origin, sanitize_label};
use serde::Deserialize;
use serde_json::{Value, json};
use tool_registry::{ToolContext, ToolError, ToolOutput};
use url::Url;

use super::{BrowserTool, Op, Shared};
use crate::account_register::bot_tag;
use crate::fill_secret::refused;
use crate::util::{exec_err, parse};

/// Name of the tool.
pub const BROWSER_CAPTURE_SECRET: &str = "browser_capture_secret";

const NAME_MAX: usize = 16;
const LABEL_MAX: usize = 16;
/// The store's longest secret name.
const SECRET_NAME_MAX: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureIn {
    #[serde(default)]
    r#ref: Option<String>,
    #[serde(default)]
    selector: Option<String>,
    name: String,
    #[serde(default)]
    label: Option<String>,
}

impl CaptureIn {
    fn target(&self) -> Result<ReadTarget<'_>, ToolError> {
        match (
            self.r#ref.as_deref().map(str::trim).filter(|r| !r.is_empty()),
            self.selector
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty()),
        ) {
            (Some(r), None) => Ok(ReadTarget::Ref(r)),
            (None, Some(s)) => Ok(ReadTarget::Selector(s)),
            _ => Err(ToolError::InvalidInput(
                "give exactly one of `ref` (from browser_snapshot) or `selector` (CSS, top page only)"
                    .into(),
            )),
        }
    }
}

/// `raw` as a lower-case slug of letters, digits and `-`.
fn slug(raw: &str, what: &str, max: usize) -> Result<String, ToolError> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty()
        || s.len() > max
        || s.starts_with('-')
        || !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ToolError::InvalidInput(format!(
            "`{what}` is 1 to {max} letters, digits or '-' (for example \"api-key\")"
        )));
    }
    Ok(s)
}

/// FNV-1a, 16 bits shown as 4 hex digits: tells two long host names with one prefix apart.
fn short_hash(text: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in text.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("{:04x}", (h ^ (h >> 16)) & 0xffff)
}

/// `token/<host>-<bot>[-<label>]-<name>`, always a legal secret name of at most 64 bytes.
fn secret_name(
    origin: &str,
    bot: &str,
    label: Option<&str>,
    name: &str,
) -> Result<String, ToolError> {
    let host = Url::parse(origin)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .ok_or_else(|| ToolError::Execution("the page has no host name".into()))?;
    let host: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let bot = bot_tag(bot)?;
    let tail = match label {
        Some(l) => format!("{bot}-{l}-{name}"),
        None => format!("{bot}-{name}"),
    };
    // `token/` + host + `-` + tail
    let room = SECRET_NAME_MAX.saturating_sub("token/".len() + 1 + tail.len());
    let host = if host.len() <= room {
        host
    } else if room >= 8 {
        // A cut host keeps a hash of the whole origin, so two long names never share an entry.
        let kept: String = host.chars().take(room - 5).collect();
        format!("{kept}-{}", short_hash(origin))
    } else {
        return Err(ToolError::InvalidInput(
            "the label and name are too long for this site's address; use shorter ones".into(),
        ));
    };
    Ok(format!("token/{host}-{tail}"))
}

/// Whether a capture may take a value from this page: ONE verifiable site, `https://` (or
/// a loopback address, so a local test page works), as both the frame and the top page.
fn page_ok(frame: Option<String>, top: Option<String>) -> bool {
    let (Some(frame), Some(top)) = (frame, top) else {
        return false;
    };
    let (Ok(frame), Ok(top)) = (canonical_origin(&frame), canonical_origin(&top)) else {
        return false;
    };
    frame == top && secure_origin(&frame)
}

/// The origin a capture is bound to: the frame's and the top page's origin, which must be
/// one and the same verifiable site ([`page_ok`]), as the vault stores it. `None` otherwise.
fn bound_origin(frame: Option<&str>, top: Option<&str>) -> Option<String> {
    if !page_ok(frame.map(str::to_owned), top.map(str::to_owned)) {
        return None;
    }
    canonical_origin(top?).ok()
}

fn secure_origin(origin: &str) -> bool {
    origin.starts_with("https://")
        || Url::parse(origin)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]")
}

impl BrowserTool {
    /// What the approval card of a capture shows: the facts are the model's request and the
    /// page the browser is on now; the value is not read before the owner agrees.
    pub(super) async fn capture_details(
        &self,
        ctx: &ToolContext,
        input: &Value,
    ) -> Result<Value, ToolError> {
        let i: CaptureIn = parse(input.clone())?;
        i.target()?;
        let name = slug(&i.name, "name", NAME_MAX)?;
        let label = i
            .label
            .as_deref()
            .filter(|l| !l.trim().is_empty())
            .map(|l| slug(l, "label", LABEL_MAX))
            .transpose()?;
        let shared = self.select_shared(ctx)?;
        let top = shared
            .driver
            .current_url()
            .await
            .and_then(|u| super::origin_of(&u))
            .ok_or_else(|| exec_err("the browser did not report which site this page is on"))?;
        // As the vault will store it (no default port).
        let top = canonical_origin(&top).unwrap_or(top);
        let shown = match (&i.r#ref, &i.selector) {
            (Some(r), _) => format!("element {}", sanitize_label(r, 40)),
            (_, Some(s)) => format!("selector {}", sanitize_label(s, 80)),
            _ => String::new(),
        };
        Ok(json!({
            "kind": "secret_capture",
            "tool": BROWSER_CAPTURE_SECRET,
            "name": name,
            "label": label,
            "site": top,
            "element": shown,
            "note": format!("Praxi reads the value of {shown} on {top} and keeps it in the vault, limited to this site and to this bot. The assistant never sees it."),
        }))
    }

    /// `browser_capture_secret`.
    pub(super) async fn capture(
        &self,
        ctx: &ToolContext,
        shared: &Arc<Shared>,
        input: &Value,
    ) -> Result<ToolOutput, ToolError> {
        let i: CaptureIn = parse(input.clone())?;
        let target = i.target()?;
        let name = slug(&i.name, "name", NAME_MAX)?;
        let label = i
            .label
            .as_deref()
            .filter(|l| !l.trim().is_empty())
            .map(|l| slug(l, "label", LABEL_MAX))
            .transpose()?;
        let Some(bot) = ctx.bot_id.as_deref() else {
            return Err(ToolError::Execution(
                "a captured value belongs to a bot; this call has no bot".into(),
            ));
        };
        let secrets: Arc<FillSecrets> = shared
            .fill_secrets
            .clone()
            .ok_or_else(|| exec_err("no secrets are configured on this daemon"))?;
        // The owner may have paused or taken over the page: read nothing then.
        BrowserTool {
            spec: self.spec.clone(),
            op: Op::Snapshot,
            shared: Arc::clone(shared),
        }
        .refuse_observation_while_latched()?;
        let read = shared
            .driver
            .read_value(target, &page_ok)
            .await
            .map_err(exec_err)?;
        let value = SecretValue::new(read.value);
        // The binding is the TOP page's origin as it was at read time, and the frame that
        // held the element must be that same origin. Checked again on what the browser
        // returned (not only by `page_ok` inside the read), and against the page the browser
        // is on NOW: a value read on one site is never kept for another.
        let origin = bound_origin(read.frame_origin.as_deref(), read.top_origin.as_deref())
            .ok_or_else(|| {
                exec_err("the element is not on one verifiable site; nothing was kept")
            })?;
        let now = shared
            .driver
            .current_url()
            .await
            .and_then(|u| super::origin_of(&u))
            .and_then(|o| canonical_origin(&o).ok());
        if now.as_deref() != Some(origin.as_str()) {
            return Err(exec_err(
                "the page is not the site the value was read on any more; nothing was kept",
            ));
        }
        let secret = secret_name(&origin, bot, label.as_deref(), &name)?;
        secrets
            .capture_secret(
                &NewCapture {
                    name: &secret,
                    site: &origin,
                    what: &name.replace('-', " "),
                    bot_id: bot,
                    note: None,
                    label: label.as_deref(),
                },
                &value,
            )
            .map_err(|e| refused(&e))?;
        drop(value);
        // The value is still on the page in the clear: no pictures of it until it navigates.
        crate::handover::handover_hub()
            .latch_visible_secret(&shared.driver)
            .await;
        Ok(ToolOutput::text(format!(
            "Captured. Secret name: {secret}\nSite: {origin}\nThe value is stored by Praxi, limited to you and to {origin}; it is not shown to you. Type it into a field of that site with browser_fill {{\"ref\":\"<ref>\",\"secret\":\"{secret}\"}}. The page hides it from snapshots and screenshots until you leave the page; account_list shows the entry, account_discard removes it."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::browser::fake::FakeBrowser;
    use crate::browser::{self, BrowserDriver};
    use secret_store::{MemoryStore, Redactor};
    use tool_registry::Tool;

    const KEY: &str = "sk-live-Zx81QpLm0AbCdEf";
    const BOT: &str = "bot-aaaa-1111";
    const SITE: &str = "https://example.com";

    struct Rig {
        _dir: tempfile::TempDir,
        secrets: Arc<FillSecrets>,
        redactor: Arc<parking_lot::Mutex<Redactor>>,
        fake: Arc<FakeBrowser>,
        tool: Arc<dyn Tool>,
    }

    fn rig(frame: Option<&str>, top: Option<&str>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let redactor = Arc::new(parking_lot::Mutex::new(Redactor::new()));
        let sink = Arc::clone(&redactor);
        let secrets = Arc::new(
            FillSecrets::in_memory(Arc::new(MemoryStore::new()))
                .with_value_hook(Arc::new(move |v| sink.lock().add_secret(v))),
        );
        let fake = Arc::new(FakeBrowser::default());
        fake.reads.lock().insert(
            "ref:e9".into(),
            (KEY.into(), frame.map(str::to_owned), top.map(str::to_owned)),
        );
        fake.reads.lock().insert(
            "sel:#api-key".into(),
            (KEY.into(), frame.map(str::to_owned), top.map(str::to_owned)),
        );
        *fake.url.lock() = Some(format!("{}/settings/keys", top.unwrap_or(SITE)));
        let tool =
            browser::tools_with_secrets(fake.clone(), dir.path(), None, Some(Arc::clone(&secrets)))
                .into_iter()
                .find(|t| t.spec().name == BROWSER_CAPTURE_SECRET)
                .unwrap();
        Rig {
            _dir: dir,
            secrets,
            redactor,
            fake,
            tool,
        }
    }

    fn ctx(bot: Option<&str>) -> ToolContext {
        ToolContext {
            bot_id: bot.map(str::to_owned),
            ..ToolContext::default()
        }
    }

    fn shown(out: &ToolOutput) -> String {
        format!("{out:?}")
    }

    /// The model gets a name and never the value; the vault holds it for this bot on this
    /// site only; the redactor learned it; no later picture of the page is taken.
    #[tokio::test]
    async fn the_value_goes_page_to_vault_bound_to_bot_and_origin_and_the_model_gets_a_name() {
        let r = rig(Some(SITE), Some(SITE));
        let name = "token/example.com-kq5s3tajfu-api-key";
        for input in [
            json!({"ref": "e9", "name": "api-key"}),
            json!({"selector": "#api-key", "name": "api-key"}),
        ] {
            let out = r.tool.call(&ctx(Some(BOT)), input).await.unwrap();
            let text = shown(&out);
            assert!(text.contains(name), "{text}");
            assert!(!text.contains(KEY) && !text.contains("Zx81"), "{text}");
        }
        assert!(!r.fake.calls.lock().iter().any(|c| c.contains(KEY)));
        // Bound to the capturing bot and to the origin it was read on.
        assert_eq!(
            r.secrets
                .resolve_value_for(name, Some(BOT))
                .unwrap()
                .expose(),
            KEY
        );
        assert!(r.secrets.check_browser(name, Some(SITE), Some(BOT)).is_ok());
        assert!(
            r.secrets
                .check_browser(name, Some("https://evil.example"), Some(BOT))
                .is_err()
        );
        assert!(
            r.secrets
                .check_browser(name, Some(SITE), Some("bot-bbbb-2222"))
                .is_err()
        );
        assert!(
            r.secrets
                .resolve_value_for(name, Some("bot-bbbb-2222"))
                .is_err()
        );
        assert!(r.secrets.resolve_value_for(name, None).is_err());
        // The same capture again is a rotation, not a second entry.
        assert_eq!(r.secrets.accounts(Some(BOT)).unwrap().len(), 1);
        // Anything that echoes the value later is masked.
        let masked = r.redactor.lock().redact(&format!("the key is {KEY}."));
        assert!(!masked.contains(KEY), "{masked}");
        // The page still shows it in the clear: pictures are withheld until it navigates.
        let driver: Arc<dyn BrowserDriver> = r.fake.clone();
        assert!(
            crate::handover::handover_hub()
                .capture_latched(&driver)
                .await
        );
    }

    #[tokio::test]
    async fn only_one_verifiable_https_site_and_a_named_bot_may_keep_a_value() {
        let cases = [
            (
                "a frame of another site",
                Some("https://ads.example"),
                Some(SITE),
            ),
            ("no frame origin", None, Some(SITE)),
            ("no top origin", Some(SITE), None),
            (
                "plain http",
                Some("http://example.com"),
                Some("http://example.com"),
            ),
        ];
        for (label, frame, top) in cases {
            let r = rig(frame, top);
            let err = r
                .tool
                .call(&ctx(Some(BOT)), json!({"ref": "e9", "name": "api-key"}))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("nothing was read"), "{label}: {err}");
            assert!(r.secrets.accounts(Some(BOT)).unwrap().is_empty(), "{label}");
            assert!(!err.contains(KEY));
        }
        let r = rig(Some(SITE), Some(SITE));
        // No bot, both or neither of ref and selector, a bad name: refused, nothing stored.
        for (label, bot, input) in [
            ("no bot", None, json!({"ref": "e9", "name": "k"})),
            (
                "both",
                Some(BOT),
                json!({"ref": "e9", "selector": "#k", "name": "k"}),
            ),
            ("neither", Some(BOT), json!({"name": "k"})),
            ("bad name", Some(BOT), json!({"ref": "e9", "name": "A B"})),
            (
                "bad label",
                Some(BOT),
                json!({"ref": "e9", "name": "k", "label": "../x"}),
            ),
            ("unknown ref", Some(BOT), json!({"ref": "e1", "name": "k"})),
        ] {
            assert!(r.tool.call(&ctx(bot), input).await.is_err(), "{label}");
        }
        assert_eq!(r.secrets.accounts(None).unwrap(), Vec::new());
        // A daemon without a secret store, and a driver that cannot read a value, refuse too.
        let dir = tempfile::tempdir().unwrap();
        let none = browser::tools_with_secrets(r.fake.clone(), dir.path(), None, None)
            .into_iter()
            .find(|t| t.spec().name == BROWSER_CAPTURE_SECRET)
            .unwrap();
        assert!(
            none.call(&ctx(Some(BOT)), json!({"ref": "e9", "name": "k"}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_capture_always_asks_and_its_card_never_holds_the_value() {
        let r = rig(Some(SITE), Some(SITE));
        let input = json!({"ref": "e9", "name": "api-key", "label": "second"});
        assert!(
            r.tool.always_ask_for(&input),
            "the Full level waives it in the gate, not here"
        );
        let card = r
            .tool
            .approval_details(&ctx(Some(BOT)), &input)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(card["kind"], "secret_capture");
        assert_eq!(card["site"], SITE);
        assert_eq!(card["label"], "second");
        assert!(!card.to_string().contains(KEY));
        // Preparing the card reads nothing from the page.
        assert!(
            !r.fake
                .calls
                .lock()
                .iter()
                .any(|c| c.starts_with("read_value"))
        );
        // A labelled capture lands under its own name next to an unlabelled one.
        r.tool.call(&ctx(Some(BOT)), input).await.unwrap();
        r.tool
            .call(&ctx(Some(BOT)), json!({"ref": "e9", "name": "api-key"}))
            .await
            .unwrap();
        let names: Vec<String> = r
            .secrets
            .accounts(Some(BOT))
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(
            names,
            [
                "token/example.com-kq5s3tajfu-api-key",
                "token/example.com-kq5s3tajfu-second-api-key"
            ]
        );
    }

    #[test]
    fn secret_names_are_legal_short_and_tell_sites_apart() {
        let n = secret_name("https://example.com", "bot-aaaa-1111", None, "api-key").unwrap();
        assert_eq!(n, "token/example.com-kq5s3tajfu-api-key");
        secret_store::validate_fill_name(&n).unwrap();
        let l = secret_name(
            "https://example.com",
            "bot-aaaa-1111",
            Some("second"),
            "api-key",
        )
        .unwrap();
        assert_eq!(l, "token/example.com-kq5s3tajfu-second-api-key");
        secret_store::validate_fill_name(&l).unwrap();
        // A long host is cut but keeps a hash of the whole origin.
        let long_a = "https://a-very-long-subdomain-name-for-one.docs.example-hosting.com";
        let long_b = "https://a-very-long-subdomain-name-for-two.docs.example-hosting.com";
        let a = secret_name(
            long_a,
            "bot-aaaa-1111",
            Some("sixteen-chars-ok"),
            "sixteen-chars-ok",
        )
        .unwrap();
        let b = secret_name(
            long_b,
            "bot-aaaa-1111",
            Some("sixteen-chars-ok"),
            "sixteen-chars-ok",
        )
        .unwrap();
        assert!(
            a.len() <= SECRET_NAME_MAX && b.len() <= SECRET_NAME_MAX,
            "{a} {b}"
        );
        assert_ne!(a, b);
        secret_store::validate_fill_name(&a).unwrap();
    }

    /// A value read on one site is never kept under another: the page the browser is on
    /// after the read must still be the site the element was on.
    #[tokio::test]
    async fn a_page_that_moved_to_another_site_after_the_read_keeps_nothing() {
        let r = rig(Some(SITE), Some(SITE));
        *r.fake.url.lock() = Some("https://evil.example/collect".into());
        let err = r
            .tool
            .call(&ctx(Some(BOT)), json!({"ref": "e9", "name": "api-key"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("nothing was kept"), "{err}");
        assert!(!err.contains(KEY));
        assert_eq!(r.secrets.accounts(None).unwrap(), Vec::new());
        // A browser that cannot say where it is keeps nothing either.
        *r.fake.url.lock() = None;
        assert!(
            r.tool
                .call(&ctx(Some(BOT)), json!({"ref": "e9", "name": "api-key"}))
                .await
                .is_err()
        );
        assert_eq!(r.secrets.accounts(None).unwrap(), Vec::new());
        // The entry that IS stored carries the site the page was on, in its name and binding.
        *r.fake.url.lock() = Some(format!("{SITE}/settings/keys"));
        r.tool
            .call(&ctx(Some(BOT)), json!({"ref": "e9", "name": "api-key"}))
            .await
            .unwrap();
        let acc = r.secrets.accounts(Some(BOT)).unwrap();
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].site, SITE);
        assert!(acc[0].name.starts_with("token/example.com-"), "{acc:?}");
    }

    #[test]
    fn a_capture_binds_to_one_origin_the_frame_and_the_top_page_share() {
        assert_eq!(
            bound_origin(Some("https://example.com"), Some("https://example.com")).as_deref(),
            Some("https://example.com")
        );
        // Default port and case do not make a second site.
        assert_eq!(
            bound_origin(Some("https://Example.com:443"), Some("https://example.com")).as_deref(),
            Some("https://example.com")
        );
        for (frame, top) in [
            (Some("https://evil.example"), Some("https://example.com")),
            (Some("https://example.com"), Some("https://evil.example")),
            (None, Some("https://example.com")),
            (Some("https://example.com"), None),
            (Some("http://example.com"), Some("http://example.com")),
        ] {
            assert_eq!(bound_origin(frame, top), None, "{frame:?} {top:?}");
        }
    }

    #[test]
    fn bots_get_their_own_namespace_from_the_whole_id() {
        // The algorithm is pinned: names already stored must keep resolving.
        assert_eq!(bot_tag(BOT).unwrap(), "kq5s3tajfu");
        // Ids that share a prefix, or only symbols, no longer share (or lose) a namespace.
        let a = secret_name(SITE, "bot-aaaa-1111", None, "api-key").unwrap();
        let b = secret_name(SITE, "bot-aaaa-2222", None, "api-key").unwrap();
        let c = secret_name(SITE, "!!!", None, "api-key").unwrap();
        let d = secret_name(SITE, "???", None, "api-key").unwrap();
        assert!(a != b && c != d && a != c, "{a} {b} {c} {d}");
        for n in [&a, &b, &c, &d] {
            secret_store::validate_fill_name(n).unwrap();
        }
        // No bot, or a blank one, makes no name.
        for bot in ["", "   "] {
            assert!(secret_name(SITE, bot, None, "api-key").is_err(), "{bot:?}");
        }
    }

    #[tokio::test]
    async fn two_bots_with_one_id_prefix_do_not_see_or_overwrite_each_other() {
        let r = rig(Some(SITE), Some(SITE));
        let input = json!({"ref": "e9", "name": "api-key"});
        r.tool.call(&ctx(Some(BOT)), input.clone()).await.unwrap();
        let other = "bot-aaaa-2222";
        r.tool.call(&ctx(Some(other)), input).await.unwrap();
        assert_eq!(r.secrets.accounts(Some(BOT)).unwrap().len(), 1);
        assert_eq!(r.secrets.accounts(Some(other)).unwrap().len(), 1);
        assert_ne!(
            r.secrets.accounts(Some(BOT)).unwrap()[0].name,
            r.secrets.accounts(Some(other)).unwrap()[0].name
        );
        // A blank bot id captures nothing.
        assert!(
            r.tool
                .call(&ctx(Some("  ")), json!({"ref": "e9", "name": "api-key"}))
                .await
                .is_err()
        );
    }

    #[test]
    fn only_one_secure_site_may_keep_a_captured_value() {
        let ok = |frame: Option<&str>, top: Option<&str>| {
            page_ok(frame.map(str::to_owned), top.map(str::to_owned))
        };
        assert!(ok(Some("https://example.com"), Some("https://example.com")));
        assert!(ok(
            Some("http://127.0.0.1:8080"),
            Some("http://127.0.0.1:8080")
        ));
        // A frame of another site, an opaque or unknown origin, plain http on the internet.
        assert!(!ok(
            Some("https://ads.example"),
            Some("https://example.com")
        ));
        assert!(!ok(None, Some("https://example.com")));
        assert!(!ok(Some("https://example.com"), None));
        assert!(!ok(Some("http://example.com"), Some("http://example.com")));
        assert!(!ok(Some("null"), Some("null")));
    }

    #[test]
    fn name_label_and_target_are_checked() {
        assert!(slug("API Key", "name", NAME_MAX).is_err());
        assert!(slug("-x", "name", NAME_MAX).is_err());
        assert!(slug("../x", "name", NAME_MAX).is_err());
        assert!(slug(&"a".repeat(NAME_MAX + 1), "name", NAME_MAX).is_err());
        assert_eq!(slug(" Api-Key ", "name", NAME_MAX).unwrap(), "api-key");
        let both: CaptureIn =
            serde_json::from_value(json!({"ref":"e1","selector":"#k","name":"k"})).unwrap();
        assert!(both.target().is_err());
        let none: CaptureIn = serde_json::from_value(json!({"name":"k"})).unwrap();
        assert!(none.target().is_err());
        assert!(matches!(
            serde_json::from_value::<CaptureIn>(json!({"ref":"e1","name":"k"}))
                .unwrap()
                .target(),
            Ok(ReadTarget::Ref("e1"))
        ));
    }
}
