//! The `secret` parameter of the fill tools: allow-list binding at the LIVE
//! receiving field, no value in any output, per-call ask, exact standing
//! grants. Each test names the guard it protects; removing that guard makes it
//! fail (checked by hand for every one, see the PR description).

use std::sync::Arc;

use parking_lot::Mutex;
use secret_store::{FillSecrets, MemoryStore, SecretError, SecretStore, SecretValue};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError, ToolOutput};

use crate::browser::fake::FakeBrowser;
use crate::browser::{self, FieldTarget};
use crate::desktop::fake::FakeDesktop;
use crate::desktop::{self, DesktopSettings};

const VALUE: &str = "hunter2-Correct-Horse-9!";
const GITHUB: &str = "https://github.com";

/// A store that remembers which names were read, so a test can prove a value
/// was (or was not) fetched.
struct SpyStore {
    inner: MemoryStore,
    reads: Mutex<Vec<String>>,
}

impl SecretStore for SpyStore {
    fn get(&self, name: &str) -> Result<Option<SecretValue>, SecretError> {
        self.reads.lock().push(name.to_owned());
        self.inner.get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.inner.set(name, value)
    }
    fn delete(&self, name: &str) -> Result<bool, SecretError> {
        self.inner.delete(name)
    }
    fn list_names(&self) -> Result<Vec<String>, SecretError> {
        self.inner.list_names()
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    store: Arc<SpyStore>,
    secrets: Arc<FillSecrets>,
    fake: Arc<FakeBrowser>,
    tools: Vec<Arc<dyn Tool>>,
}

fn field(frame: Option<&str>, top: Option<&str>) -> FieldTarget {
    FieldTarget {
        name: "Password".into(),
        role: "textbox".into(),
        frame_origin: frame.map(str::to_owned),
        top_origin: top.map(str::to_owned),
        sensitive: false,
    }
}

fn rig_with(origins: &[&str], frame: Option<&str>, top: Option<&str>, echo: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SpyStore {
        inner: MemoryStore::new(),
        reads: Mutex::new(Vec::new()),
    });
    let secrets = Arc::new(FillSecrets::in_memory(store.clone()));
    let origins: Vec<String> = origins.iter().map(|o| (*o).to_owned()).collect();
    secrets
        .upsert("login/github", Some(VALUE), Some(&origins), None)
        .unwrap();
    store.reads.lock().clear();
    let fake = Arc::new(FakeBrowser {
        echo_value: echo,
        ..FakeBrowser::default()
    });
    fake.targets.lock().insert("e5".into(), field(frame, top));
    let tools =
        browser::tools_with_secrets(fake.clone(), dir.path(), None, Some(Arc::clone(&secrets)));
    Rig {
        _dir: dir,
        store,
        secrets,
        fake,
        tools,
    }
}

fn rig() -> Rig {
    rig_with(&[GITHUB], Some(GITHUB), Some(GITHUB), false)
}

fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.spec().name == name)
        .unwrap()
        .clone()
}

