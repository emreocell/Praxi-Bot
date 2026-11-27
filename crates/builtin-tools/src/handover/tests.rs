use std::sync::Mutex as StdMutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext};

use super::*;
use crate::browser::Wait;

/// Local "pages": the snapshot text a driver shows.
const RECAPTCHA_PAGE: &str = "# Sign up (https://shop.example/signup)\n  [e1] textbox \"Email\"\n  iframe \"reCAPTCHA\"\n    [e2] checkbox \"I'm not a robot\"\n  [e3] button \"Create account\"";
const V3_PAGE: &str = "# Contact (https://shop.example/contact)\n  [e1] textbox \"Email\"\n  [e2] button \"Send\"\n  iframe \"reCAPTCHA\"\n  text \"This site is protected by reCAPTCHA and the Google Privacy Policy and Terms of Service apply.\"";
const HOLD_PAGE: &str = "# Just a moment (https://shop.example/x)\n  text \"Verify you are human\"\n  [e4] button \"Press & hold\"";
const TILES_PAGE: &str = "# Check (https://shop.example/x)\n  text \"hCaptcha\"\n  text \"Select all images with a bus\"";
const CODE_PAGE: &str = "# Verify (https://bank.example/2fa)\n  text \"We sent a code to your phone\"\n  [e7] textbox \"Verification code\"\n  [e8] button \"Verify\"";
const CODE_PAGE_TR: &str =
    "# Doğrulama (https://bank.example/2fa)\n  [e1] textbox \"Doğrulama kodu\"";
const CODE_PAGE_DE: &str =
    "# Prüfung (https://bank.example/2fa)\n  [e1] textbox \"Bestätigungscode\"";
const PROSE_ABOUT_CAPTCHA: &str = "# Blog (https://blog.example/p)\n  heading \"Why CAPTCHA tests are annoying\"\n  text \"A captcha is a test\"";
const COUPON_PAGE: &str = "# Cart (https://shop.example/cart)\n  [e1] textbox \"Discount code\"\n  [e2] textbox \"Postal code\"\n  [e3] button \"Apply\"";
const LOGIN_PAGE: &str = "# Sign in (https://bank.example/login)\n  [e1] textbox \"Email\"\n  [e2] textbox \"Password\"\n  [e3] button \"Sign in\"";