fn text(o: &ToolOutput) -> String {
    o.content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn fill_secret_input() -> Value {
    json!({"ref": "e5", "secret": "login/github"})
}

fn value_was_read(r: &Rig) -> bool {
    r.store
        .reads
        .lock()
        .iter()
        .any(|n| n == "fill/login/github")
}

/// Guard: the value is typed, but never returned, even when the page echoes it.
/// Fails without `scrub` in `fill_with_secret`.
#[tokio::test]
async fn a_secret_fill_types_the_value_and_never_returns_it() {
    let r = rig_with(&[GITHUB], Some(GITHUB), Some(GITHUB), true);
    let out = tool(&r.tools, "browser_fill")
        .call(&ToolContext::default(), fill_secret_input())
        .await
        .unwrap();
    assert_eq!(r.fake.typed_secrets.lock().as_slice(), [VALUE]);
    // Positive control: the page really echoed the value.
    assert!(r.fake.raw_secret_output.lock().contains(VALUE));
    assert!(!text(&out).contains(VALUE), "{}", text(&out));
    assert!(!text(&out).contains("Correct-Horse"), "{}", text(&out));
    // The call log never saw the value either.
    assert!(!r.fake.calls.lock().iter().any(|c| c.contains(VALUE)));
}

/// A page that reports the typed value in its failure.
struct Leaky(Arc<FakeBrowser>);
#[async_trait::async_trait]
impl browser::BrowserDriver for Leaky {
    async fn field_target(&self, r: &str) -> Result<FieldTarget, String> {
        self.0.field_target(r).await
    }
    async fn fill_secret(
        &self,
        _r: &str,
        text: &str,
        _accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
    ) -> Result<String, String> {
        Err(format!("input handler threw: {text}"))
    }
    async fn open(&self, _: &str, _: Option<&str>) -> Result<String, String> {
        unreachable!()
    }
    async fn snapshot(&self, _: bool) -> Result<String, String> {
        unreachable!()
    }
    async fn click(&self, _: &str) -> Result<String, String> {
        unreachable!()
    }
    async fn fill(&self, _: &str, _: &str) -> Result<String, String> {
        unreachable!()
    }
    async fn select(&self, _: &str, _: &str) -> Result<String, String> {
        unreachable!()
    }
    async fn press(&self, _: &str) -> Result<String, String> {
        unreachable!()
    }
    async fn scroll(&self, _: Option<&str>, _: f64, _: f64) -> Result<String, String> {
        unreachable!()
    }
    async fn wait(
        &self,
        _: browser::Wait,
        _: Option<std::time::Duration>,
    ) -> Result<String, String> {
        unreachable!()
    }
    async fn back(&self) -> Result<String, String> {
        unreachable!()
    }
    async fn tabs(&self, _: Option<usize>) -> Result<String, String> {
        unreachable!()
    }
    async fn screenshot(
        &self,
        _: bool,
        _: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        unreachable!()
    }
    async fn eval(&self, _: &str) -> Result<Value, String> {
        unreachable!()
    }
}

/// Guard: the error of a failed typing is scrubbed too.
#[tokio::test]
async fn a_failed_secret_fill_error_does_not_carry_the_value() {
    let r = rig();
    let dir = tempfile::tempdir().unwrap();
    let t = browser::tools_with_secrets(
        Arc::new(Leaky(r.fake.clone())),
        dir.path(),
        None,
        Some(Arc::clone(&r.secrets)),
    );
    let err = tool(&t, "browser_fill")
        .call(&ToolContext::default(), fill_secret_input())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("input handler threw"), "{err}");
    assert!(!err.to_string().contains("Correct-Horse"), "{err}");
}

/// Guard: the approval card names the secret, the field and the FRAME origin,
/// and never contains the value. Fails if `approval_details` drops a fact.
#[tokio::test]
async fn the_card_names_secret_field_and_frame_origin_but_not_the_value() {
    let r = rig();
    let d = tool(&r.tools, "browser_fill")
        .approval_details(&ToolContext::default(), &fill_secret_input())
        .await
        .unwrap()
        .expect("a secret fill has a card");
    assert_eq!(d["secret"], "login/github");
    assert_eq!(d["field"], "Password");
    assert_eq!(d["frame_origin"], GITHUB);
    assert_eq!(d["frame_differs_from_top"], false);
    assert!(!d.to_string().contains(VALUE));
    // Preparing the card must not read the value.
    assert!(!value_was_read(&r));
}

/// Guard: an embedded frame is checked by ITS origin and called out. A page the
/// owner allowed that embeds a foreign frame must not receive the secret in it.
#[tokio::test]
async fn a_cross_origin_iframe_is_checked_by_its_own_origin() {
    // Allowed top page, foreign iframe: refused, no card.
    let r = rig_with(&[GITHUB], Some("https://evil.test"), Some(GITHUB), false);
    let err = tool(&r.tools, "browser_fill")
        .approval_details(&ToolContext::default(), &fill_secret_input())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not allowed on https://evil.test"),
        "{err}"
    );
    let err = tool(&r.tools, "browser_fill")
        .call(&ToolContext::default(), fill_secret_input())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
    assert!(r.fake.typed_secrets.lock().is_empty());
    assert!(!value_was_read(&r));

    // Allowed frame inside a NON-allowed top-level page: refused, no card.
    let r = rig_with(&[GITHUB], Some(GITHUB), Some("https://shop.test"), false);
    let t = tool(&r.tools, "browser_fill");
    assert!(
        t.approval_details(&ToolContext::default(), &fill_secret_input())
            .await
            .is_err()
    );
    assert!(
        t.call(&ToolContext::default(), fill_secret_input())
            .await
            .is_err()
    );
    assert!(r.fake.typed_secrets.lock().is_empty());
    assert!(!value_was_read(&r));

    // Both origins allowed but different: a card naming both is offered, and
    // a standing grant for the frame origin does NOT apply to the embed.
    let gist = "https://gist.github.com";
    let r = rig_with(&[GITHUB, gist], Some(GITHUB), Some(gist), false);
    r.secrets.grant_create("login/github", GITHUB).unwrap();
    let t = tool(&r.tools, "browser_fill");
    let d = t
        .approval_details(&ToolContext::default(), &fill_secret_input())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d["frame_origin"], GITHUB);
    assert_eq!(d["top_origin"], gist);
    assert_eq!(d["frame_differs_from_top"], true);
    assert!(
        t.standing_grant(&ToolContext::default(), &fill_secret_input())
            .await
            .is_none()
    );
}

/// Guard: a field on any other origin is refused BEFORE a card exists and
/// before the value is read. Fails if the `check_browser` call is removed from
/// `secret_fill_facts` or from `fill_with_secret`.
#[tokio::test]
async fn a_wrong_origin_is_refused_before_any_card_and_nothing_is_typed() {
    for live in [
        Some("https://github.com.evil.test"),
        Some("http://github.com"),
        Some("https://github.com:8443"),
        Some("https://evil.test"),
        None,
    ] {
        let r = rig_with(&[GITHUB], live, live, false);
        let ctx = ToolContext::default();
        let t = tool(&r.tools, "browser_fill");
        assert!(
            t.approval_details(&ctx, &fill_secret_input())
                .await
                .is_err(),
            "{live:?}: no card may be offered"
        );
        assert!(
            t.call(&ctx, fill_secret_input()).await.is_err(),
            "{live:?}: no fill"
        );
        assert!(r.fake.typed_secrets.lock().is_empty(), "{live:?}");
        assert!(!value_was_read(&r), "{live:?}: the value must not be read");
    }
}

/// Guard: the frame origin is re-read from the browser right before insertion;
/// a navigation after the card makes the call type nothing.
#[tokio::test]
async fn the_origin_is_rechecked_at_insertion() {
    let r = rig();
    *r.fake.origin_at_insert.lock() = Some(Some("https://evil.test".into()));
    let err = tool(&r.tools, "browser_fill")
        .call(&ToolContext::default(), fill_secret_input())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
    assert!(r.fake.typed_secrets.lock().is_empty());
}

/// Guard: exactly one of text | secret.
#[tokio::test]
async fn exactly_one_of_text_or_secret() {
    let r = rig();
    let t = tool(&r.tools, "browser_fill");
    let ctx = ToolContext::default();
    for bad in [
        json!({"ref": "e5"}),
        json!({"ref": "e5", "text": "a", "secret": "login/github"}),
        json!({"ref": "e5", "secret": ""}),
    ] {
        assert!(matches!(
            t.call(&ctx, bad.clone()).await,
            Err(ToolError::InvalidInput(_))
        ));
        assert!(t.approval_details(&ctx, &bad).await.is_err(), "{bad}");
    }
    // Ordinary text is untouched: never masked, no card details, no ask.
    assert!(
        t.approval_details(&ctx, &json!({"ref":"e5","text":"hello"}))
            .await
            .unwrap()
            .is_none()
    );
    assert!(!t.always_ask_for(&json!({"ref":"e5","text":"hello"})));
    let out = t
        .call(&ctx, json!({"ref":"e5","text":"hello"}))
        .await
        .unwrap();
    assert_eq!(text(&out), "ok fill:e5:hello");
}

/// Guard: reserved namespaces cannot be referenced through `secret`: the store
/// entry is never read, whatever the model writes.
#[tokio::test]
async fn reserved_namespaces_are_never_read_through_the_secret_parameter() {
    let r = rig();
    r.store
        .inner
        .set("composio/gmail", "composio-token-1234")
        .unwrap();
    r.store.inner.set("mcp/x/oauth", "mcp-token-12345").unwrap();
    r.store.inner.set("openai", "provider-key-12345").unwrap();
    let t = tool(&r.tools, "browser_fill");
    let ctx = ToolContext::default();
    for name in [
        "composio/gmail",
        "mcp/x/oauth",
        "openai",
        "fill/login/github",
        "../composio/gmail",
        "login/github/../../composio/gmail",
        "FILL/login/github",
    ] {
        let input = json!({"ref": "e5", "secret": name});
        assert!(t.approval_details(&ctx, &input).await.is_err(), "{name}");
        assert!(t.call(&ctx, input).await.is_err(), "{name}");
    }
    assert!(r.fake.typed_secrets.lock().is_empty());
    let reads = r.store.reads.lock().clone();
    assert!(
        !reads
            .iter()
            .any(|n| n.contains("composio") || n.contains("mcp") || n == "openai"),
        "{reads:?}"
    );
}