/// The typed-code latch list is process-wide (a desktop tool looks at every browser), so the
/// tests that type a code or call a desktop tool take turns.
static LATCH_STATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds the turn and, when the test ends, drops every typed-code latch it left behind so a
/// desktop-tool test running next never meets it.
struct LatchTurn(#[allow(dead_code)] tokio::sync::MutexGuard<'static, ()>);

impl Drop for LatchTurn {
    fn drop(&mut self) {
        handover_hub()
            .captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

async fn serial() -> LatchTurn {
    LatchTurn(LATCH_STATE.lock().await)
}

struct Page {
    snapshot: StdMutex<String>,
    url: StdMutex<Option<String>>,
    typed: StdMutex<Vec<(String, String)>>,
    fail_with_code: bool,
    epoch: StdMutex<Option<String>>,
    stop_on_snapshot: StdMutex<Option<String>>,
    /// How many times the bot's own pointer was taken off the page.
    cursor_hidden: std::sync::atomic::AtomicUsize,
}

impl Page {
    fn new(snapshot: &str, url: &str) -> Arc<Self> {
        Arc::new(Self {
            snapshot: StdMutex::new(snapshot.to_owned()),
            url: StdMutex::new(Some(url.to_owned())),
            typed: StdMutex::new(Vec::new()),
            fail_with_code: false,
            epoch: StdMutex::new(None),
            stop_on_snapshot: StdMutex::new(None),
            cursor_hidden: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn show(&self, snapshot: &str, url: &str) {
        *self.snapshot.lock().unwrap() = snapshot.to_owned();
        *self.url.lock().unwrap() = Some(url.to_owned());
    }
}

#[async_trait]
impl BrowserDriver for Page {
    async fn bot_cursor_hide(&self) {
        self.cursor_hidden
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    async fn open(&self, _u: &str, _p: Option<&str>) -> Result<String, String> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
    async fn snapshot(&self, _i: bool) -> Result<String, String> {
        if let Some(run) = self.stop_on_snapshot.lock().unwrap().take() {
            handover_hub().forget(&run);
        }
        Ok(self.snapshot.lock().unwrap().clone())
    }
    async fn click(&self, _r: &str) -> Result<String, String> {
        Ok(String::new())
    }
    async fn fill(&self, _r: &str, _t: &str) -> Result<String, String> {
        Ok(String::new())
    }
    async fn fill_secret(
        &self,
        r: &str,
        text: &str,
        accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
    ) -> Result<String, String> {
        let origin = self.url.lock().unwrap().clone();
        if !accept(origin) {
            return Err("refused".into());
        }
        if self.fail_with_code {
            return Err(format!("the page said no to {text}"));
        }
        self.typed
            .lock()
            .unwrap()
            .push((r.to_owned(), text.to_owned()));
        Ok(format!("filled {r}"))
    }
    async fn select(&self, _r: &str, _v: &str) -> Result<String, String> {
        Ok(String::new())
    }
    async fn press(&self, _k: &str) -> Result<String, String> {
        Ok(String::new())
    }
    async fn scroll(&self, _r: Option<&str>, _x: f64, _y: f64) -> Result<String, String> {
        Ok(String::new())
    }
    async fn wait(&self, _c: Wait, _t: Option<Duration>) -> Result<String, String> {
        Ok(String::new())
    }
    async fn back(&self) -> Result<String, String> {
        Ok(String::new())
    }
    async fn tabs(&self, _s: Option<usize>) -> Result<String, String> {
        Ok(String::new())
    }
    async fn screenshot(
        &self,
        _f: bool,
        _e: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Ok((vec![], "png"))
    }
    async fn eval(&self, _js: &str) -> Result<Value, String> {
        Ok(Value::Null)
    }
    async fn pointer(&self, _o: &crate::browser::PointerOpts) -> Result<String, String> {
        Ok("pointed".into())
    }
    async fn grounded_screenshot(&self) -> Result<crate::browser::GroundedShot, String> {
        Ok(crate::browser::GroundedShot {
            bytes: b"\xff\xd8jpeg".to_vec(),
            ext: "jpg",
            width: 800,
            height: 600,
            notes: "image 800x600 px".into(),
        })
    }
    async fn current_url(&self) -> Option<String> {
        self.url.lock().unwrap().clone()
    }
    async fn capture_epoch(&self) -> Option<String> {
        self.epoch
            .lock()
            .unwrap()
            .clone()
            .or_else(|| self.url.lock().unwrap().clone())
    }
    fn on_desktop(&self) -> bool {
        true
    }
}

fn tool(page: &Arc<Page>, name: &str) -> Arc<dyn Tool> {
    let dir = tempfile::tempdir().unwrap();
    crate::browser::tools(page.clone(), dir.path(), None)
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

fn ctx(run: &str) -> ToolContext {
    ToolContext {
        run_id: run.into(),
        ..ToolContext::default()
    }
}

#[test]
fn detects_captchas_codes_and_ignores_look_alikes() {
    assert_eq!(
        detect(RECAPTCHA_PAGE),
        Some(Challenge::Captcha { hard: false })
    );
    assert_eq!(detect(HOLD_PAGE), Some(Challenge::Captcha { hard: true }));
    assert_eq!(detect(TILES_PAGE), Some(Challenge::Captcha { hard: true }));
    for page in [CODE_PAGE, CODE_PAGE_TR, CODE_PAGE_DE] {
        assert!(
            matches!(detect(page), Some(Challenge::Code { field: Some(_) })),
            "{page}"
        );
    }
    assert_eq!(
        detect(CODE_PAGE),
        Some(Challenge::Code {
            field: Some("e7".into())
        })
    );
    // An article about CAPTCHAs, a coupon box, a plain login: nothing to hand over.
    for page in [PROSE_ABOUT_CAPTCHA, COUPON_PAGE, LOGIN_PAGE] {
        assert_eq!(detect(page), None, "{page}");
    }
}

#[test]
fn own_captcha_attempts_are_capped_at_two_then_the_notice_says_hand_over() {
    let hub = HandoverHub::default();
    let c = Challenge::Captcha { hard: false };
    let site = Some("https://shop.example");
    let spent: Vec<bool> = (0..3)
        .map(|_| hub.captcha_action("run-cap", site, RECAPTCHA_PAGE).is_ok())
        .collect();
    assert_eq!(spent, [true, true, false]);
    assert_eq!(hub.next_captcha_attempt("run-cap", site), 3);
    assert_eq!(
        hub.next_captcha_attempt("run-cap", Some("https://other.example")),
        1
    );
    assert!(notice(&c, 1).contains("attempt 1 of 2"));
    assert!(notice(&c, 2).contains("attempt 2 of 2"));
    let third = notice(&c, 3);
    assert!(
        third.contains("must not try") && third.contains("browser_handover"),
        "{third}"
    );
    assert!(notice(&Challenge::Captcha { hard: true }, 1).contains("must not try"));
    hub.forget("run-cap");
    assert_eq!(hub.next_captcha_attempt("run-cap", site), 1);
}

#[tokio::test]
async fn snapshot_ends_with_a_daemon_notice_only_when_a_challenge_is_there() {
    let page = Page::new(RECAPTCHA_PAGE, "https://shop.example/signup");
    let snap = tool(&page, "browser_snapshot");
    let out = snap.call(&ctx("run-notice"), json!({})).await.unwrap();
    let ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(
        text.contains("[Praxi notice]") && text.contains("attempt 1 of 2"),
        "{text}"
    );
    page.show(LOGIN_PAGE, "https://bank.example/login");
    let out = snap.call(&ctx("run-notice"), json!({})).await.unwrap();
    let ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(!text.contains("[Praxi notice]"), "{text}");
    handover_hub().forget("run-notice");
}

#[tokio::test]
async fn the_tool_checks_the_page_itself_and_names_the_site_the_browser_reports() {
    let page = Page::new(LOGIN_PAGE, "https://bank.example/login");
    let t = tool(&page, "browser_handover");
    // The model claims a code prompt where there is none: refused, nothing armed.
    let err = t
        .call(&ctx("run-h1"), json!({"kind":"code"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not show"), "{err}");
    assert_eq!(
        handover_hub().type_code("run-h1", "123456").await,
        Err(HandoverError::NotArmed)
    );
    page.show(CODE_PAGE, "https://bank.example/2fa");
    let out = t
        .call(&ctx("run-h1"), json!({"kind":"code"}))
        .await
        .unwrap();
    let ContentPart::Json { data } = &out.content[0] else {
        panic!("card part first")
    };
    let card = HandoverCard::from_part(data).unwrap();
    assert_eq!(card.kind, HandoverKind::Code);
    assert_eq!(card.site, "https://bank.example:443");
    handover_hub().forget("run-h1");
}

#[tokio::test]
async fn a_code_is_typed_into_the_page_it_was_asked_for_and_never_elsewhere() {
    let _serial = serial().await;
    let page = Page::new(CODE_PAGE, "https://bank.example/2fa");
    let t = tool(&page, "browser_handover");
    t.call(&ctx("run-h2"), json!({"kind":"code"}))
        .await
        .unwrap();
    let hub = handover_hub();
    // Not a code: refused before anything is typed.
    assert_eq!(
        hub.type_code("run-h2", "12").await,
        Err(HandoverError::BadCode)
    );
    assert_eq!(
        hub.type_code("run-h2", "12 34 <b>").await,
        Err(HandoverError::BadCode)
    );
    // The page moved to another site: nothing is typed there.
    page.show(CODE_PAGE, "https://evil.example/2fa");
    assert_eq!(
        hub.type_code("run-h2", "123456").await,
        Err(HandoverError::WrongPage)
    );
    // Same site but no code prompt any more.
    page.show(LOGIN_PAGE, "https://bank.example/home");
    assert_eq!(
        hub.type_code("run-h2", "123456").await,
        Err(HandoverError::NoPrompt)
    );
    assert!(page.typed.lock().unwrap().is_empty());
    // Back on the prompt: typed, with separators removed.
    page.show(CODE_PAGE, "https://bank.example/2fa");
    hub.type_code("run-h2", "123 456").await.unwrap();
    assert_eq!(
        page.typed.lock().unwrap().as_slice(),
        [("e7".to_owned(), "123456".to_owned())]
    );
    hub.forget("run-h2");
}

#[tokio::test]
async fn a_failing_page_never_echoes_the_code_back() {
    let _serial = serial().await;
    let page = Arc::new(Page {
        fail_with_code: true,
        epoch: StdMutex::new(None),
        ..Arc::try_unwrap(Page::new(CODE_PAGE, "https://bank.example/2fa"))
            .ok()
            .unwrap()
    });
    let t = tool(&page, "browser_handover");
    t.call(&ctx("run-h3"), json!({"kind":"code"}))
        .await
        .unwrap();
    let err = handover_hub()
        .type_code("run-h3", "987654")
        .await
        .unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("987654"), "{shown}");
    handover_hub().forget("run-h3");
}

#[tokio::test]
async fn a_handover_takes_the_bot_pointer_off_the_page() {
    let page = Page::new(HOLD_PAGE, "https://shop.example/x");
    assert_eq!(
        page.cursor_hidden.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    tool(&page, "browser_handover")
        .call(&ctx("run-h-cursor"), json!({"kind":"captcha"}))
        .await
        .unwrap();
    assert_eq!(
        page.cursor_hidden.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    handover_hub().forget("run-h-cursor");
}

#[tokio::test]
async fn a_captcha_handover_finishes_only_when_the_check_is_gone() {
    let page = Page::new(HOLD_PAGE, "https://shop.example/x");
    let t = tool(&page, "browser_handover");
    t.call(&ctx("run-h4"), json!({"kind":"captcha"}))
        .await
        .unwrap();
    let hub = handover_hub();
    assert_eq!(
        hub.verify_solved("run-h4").await,
        Err(HandoverError::StillThere)
    );
    page.show(LOGIN_PAGE, "https://shop.example/account");
    hub.verify_solved("run-h4").await.unwrap();
    // A captcha request cannot be used to type a code.
    assert_eq!(
        hub.type_code("run-h4", "123456").await,
        Err(HandoverError::NotArmed)
    );
    hub.forget("run-h4");
}

#[tokio::test]
async fn regression_third_captcha_action_refused_and_leaving_never_gives_tries_back() {
    let page = Page::new(RECAPTCHA_PAGE, "https://shop.example/signup");
    let click = tool(&page, "browser_click");
    let open = tool(&page, "browser_open");
    let snapshot = tool(&page, "browser_snapshot");
    let context = ctx("regression-cap");
    // One own attempt, then the model leaves for an unrelated site and comes back.
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    page.show(LOGIN_PAGE, "https://example.com/");
    open.call(&context, json!({"url":"https://example.com/"}))
        .await
        .unwrap();
    snapshot.call(&context, json!({})).await.unwrap();
    page.show(RECAPTCHA_PAGE, "https://shop.example/signup");
    open.call(&context, json!({"url":"https://shop.example/signup"}))
        .await
        .unwrap();
    // Same site: the second attempt is the last, the third is refused.
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    // Even a challenge-free page of the same site seen in between gives nothing back.
    page.show(LOGIN_PAGE, "https://shop.example/account");
    snapshot.call(&context, json!({})).await.unwrap();
    page.show(RECAPTCHA_PAGE, "https://shop.example/signup?retry=1");
    for _ in 0..2 {
        let err = click.call(&context, json!({"ref":"e2"})).await.unwrap_err();
        assert!(err.to_string().contains("browser_handover"), "{err}");
    }
    // Cancelling a card cannot restore attempts either.
    handover_hub().disarm(&context.run_id);
    assert!(click.call(&context, json!({"ref":"e2"})).await.is_err());
    // Another site has its own budget.
    page.show(RECAPTCHA_PAGE, "https://other.example/signup");
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    // Only the owner's verified "Done" on that site gives it back.
    page.show(RECAPTCHA_PAGE, "https://shop.example/signup");
    tool(&page, "browser_handover")
        .call(&context, json!({"kind":"captcha"}))
        .await
        .unwrap();
    page.show(LOGIN_PAGE, "https://shop.example/signup");
    handover_hub().verify_solved(&context.run_id).await.unwrap();
    page.show(RECAPTCHA_PAGE, "https://shop.example/signup");
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn a_captcha_in_an_action_result_reaches_the_desktop_gate_and_a_moved_on_page_frees_it() {
    let _serial = serial().await;
    let page = Page::new(LOGIN_PAGE, "https://shop.example/account");
    let context = ctx("regression-desktop-fresh");
    // The browser tool submits; the CAPTCHA only shows up afterwards, never in a snapshot the
    // model asked for. The desktop tool looks at the page itself.
    tool(&page, "browser_click")
        .call(&context, json!({"ref":"e3"}))
        .await
        .unwrap();
    page.show(RECAPTCHA_PAGE, "https://shop.example/signup");
    let err = desktop_tool("desktop_click")
        .call(&context, json!({"x": 10, "y": 10}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    // The browser moves to another site: the remembered CAPTCHA of the first one does not
    // lock the desktop for the rest of the run.
    page.show(LOGIN_PAGE, "https://example.com/");
    let err = desktop_tool("desktop_click")
        .call(&context, json!({"x": 10, "y": 10}))
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("browser_handover"), "{err}");
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn regression_a_subagent_shares_the_parents_captcha_budget() {
    let page = Page::new(RECAPTCHA_PAGE, "https://shop.example/signup");
    let parent = ctx("family-parent");
    let child = ToolContext {
        lane: Some("family-child".into()),
        ..ctx("family-child")
    };
    handover_hub().link_child("family-child", "family-parent");
    let click = tool(&page, "browser_click");
    click.call(&parent, json!({"ref":"e2"})).await.unwrap();
    click.call(&child, json!({"ref":"e2"})).await.unwrap();
    // Both are spent: neither the parent nor another child gets more.
    assert!(click.call(&parent, json!({"ref":"e2"})).await.is_err());
    assert!(click.call(&child, json!({"ref":"e2"})).await.is_err());
    handover_hub().link_child("family-child-2", "family-child");
    let other = ctx("family-child-2");
    assert!(click.call(&other, json!({"ref":"e2"})).await.is_err());
    // A child ending gives nothing back; the root ending does.
    handover_hub().forget("family-child");
    assert!(click.call(&parent, json!({"ref":"e2"})).await.is_err());
    handover_hub().forget("family-child-2");
    handover_hub().forget("family-parent");
    click.call(&parent, json!({"ref":"e2"})).await.unwrap();
    handover_hub().forget("family-parent");
}

#[tokio::test]
async fn the_same_site_under_other_spellings_shares_one_budget() {
    let page = Page::new(RECAPTCHA_PAGE, "https://shop.example/signup");
    let click = tool(&page, "browser_click");
    let context = ctx("regression-variants");
    for url in [
        "https://shop.example/signup",
        "https://www.shop.example/signup",
    ] {
        page.show(RECAPTCHA_PAGE, url);
        click.call(&context, json!({"ref":"e2"})).await.unwrap();
    }
    page.show(RECAPTCHA_PAGE, "http://shop.example:8080/signup");
    assert!(click.call(&context, json!({"ref":"e2"})).await.is_err());
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn regression_the_passive_recaptcha_badge_is_not_a_challenge() {
    assert_eq!(detect(V3_PAGE), None);
    let page = Page::new(V3_PAGE, "https://shop.example/contact");
    let context = ctx("regression-v3");
    let click = tool(&page, "browser_click");
    for _ in 0..5 {
        click.call(&context, json!({"ref":"e2"})).await.unwrap();
    }
    let out = tool(&page, "browser_snapshot")
        .call(&context, json!({}))
        .await
        .unwrap();
    let ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(!text.contains("[Praxi notice]"), "{text}");
    assert!(
        tool(&page, "browser_handover")
            .call(&context, json!({"kind":"captcha"}))
            .await
            .is_err(),
        "a badge alone is nothing to hand over"
    );
    // A real v2 checkbox on the same kind of page still is a challenge.
    assert!(matches!(
        detect(RECAPTCHA_PAGE),
        Some(Challenge::Captcha { hard: false })
    ));
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn regression_a_second_done_on_an_unchanged_page_is_the_owners_confirmation() {
    let page = Page::new(RECAPTCHA_PAGE, "https://shop.example/signup");
    let context = ctx("regression-valve");
    let click = tool(&page, "browser_click");
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    tool(&page, "browser_handover")
        .call(&context, json!({"kind":"captcha"}))
        .await
        .unwrap();
    // The solved widget stays on the page (an inline iframe): the first Done cannot tell.
    assert_eq!(
        handover_hub().verify_solved(&context.run_id).await,
        Err(HandoverError::StillThere)
    );
    // The page changes a little: that is not "the same page", Done is refused again.
    page.show(
        &format!("{RECAPTCHA_PAGE}\n  text \"Solved\""),
        "https://shop.example/signup",
    );
    assert_eq!(
        handover_hub().verify_solved(&context.run_id).await,
        Err(HandoverError::StillThere)
    );
    // Same page as at the last Done: the owner confirms, the site's budget is given back.
    assert_eq!(
        handover_hub().verify_solved(&context.run_id).await,
        Ok(SolvedBy::OwnerConfirmed)
    );
    click.call(&context, json!({"ref":"e2"})).await.unwrap();
    // A blank page never passes, however often Done is pressed.
    tool(&page, "browser_handover")
        .call(&context, json!({"kind":"captcha"}))
        .await
        .unwrap();
    page.show("", "https://shop.example/signup");
    for _ in 0..3 {
        assert!(handover_hub().verify_solved(&context.run_id).await.is_err());
    }
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn regression_another_tab_cannot_purge_the_typed_code_latch() {
    let _serial = serial().await;
    let page = Page::new(CODE_PAGE, "https://bank.example/2fa");
    let context = ctx("regression-two-tabs");
    tool(&page, "browser_handover")
        .call(&context, json!({"kind":"code"}))
        .await
        .unwrap();
    *page.epoch.lock().unwrap() = Some("tabA:1".into());
    handover_hub()
        .type_code(&context.run_id, "123456")
        .await
        .unwrap();
    let driver: Arc<dyn BrowserDriver> = page.clone();
    // Tab B asks (a screenshot, an eval): that must not release tab A's latch.
    *page.epoch.lock().unwrap() = Some("tabB:1".into());
    assert!(handover_hub().capture_latched(&driver).await);
    *page.epoch.lock().unwrap() = Some("tabA:1".into());
    assert!(handover_hub().capture_latched(&driver).await);
    // Tab A moves on to a new document: released.
    *page.epoch.lock().unwrap() = Some("tabA:2".into());
    assert!(!handover_hub().capture_latched(&driver).await);
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn regression_stop_during_code_preparation_types_nothing() {
    let _serial = serial().await;
    let page = Page::new(CODE_PAGE, "https://bank.example/2fa");
    tool(&page, "browser_handover")
        .call(&ctx("regression-stop"), json!({"kind":"code"}))
        .await
        .unwrap();
    *page.stop_on_snapshot.lock().unwrap() = Some("regression-stop".into());
    assert_eq!(
        handover_hub().type_code("regression-stop", "123456").await,
        Err(HandoverError::NotArmed)
    );
    assert!(page.typed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn regression_done_rejects_blank_foreign_and_unrendered_pages() {
    let page = Page::new(RECAPTCHA_PAGE, "https://shop.example/signup");
    tool(&page, "browser_handover")
        .call(&ctx("regression-done"), json!({"kind":"captcha"}))
        .await
        .unwrap();
    for (snapshot, url) in [
        ("", "https://shop.example/signup"),
        (LOGIN_PAGE, "https://evil.example/account"),
        (
            "# Loading\n  iframe \"verification\"",
            "https://shop.example/signup",
        ),
    ] {
        page.show(snapshot, url);
        assert!(
            handover_hub()
                .verify_solved("regression-done")
                .await
                .is_err(),
            "accepted {url}: {snapshot}"
        );
    }
    handover_hub().forget("regression-done");
}

#[tokio::test]
async fn regression_screenshot_refused_after_code_is_typed() {
    let _serial = serial().await;
    let page = Page::new(CODE_PAGE, "https://bank.example/2fa");
    let context = ctx("regression-capture");
    tool(&page, "browser_handover")
        .call(&context, json!({"kind":"code"}))
        .await
        .unwrap();
    handover_hub()
        .type_code(&context.run_id, "123456")
        .await
        .unwrap();
    assert!(
        tool(&page, "browser_screenshot")
            .call(&context, json!({}))
            .await
            .is_err()
    );
    handover_hub().forget(&context.run_id);
}

fn desktop_tool(name: &str) -> Arc<dyn Tool> {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(crate::desktop::fake::FakeDesktop::new(false));
    crate::desktop::tools(
        backend,
        None,
        dir.path(),
        crate::desktop::DesktopSettings::default(),
        None,
    )
    .into_iter()
    .find(|t| t.spec().name == name)
    .unwrap()
}

#[tokio::test]
async fn regression_desktop_is_not_read_while_a_typed_code_is_on_screen() {
    let _serial = serial().await;
    let page = Page::new(CODE_PAGE, "https://bank.example/2fa");
    let context = ctx("regression-desktop-code");
    tool(&page, "browser_handover")
        .call(&context, json!({"kind":"code"}))
        .await
        .unwrap();
    handover_hub()
        .type_code(&context.run_id, "123456")
        .await
        .unwrap();
    let err = desktop_tool("desktop_get_state")
        .call(&context, json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("one-time code"), "{err}");
    // The page moved on (a navigation changes the document identity): reads work again.
    page.show(LOGIN_PAGE, "https://bank.example/home");
    desktop_tool("desktop_get_state")
        .call(&context, json!({}))
        .await
        .unwrap();
    handover_hub().forget(&context.run_id);
}

/// A pointer action ends with a fresh screenshot, except while a typed one-time code is
/// visible on the page: the same rule as `browser_screenshot`.
#[tokio::test]
async fn a_pointer_result_carries_no_screenshot_while_a_typed_code_is_visible() {
    let _serial = serial().await;
    let page = Page::new(CODE_PAGE, "https://bank.example/2fa");
    let context = ctx("pointer-latch");
    let dir = tempfile::tempdir().unwrap();
    let tools = crate::browser::tools(page.clone(), dir.path(), None);
    let get = |name: &str| {
        tools
            .iter()
            .find(|t| t.spec().name == name)
            .unwrap()
            .clone()
    };
    let click = json!({"action": "click", "x": 5, "y": 5});
    let shown = get("browser_pointer")
        .call(&context, click.clone())
        .await
        .unwrap();
    assert!(
        text_of(&shown).contains("\nscreenshot: "),
        "{}",
        text_of(&shown)
    );

    get("browser_handover")
        .call(&context, json!({"kind": "code"}))
        .await
        .unwrap();
    handover_hub()
        .type_code(&context.run_id, "123456")
        .await
        .unwrap();
    let hidden = get("browser_pointer").call(&context, click).await.unwrap();
    assert!(
        !text_of(&hidden).contains("screenshot:"),
        "{}",
        text_of(&hidden)
    );
    handover_hub().forget(&context.run_id);
}

fn text_of(out: &tool_registry::ToolOutput) -> String {
    match &out.content[0] {
        ContentPart::Text { text } => text.clone(),
        _ => panic!(),
    }
}

// ── Forms that merely embed a CAPTCHA ───────────────────────────────────────

/// A sign-up form with an hCaptcha frame beside ordinary fields.
const HCAPTCHA_FORM: &str = "# Sign up (https://forum.example/signup)\n  [e1] textbox \"Email\"\n  [e2] textbox \"Username\"\n  [e3] combobox \"Country\"\n  [e4] checkbox \"I accept the terms\"\n  iframe \"Widget containing checkbox for hCaptcha security challenge\"\n    [e5] checkbox \"hCaptcha checkbox\"\n  [e6] button \"Create account\"\n  [e7] link \"Privacy policy\"";

/// The same form with a reCAPTCHA checkbox (a simple check the bot may try).
const RECAPTCHA_FORM: &str = "# Sign up (https://shop.example/signup)\n  [e1] textbox \"Email\"\n  [e2] textbox \"Display name\"\n  [e3] textbox \"Website\"\n  [e4] textbox \"About you\"\n  iframe \"reCAPTCHA\"\n    [e5] checkbox \"I'm not a robot\"\n  [e6] button \"Create account\"";

#[tokio::test]
async fn an_hcaptcha_iframe_does_not_stop_the_form_from_being_filled() {
    let page = Page::new(HCAPTCHA_FORM, "https://forum.example/signup");
    let context = ctx("form-hcaptcha");
    let fill = tool(&page, "browser_fill");
    // Many ordinary fields, more than any own-attempt budget: all admitted.
    for _ in 0..4 {
        fill.call(&context, json!({"ref":"e1","text":"bot@agentmail.example"}))
            .await
            .unwrap();
        fill.call(&context, json!({"ref":"e2","text":"praxi_bot"}))
            .await
            .unwrap();
    }
    tool(&page, "browser_select")
        .call(&context, json!({"ref":"e3","value":"TR"}))
        .await
        .unwrap();
    let click = tool(&page, "browser_click");
    click.call(&context, json!({"ref":"e7"})).await.unwrap();
    tool(&page, "browser_press")
        .call(&context, json!({"key":"Escape"}))
        .await
        .unwrap();
    tool(&page, "browser_back")
        .call(&context, json!({}))
        .await
        .unwrap();
    assert_eq!(
        handover_hub().next_captcha_attempt(&context.run_id, Some("https://forum.example")),
        1,
        "none of that touched the check"
    );
    // The check itself is a hard one: never tried by the bot.
    let err = click.call(&context, json!({"ref":"e5"})).await.unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    // A tick box of the form could be the check, and Tab probes the focus order: not free.
    let err = click.call(&context, json!({"ref":"e4"})).await.unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    let err = tool(&page, "browser_press")
        .call(&context, json!({"key":"Tab"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    // The submit needs the solved token: refused, and the reason is plain.
    let err = click.call(&context, json!({"ref":"e6"})).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot be sent") && msg.contains("browser_handover"),
        "{msg}"
    );
    let err = tool(&page, "browser_press")
        .call(&context, json!({"key":"Enter"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    // Filling still works after the refusals.
    fill.call(&context, json!({"ref":"e1","text":"bot@agentmail.example"}))
        .await
        .unwrap();
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn filling_fields_next_to_a_recaptcha_never_spends_its_two_attempts() {
    let page = Page::new(RECAPTCHA_FORM, "https://shop.example/signup");
    let context = ctx("form-recaptcha");
    let fill = tool(&page, "browser_fill");
    for r in ["e1", "e2", "e3", "e4", "e1", "e2"] {
        fill.call(&context, json!({"ref": r, "text": "x"}))
            .await
            .unwrap();
    }
    let site = Some("https://shop.example");
    assert_eq!(
        handover_hub().next_captcha_attempt(&context.run_id, site),
        1
    );
    let click = tool(&page, "browser_click");
    // The check is attempt 1 and the submit is attempt 2: a form still goes through.
    click.call(&context, json!({"ref":"e5"})).await.unwrap();
    click.call(&context, json!({"ref":"e6"})).await.unwrap();
    // Nothing is left for a third try at the check or for sending the form again...
    assert!(click.call(&context, json!({"ref":"e5"})).await.is_err());
    assert!(click.call(&context, json!({"ref":"e6"})).await.is_err());
    // ...but the ordinary fields stay usable, so a parked page can still be left tidy.
    fill.call(&context, json!({"ref":"e1","text":"x"}))
        .await
        .unwrap();
    handover_hub().forget(&context.run_id);
}

#[tokio::test]
async fn a_pointer_click_or_a_script_on_a_captcha_page_still_counts_but_a_move_does_not() {
    let page = Page::new(RECAPTCHA_FORM, "https://shop.example/signup");
    let context = ctx("form-pointer");
    let pointer = tool(&page, "browser_pointer");
    pointer
        .call(&context, json!({"action":"move","x":10,"y":10}))
        .await
        .unwrap();
    pointer
        .call(&context, json!({"action":"scroll","x":10,"y":10,"dy":300}))
        .await
        .unwrap();
    assert_eq!(
        handover_hub().next_captcha_attempt(&context.run_id, Some("https://shop.example")),
        1
    );
    for _ in 0..2 {
        pointer
            .call(&context, json!({"action":"click","x":10,"y":10}))
            .await
            .unwrap();
    }
    let err = pointer
        .call(&context, json!({"action":"click","x":10,"y":10}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    let err = tool(&page, "browser_eval")
        .call(&context, json!({"js":"document.forms[0].submit()"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    handover_hub().forget(&context.run_id);
}

#[test]
fn only_the_check_and_the_buttons_are_gated_by_element() {
    let hub = HandoverHub::default();
    let at = |reach| {
        hub.captcha_action_at(
            "run-reach",
            Some("https://forum.example"),
            HCAPTCHA_FORM,
            reach,
        )
    };
    // Typed fields, a select list, a link, a hover on a field, leaving the page: free.
    for r in ["e1", "e2", "e3", "e7"] {
        assert!(at(Reach::Element(r)).is_ok(), "{r}");
    }
    assert!(at(Reach::Look("e2")).is_ok());
    assert!(at(Reach::Look("e7")).is_ok());
    assert!(at(Reach::Nothing).is_ok());
    // The check (inside the frame), a tick box, the submit, hovering the submit and "anywhere"
    // are not.
    assert!(at(Reach::Element("e5")).is_err());
    assert!(at(Reach::Element("e4")).is_err());
    assert!(
        at(Reach::Look("e6")).is_err(),
        "hovering the submit is probing it"
    );
    assert!(
        at(Reach::Look("e5")).is_err(),
        "pointing at the check is an action on it"
    );
    assert!(at(Reach::Element("e6")).is_err());
    assert!(at(Reach::Anywhere).is_err());
    // A ref the page does not have is treated as the worst case.
    let soft = |reach| {
        hub.captcha_action_at(
            "run-reach-soft",
            Some("https://shop.example"),
            RECAPTCHA_FORM,
            reach,
        )
    };
    assert!(soft(Reach::Element("e99")).is_ok());
    assert!(soft(Reach::Element("e99")).is_ok());
    assert!(soft(Reach::Element("e99")).is_err());
    // A page without a challenge never gates anything.
    assert!(
        hub.captcha_action_at("run-reach-none", None, LOGIN_PAGE, Reach::Element("e3"))
            .is_ok()
    );
    hub.forget("run-reach");
    hub.forget("run-reach-soft");
}

#[test]
fn a_captcha_word_in_the_address_or_a_wide_container_does_not_turn_fields_into_the_check() {
    let hub = HandoverHub::default();
    let page = "# Sign up (https://forum.example/recaptcha-demo/signup)\n  group \"Sign up, protected by reCAPTCHA\"\n    [e1] textbox \"Email\"\n  iframe \"reCAPTCHA challenge\"\n    [e2] checkbox \"I'm not a robot\"\n  [e3] textbox \"Name\"";
    assert_eq!(detect(page), Some(Challenge::Captcha { hard: false }));
    for _ in 0..5 {
        for r in ["e1", "e3"] {
            hub.captcha_action_at("run-wide", None, page, Reach::Element(r))
                .unwrap();
        }
    }
    assert!(
        hub.captcha_action_at("run-wide", None, page, Reach::Element("e2"))
            .is_ok()
    );
    hub.forget("run-wide");
}

#[tokio::test]
async fn the_notice_tells_the_model_that_filling_is_free() {
    let page = Page::new(RECAPTCHA_FORM, "https://shop.example/signup");
    let snap = tool(&page, "browser_snapshot");
    let out = snap.call(&ctx("form-notice"), json!({})).await.unwrap();
    let ContentPart::Text { text } = &out.content[0] else {
        panic!()
    };
    assert!(
        text.contains("attempt 1 of 2") && text.contains("Typing into the text fields"),
        "{text}"
    );
    handover_hub().forget("form-notice");
}

// ── Review fixes: roles, frames, keys, hovers, navigation ───────────────────

/// A page whose check is named only "Verify" and sits in frames with harmless titles.
const DISGUISED_PAGE: &str = "# Join (https://club.example/join)\n  text \"Verify you are human\"\n  [e1] textbox \"Email\"\n  [e2] link \"Help\"\n  frame \"Widget\"\n    [e3] checkbox \"Verify\"\n  iframe \"Embedded content\"\n    [e4] checkbox \"Continue\"\n    [e5] textbox \"Answer\"\n  shadow-host \"x\"\n    [e6] button \"Go\"\n  [e7] checkbox \"Remember me\"\n  [e8] radio \"Plan A\"\n  [e9] slider \"Volume\"\n  [e10] canvas \"puzzle\"\n  [e11] img \"tile\"\n  [e12] clickable \"Open\"\n  [e13] button \"Join\"\n  [e14] listbox \"Plan\"\n  [e15] searchbox \"Find\"\n  [e16] combobox \"Country\"\n  [e17] spinbutton \"Seats\"\n  [e18] textbox \"g-recaptcha-response\"";

/// Whether `reach` is counted on `page`: free actions never run out, counted ones do.
fn counted(page: &str, reach: Reach<'_>) -> bool {
    let hub = HandoverHub::default();
    (0..3)
        .map(|_| hub.captcha_action_at("run-probe", Some("https://club.example"), page, reach))
        .any(|r| r.is_err())
}

#[test]
fn the_role_decides_what_is_free_not_the_label_or_the_title_of_a_frame() {
    assert_eq!(
        detect(DISGUISED_PAGE),
        Some(Challenge::Captcha { hard: false })
    );
    // Only typed fields and links are ordinary.
    for r in ["e1", "e2", "e14", "e15", "e16", "e17"] {
        assert!(!counted(DISGUISED_PAGE, Reach::Element(r)), "{r} is free");
    }
    // A tick box called "Verify" in a frame called "Widget", the same in a frame with another
    // name, a typed answer inside a frame, a shadow host, every role that ticks, drags or
    // points, a role the budget has never heard of, the submit and a typed token field that
    // names the service are all the check.
    for r in [
        "e3", "e4", "e5", "e6", "e7", "e8", "e9", "e10", "e11", "e12", "e13", "e18",
    ] {
        assert!(counted(DISGUISED_PAGE, Reach::Element(r)), "{r} is counted");
    }
    // A ref the page does not show is the worst case, looked at or not.
    assert!(counted(DISGUISED_PAGE, Reach::Element("e99")));
    assert!(counted(DISGUISED_PAGE, Reach::Look("e99")));
}

#[test]
fn looking_at_the_check_or_the_submit_is_an_attempt_but_looking_at_a_field_is_not() {
    for r in ["e3", "e4", "e7", "e10", "e13"] {
        assert!(counted(DISGUISED_PAGE, Reach::Look(r)), "{r}");
    }
    for r in ["e1", "e2", "e14"] {
        assert!(!counted(DISGUISED_PAGE, Reach::Look(r)), "{r}");
    }
}

#[test]
fn only_closing_and_editing_keys_are_free_on_a_captcha_page() {
    use crate::browser::key_reach;
    for key in [
        "Tab",
        "Shift+Tab",
        "ArrowLeft",
        "ArrowRight",
        "Home",
        "End",
        "PageDown",
        "Enter",
        "Space",
        "a",
    ] {
        assert_eq!(key_reach(key), Reach::Anywhere, "{key}");
    }
    for key in ["Escape", "Esc", "Backspace", "Delete", "Control+Backspace"] {
        assert_eq!(key_reach(key), Reach::Nothing, "{key}");
    }
}

#[tokio::test]
async fn tabbing_and_hovering_the_submit_use_up_the_attempts_but_scroll_back_and_tab_switch_do_not()
{
    let page = Page::new(RECAPTCHA_FORM, "https://shop.example/signup");
    let context = ctx("form-probes");
    let site = Some("https://shop.example");
    let attempts = || handover_hub().next_captcha_attempt("form-probes", site);
    for _ in 0..4 {
        tool(&page, "browser_scroll")
            .call(&context, json!({"dy": 300}))
            .await
            .unwrap();
        tool(&page, "browser_back")
            .call(&context, json!({}))
            .await
            .unwrap();
        tool(&page, "browser_tabs")
            .call(&context, json!({"switch_to": 1}))
            .await
            .unwrap();
        tool(&page, "browser_press")
            .call(&context, json!({"key": "Escape"}))
            .await
            .unwrap();
        // Hovering a field is looking at it.
        let _ = tool(&page, "browser_hover")
            .call(&context, json!({"ref": "e1"}))
            .await;
    }
    assert_eq!(attempts(), 1, "none of that is an attempt");
    // Tab is one attempt, hovering the submit the second; then nothing is left to probe with.
    tool(&page, "browser_press")
        .call(&context, json!({"key": "Tab"}))
        .await
        .unwrap();
    assert_eq!(attempts(), 2);
    let _ = tool(&page, "browser_hover")
        .call(&context, json!({"ref": "e6"}))
        .await;
    assert_eq!(attempts(), 3, "the hover was admitted as an attempt");
    for call in [
        tool(&page, "browser_press")
            .call(&context, json!({"key": "Tab"}))
            .await,
        tool(&page, "browser_hover")
            .call(&context, json!({"ref": "e6"}))
            .await,
    ] {
        assert!(call.unwrap_err().to_string().contains("browser_handover"));
    }
    handover_hub().forget("form-probes");
}

#[tokio::test]
async fn dismissing_a_consent_dialog_or_selecting_text_cannot_reach_the_check_for_free() {
    let page = Page::new(RECAPTCHA_FORM, "https://shop.example/signup");
    let context = ctx("form-consent");
    let site = Some("https://shop.example");
    let attempts = || handover_hub().next_captcha_attempt("form-consent", site);
    // The driver refuses these (the test page has no dialog), but the budget answers first.
    let _ = tool(&page, "browser_dismiss_consent")
        .call(&context, json!({}))
        .await;
    assert_eq!(attempts(), 2, "the dismiss helper clicks: one attempt");
    // Selecting inside a field is free, inside the check or by text alone is not.
    let select = tool(&page, "browser_select_text");
    let _ = select.call(&context, json!({"ref": "e1"})).await;
    assert_eq!(attempts(), 2);
    let _ = select.call(&context, json!({"text": "robot"})).await;
    assert_eq!(attempts(), 3);
    let err = select
        .call(&context, json!({"ref": "e5"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    let err = tool(&page, "browser_dismiss_consent")
        .call(&context, json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    handover_hub().forget("form-consent");
}

#[test]
fn only_an_address_that_could_send_the_form_counts_as_sending_it() {
    let here = Some("https://shop.example/signup");
    for (target, sends) in [
        ("https://shop.example/signup?captcha=7x4k", true),
        ("https://www.shop.example/done?ok=1", true),
        ("https://shop.example:443/a?b", true),
        // Leaving, another page of the site without a query, or an empty query: nothing sent.
        ("https://other.example/signup?captcha=7x4k", false),
        ("https://shop.example/pricing", false),
        ("https://shop.example/pricing?", false),
        ("javascript:document.forms[0].submit()", false),
        ("about:blank", false),
        ("not a url", false),
    ] {
        assert_eq!(navigation_submits(here, target), sends, "{target}");
    }
    assert!(!navigation_submits(None, "https://shop.example/a?b=1"));
    assert!(!navigation_submits(
        Some("about:blank"),
        "https://shop.example/a?b=1"
    ));
}

#[tokio::test]
async fn an_address_that_could_send_the_captcha_form_is_an_attempt_but_leaving_is_free() {
    let page = Page::new(RECAPTCHA_FORM, "https://shop.example/signup");
    let context = ctx("form-open");
    let site = Some("https://shop.example");
    let attempts = || handover_hub().next_captcha_attempt("form-open", site);
    let open = tool(&page, "browser_open");
    // Leaving for another site, or another page without a query, spends nothing, however often.
    for url in [
        "https://other.example/signup?captcha=1",
        "https://shop.example/pricing",
        "https://other.example/",
    ] {
        for _ in 0..3 {
            open.call(&context, json!({"url": url})).await.unwrap();
        }
    }
    assert_eq!(attempts(), 1);
    // A same-site address with a query is a GET form: one attempt each, never a third.
    for n in 0..2 {
        open.call(
            &context,
            json!({"url": format!("https://shop.example/signup?captcha=guess{n}")}),
        )
        .await
        .unwrap();
    }
    let err = open
        .call(
            &context,
            json!({"url": "https://shop.example/signup?captcha=guess2"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    // A new tab follows the same rule.
    let err = tool(&page, "browser_tabs")
        .call(
            &context,
            json!({"new": true, "url": "https://shop.example/signup?captcha=guess3"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("browser_handover"), "{err}");
    // Leaving still works after the refusals.
    open.call(&context, json!({"url": "https://other.example/"}))
        .await
        .unwrap();
    handover_hub().forget("form-open");
}

#[tokio::test]
async fn script_and_data_addresses_are_refused_with_or_without_a_captcha() {
    for (run, snapshot) in [
        ("scheme-captcha", RECAPTCHA_FORM),
        ("scheme-plain", LOGIN_PAGE),
    ] {
        let page = Page::new(snapshot, "https://shop.example/signup");
        let context = ctx(run);
        for url in [
            "javascript:document.forms[0].submit()",
            " JaVaScRiPt:void(0)",
            "java\nscript:alert(1)",
            "data:text/html,<form method=post><script>document.forms[0].submit()</script>",
        ] {
            let err = tool(&page, "browser_open")
                .call(&context, json!({"url": url}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, tool_registry::ToolError::InvalidInput(_)),
                "{url}: {err}"
            );
            let err = tool(&page, "browser_tabs")
                .call(&context, json!({"new": true, "url": url}))
                .await
                .unwrap_err();
            assert!(
                matches!(err, tool_registry::ToolError::InvalidInput(_)),
                "{url}: {err}"
            );
        }
        assert_eq!(
            handover_hub().next_captcha_attempt(run, Some("https://shop.example")),
            1,
            "a refused address spends nothing"
        );
        handover_hub().forget(run);
    }
}