/// Guard: no secrets service, no secret fill.
#[tokio::test]
async fn without_a_secret_service_a_secret_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBrowser::default());
    fake.targets
        .lock()
        .insert("e5".into(), field(Some(GITHUB), Some(GITHUB)));
    let t = browser::tools(fake.clone(), dir.path(), None);
    let err = tool(&t, "browser_fill")
        .call(&ToolContext::default(), fill_secret_input())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("no secrets are configured"),
        "{err}"
    );
    assert!(fake.typed_secrets.lock().is_empty());
}

/// Guard: a driver that cannot report the frame (the trait default) can never
/// receive a secret.
#[tokio::test]
async fn a_driver_that_cannot_report_the_frame_never_gets_a_secret() {
    let r = rig();
    // `e9` has no recorded target: field_target fails.
    let input = json!({"ref": "e9", "secret": "login/github"});
    let err = tool(&r.tools, "browser_fill")
        .call(&ToolContext::default(), input)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown ref"), "{err}");
    assert!(r.fake.typed_secrets.lock().is_empty());
}

/// Guard: typing a secret always asks (per call), text never does.
#[test]
fn only_a_secret_fill_is_always_ask() {
    let r = rig();
    let fill = tool(&r.tools, "browser_fill");
    assert!(fill.always_ask_for(&fill_secret_input()));
    assert!(!fill.always_ask_for(&json!({"ref":"e5","text":"x"})));
    for other in ["browser_click", "browser_press", "browser_select"] {
        assert!(
            !tool(&r.tools, other).always_ask_for(&json!({"secret":"x"})),
            "{other}"
        );
    }
}

/// Guard: a standing grant covers exactly its secret + origin + tool kind.
#[tokio::test]
async fn a_standing_grant_matches_only_its_exact_secret_origin_and_tool() {
    let r = rig();
    let ctx = ToolContext::default();
    let fill = tool(&r.tools, "browser_fill");
    // No grant yet.
    assert!(
        fill.standing_grant(&ctx, &fill_secret_input())
            .await
            .is_none()
    );
    let grant = r.secrets.grant_create("login/github", GITHUB).unwrap();
    let g = fill
        .standing_grant(&ctx, &fill_secret_input())
        .await
        .expect("the exact secret on the exact origin is covered");
    assert_eq!(g.id, grant.id);
    assert!(!g.label.contains(VALUE));

    // Another origin (the live field moved) does not match.
    r.secrets
        .upsert(
            "login/github",
            None,
            Some(&[GITHUB.to_owned(), "https://gitlab.com".to_owned()]),
            None,
        )
        .unwrap();
    r.fake.targets.lock().insert(
        "e5".into(),
        field(Some("https://gitlab.com"), Some("https://gitlab.com")),
    );
    assert!(
        fill.standing_grant(&ctx, &fill_secret_input())
            .await
            .is_none()
    );
    r.fake
        .targets
        .lock()
        .insert("e5".into(), field(Some(GITHUB), Some(GITHUB)));

    // Another secret does not match.
    r.secrets
        .upsert(
            "login/other",
            Some("other-value-123"),
            Some(&[GITHUB.to_owned()]),
            None,
        )
        .unwrap();
    assert!(
        fill.standing_grant(&ctx, &json!({"ref":"e5","secret":"login/other"}))
            .await
            .is_none()
    );
    // Another field type (text, or a different tool) does not match.
    assert!(
        fill.standing_grant(&ctx, &json!({"ref":"e5","text":"x"}))
            .await
            .is_none()
    );
    for other in ["browser_click", "browser_select", "browser_press"] {
        assert!(
            tool(&r.tools, other)
                .standing_grant(&ctx, &fill_secret_input())
                .await
                .is_none(),
            "{other}"
        );
    }
    // Revoking removes it.
    assert!(r.secrets.grant_revoke(&grant.id).unwrap());
    assert!(
        fill.standing_grant(&ctx, &fill_secret_input())
            .await
            .is_none()
    );
}

/// Guard: the model sees names and allowed places only, sanitised and capped.
#[tokio::test]
async fn secrets_list_shows_names_and_places_never_values() {
    let r = rig();
    r.secrets
        .upsert(
            "zeta/long",
            Some("zzz-value-12345"),
            Some(&["https://z.test".into()]),
            None,
        )
        .unwrap();
    let list = crate::fill_secret::secrets_list_tool(Arc::clone(&r.secrets));
    assert!(!list.always_ask_for(&json!({})));
    let out = text(&list.call(&ToolContext::default(), json!({})).await.unwrap());
    assert!(out.contains("login/github: https://github.com"), "{out}");
    assert!(out.contains("zeta/long: https://z.test"), "{out}");
    assert!(!out.contains(VALUE) && !out.contains("zzz-value"), "{out}");
    // Deterministic: same bytes twice.
    let again = text(&list.call(&ToolContext::default(), json!({})).await.unwrap());
    assert_eq!(out, again);
    // The listing read no value.
    assert!(r.store.reads.lock().is_empty());
}

// ------------------------------------------------------------------ desktop

/// What a desktop test needs: the data dir guard, the fake backend, the secrets
/// service and the tools built over them.
type DesktopRig = (
    tempfile::TempDir,
    Arc<FakeDesktop>,
    Arc<FillSecrets>,
    Vec<Arc<dyn Tool>>,
);

fn desktop_rig(identity: Option<&str>, allowed: &[&str]) -> DesktopRig {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let secrets = Arc::new(FillSecrets::in_memory(store));
    let programs: Vec<String> = allowed.iter().map(|p| (*p).to_owned()).collect();
    secrets
        .upsert("app/vpn", Some(VALUE), Some(&[]), Some(&programs))
        .unwrap();
    let fake = Arc::new(FakeDesktop::new(false));
    *fake.identity.lock() = identity.map(str::to_owned);
    let tools = desktop::tools_dynamic_with_secrets(
        Some(fake.clone()),
        None,
        dir.path(),
        DesktopSettings {
            action_settle: std::time::Duration::ZERO,
            launch_settle: std::time::Duration::ZERO,
        },
        None,
        Some(Arc::clone(&secrets)),
    );
    (dir, fake, secrets, tools)
}

async fn observe(t: &[Arc<dyn Tool>], ctx: &ToolContext) {
    tool(t, "desktop_get_state")
        .call(ctx, json!({}))
        .await
        .unwrap();
}

/// A bot-owned account is typed into a desktop program only by its own bot,
/// even when the owner allowed that program for it. Fails without the bot scope
/// inside `FillSecrets::check_desktop` / `resolve_value_for`.
#[tokio::test]
async fn another_bots_account_is_not_typed_into_a_desktop_program() {
    let (_d, fake, secrets, t) = desktop_rig(Some("win:sha256:abc"), &["win:sha256:abc"]);
    secrets
        .register_account(&secret_store::NewAccount {
            name: "login/acct-a",
            site: GITHUB,
            login_id: "a@example.org",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
    secrets
        .upsert(
            "login/acct-a",
            None,
            None,
            Some(&["win:sha256:abc".to_owned()]),
        )
        .unwrap();
    let input = json!({"index": 3, "gen": 1, "secret": "login/acct-a"});
    let ctx_for = |bot: Option<&str>| ToolContext {
        run_id: "run-d".into(),
        bot_id: bot.map(str::to_owned),
        ..ToolContext::default()
    };
    observe(&t, &ctx_for(None)).await;
    let ty = tool(&t, "desktop_type");
    for bot in [Some("bot-b"), None] {
        let ctx = ctx_for(bot);
        let e = ty.approval_details(&ctx, &input).await.unwrap_err();
        assert!(e.to_string().contains("belongs to another bot"), "{e}");
        assert!(ty.call(&ctx, input.clone()).await.is_err());
    }
    assert!(fake.typed.lock().is_empty());
    ty.call(&ctx_for(Some("bot-a")), input).await.unwrap();
    assert_eq!(fake.typed.lock().len(), 20);
}

/// Guard: a desktop secret is typed only into a program with a verified
/// identity on the allow-list; none supplied / another identity -> refused
/// before a card; the card names window and program; the output is scrubbed.
#[tokio::test]
async fn a_desktop_secret_needs_a_verified_allow_listed_program() {
    let ctx = ToolContext {
        run_id: "run-d".into(),
        ..ToolContext::default()
    };
    let input = json!({"index": 3, "gen": 1, "secret": "app/vpn"});

    // The backend cannot verify a program: always refused.
    let (_d, fake, _s, t) = desktop_rig(None, &["win:sha256:abc"]);
    observe(&t, &ctx).await;
    let ty = tool(&t, "desktop_type");
    assert!(ty.always_ask_for(&input));
    let err = ty.approval_details(&ctx, &input).await.unwrap_err();
    assert!(err.to_string().contains("cannot verify"), "{err}");
    assert!(ty.call(&ctx, input.clone()).await.is_err());
    assert!(fake.typed.lock().is_empty());

    // Another verified program: refused.
    let (_d, fake, _s, t) = desktop_rig(Some("win:sha256:other"), &["win:sha256:abc"]);
    observe(&t, &ctx).await;
    assert!(
        tool(&t, "desktop_type")
            .call(&ctx, input.clone())
            .await
            .is_err()
    );
    assert!(
        tool(&t, "desktop_type")
            .approval_details(&ctx, &input)
            .await
            .is_err()
    );
    assert!(fake.typed.lock().is_empty());

    // The allow-listed program: a card with window + program, then the value is
    // typed and the refreshed tree (which shows it) is scrubbed.
    let (_d, fake, _s, t) = desktop_rig(Some("win:sha256:abc"), &["win:sha256:abc"]);
    observe(&t, &ctx).await;
    let ty = tool(&t, "desktop_type");
    let card = ty.approval_details(&ctx, &input).await.unwrap().unwrap();
    assert_eq!(card["secret"], "app/vpn");
    assert_eq!(card["program"], "win:sha256:abc");
    assert!(!card.to_string().contains(VALUE));
    let out = text(&ty.call(&ctx, input.clone()).await.unwrap());
    assert_eq!(fake.typed.lock().as_str(), VALUE);
    assert!(!out.contains("Correct-Horse"), "{out}");
    assert!(
        out.contains("[REDACTED]") || !out.contains("hunter2"),
        "{out}"
    );
    // A desktop program never has a standing grant.
    assert!(ty.standing_grant(&ctx, &input).await.is_none());

    // set_value takes the same path.
    let (_d, fake, _s, t) = desktop_rig(Some("win:sha256:abc"), &["win:sha256:abc"]);
    observe(&t, &ctx).await;
    let sv = tool(&t, "desktop_set_value");
    let sv_in = json!({"index": 3, "gen": 1, "secret": "app/vpn"});
    assert!(sv.always_ask_for(&sv_in));
    let out = text(&sv.call(&ctx, sv_in).await.unwrap());
    assert_eq!(fake.typed.lock().as_str(), VALUE);
    assert!(!out.contains("Correct-Horse"), "{out}");
    // Both given / none given is an input error.
    assert!(
        sv.call(
            &ctx,
            json!({"index":3,"gen":1,"value":"a","secret":"app/vpn"})
        )
        .await
        .is_err()
    );
    assert!(sv.call(&ctx, json!({"index":3,"gen":1})).await.is_err());
}

/// A bot-owned account is typed only by its own bot. Fails without the
/// `check_bot` calls in the browser tool.
#[tokio::test]
async fn another_bots_account_is_not_typed() {
    let r = rig();
    r.secrets
        .register_account(&secret_store::NewAccount {
            name: "login/acct-a",
            site: GITHUB,
            login_id: "a@example.org",
            email_account: None,
            bot_id: "bot-a",
            note: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
    let input = json!({"ref": "e5", "secret": "login/acct-a"});
    let fill = tool(&r.tools, "browser_fill");
    let other = ToolContext {
        bot_id: Some("bot-b".into()),
        ..ToolContext::default()
    };
    assert!(fill.call(&other, input.clone()).await.is_err());
    assert!(fill.approval_details(&other, &input).await.is_err());
    assert!(r.fake.typed_secrets.lock().is_empty());
    let owner = ToolContext {
        bot_id: Some("bot-a".into()),
        ..ToolContext::default()
    };
    fill.call(&owner, input).await.unwrap();
    assert_eq!(r.fake.typed_secrets.lock().len(), 1);
}
