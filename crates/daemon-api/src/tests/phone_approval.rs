//! Phone approval of money asks (ADR `docs/design/phone-money-approval-adr.md`): the link is
//! single-use, bound to one money ask, expires, and never works without the PIN; a chat
//! channel alone can never approve money.

use super::*;

const ADDRESS: &str = "https://pay.example.org";
const PIN: &str = "246810";

/// A tool that moves money: the money floor asks on every level.
struct Pay;

#[async_trait]
impl Tool for Pay {
    fn spec(&self) -> &ToolSpec {
        static SPEC: std::sync::OnceLock<ToolSpec> = std::sync::OnceLock::new();
        SPEC.get_or_init(|| ToolSpec {
            name: "pay_now".to_owned(),
            description: "pays".to_owned(),
            input_schema: json!({"type": "object"}),
            effect: Effect::Write,
            category: PermissionCategory::Money,
            platforms: vec![Platform::All],
            content: tool_registry::ContentClass::none(),
        })
    }
    async fn call(&self, _: &ToolContext, _: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("paid"))
    }
    async fn checkout_facts(
        &self,
        _: &ToolContext,
        _: &Value,
    ) -> Option<tool_registry::CheckoutFacts> {
        Some(tool_registry::CheckoutFacts {
            domain: Some("shop.example".into()),
            merchant: Some("<b>Evil</b> Shop".into()),
            amount: Some("€12.00".into()),
            screenshot: Some((SHOT.to_vec(), "image/png")),
        })
    }
}

/// The checkout screenshot the fake payment tool reports.
const SHOT: &[u8] = b"\x89PNG-checkout-pixels";

fn setup_money() -> T {
    let mut config = LoopConfig::default();
    config.legacy_tool_surface = true;
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let tools = ToolRegistry::new();
    tools.register(Arc::new(Danger));
    tools.register(Arc::new(Pay));
    let agent = AgentLoop::new(
        Arc::clone(&storage),
        tools,
        Providers::single(Arc::new(FakeProvider)),
        config,
    );
    let state = AppState::new(agent.clone(), storage, DaemonConfig::new(TOKEN)).unwrap();
    state.set_owner_key(OWNER_KEY.into());
    T {
        app: router(state.clone()),
        state,
        agent,
        _dir: dir,
    }
}

async fn turn_on(t: &T) {
    let r = call(
        t,
        Method::PUT,
        "/v1/phone-approval",
        Some(json!({"address": format!("{ADDRESS}/"), "pin": PIN})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.json()["enabled"], true);
    assert_eq!(r.json()["address"], ADDRESS);
}

/// Start a run whose tool call asks; returns `(call_id, run_id)`.
async fn ask(t: &T, tool: &str) -> (String, String) {
    let (_, chat) = make_chat(t).await;
    send_msg(t, &chat, &format!("call:{tool} {{}}"), "queue").await;
    pending_call(t).await
}

/// Mint a link; returns the nonce.
async fn mint(t: &T, call_id: &str) -> String {
    let r = call(
        t,
        Method::POST,
        "/v1/phone-approval/links",
        Some(json!({"call_id": call_id})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let url = r.json()["url"].as_str().unwrap().to_owned();
    let nonce = url
        .strip_prefix(&format!("{ADDRESS}/v1/phone-approval/p/"))
        .unwrap_or_else(|| panic!("{url}"))
        .to_owned();
    assert_eq!(nonce.len(), 43, "32 random bytes");
    nonce
}

/// A phone browser request: no bearer, an optional form body, the page's own origin.
async fn phone(t: &T, method: Method, path: &str, form: Option<&str>, origin: &str) -> Resp {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:47821")
        .header("accept-language", "en-US,en;q=0.9");
    if !origin.is_empty() {
        b = b.header("origin", origin);
    }
    let req = match form {
        Some(f) => b
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(f.to_owned())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let resp = t.app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    Resp {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn text(r: &Resp) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

async fn unlock(t: &T, nonce: &str, pin: &str) -> Resp {
    phone(
        t,
        Method::POST,
        &format!("/v1/phone-approval/p/{nonce}/unlock"),
        Some(&format!("pin={pin}")),
        ADDRESS,
    )
    .await
}

async fn decide(t: &T, nonce: &str, confirm: &str, decision: &str) -> Resp {
    phone(
        t,
        Method::POST,
        &format!("/v1/phone-approval/p/{nonce}/decide"),
        Some(&format!("confirm={confirm}&decision={decision}")),
        ADDRESS,
    )
    .await
}

fn confirm_of(r: &Resp) -> String {
    let page = text(r);
    let start = page
        .find("name=confirm value=\"")
        .unwrap_or_else(|| panic!("{page}"))
        + "name=confirm value=\"".len();
    page[start..].split('"').next().unwrap().to_owned()
}

async fn approval_state(t: &T, run: &str, call_id: &str) -> Value {
    let r = call(t, Method::GET, &format!("/v1/approvals?run_id={run}"), None).await;
    r.json()
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["call_id"] == call_id)
        .map(|a| a["decision"].clone())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_payment_is_approved_from_the_phone_once_with_the_pin() {
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, run) = ask(&t, "pay_now").await;
    let nonce = mint(&t, &call_id).await;

    let r = phone(
        &t,
        Method::GET,
        &format!("/v1/phone-approval/p/{nonce}"),
        None,
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    let page = text(&r);
    assert!(
        page.contains("Approve a payment") && page.contains("pay_now"),
        "{page}"
    );
    assert!(!page.contains("decision"), "no decision before the PIN");
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(r.headers["referrer-policy"], "same-origin");
    assert!(
        r.headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );

    let r = unlock(&t, &nonce, PIN).await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    let confirm = confirm_of(&r);
    let r = decide(&t, &nonce, &confirm, "approve").await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    assert!(text(&r).contains("Approved"));
    wait_state(&t, &run, "succeeded").await;
    assert_eq!(approval_state(&t, &run, &call_id).await, "approved");

    // Replay: the link was used up.
    let r = decide(&t, &nonce, &confirm, "approve").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = unlock(&t, &nonce, PIN).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_phone_can_deny_a_payment() {
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, run) = ask(&t, "pay_now").await;
    let nonce = mint(&t, &call_id).await;
    let confirm = confirm_of(&unlock(&t, &nonce, PIN).await);
    let r = decide(&t, &nonce, &confirm, "deny").await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    assert!(text(&r).contains("Denied"));
    wait_state(&t, &run, "succeeded").await;
    assert_eq!(approval_state(&t, &run, &call_id).await, "denied");
}

#[tokio::test]
async fn a_chat_channel_alone_can_never_approve_money() {
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, run) = ask(&t, "pay_now").await;
    let (k, v) = auth();
    // The Telegram bridge holds the owner key and marks its requests: approving money is refused.
    for scope in ["once", "this_chat", "always"] {
        let r = send(
            &t.app,
            Method::POST,
            &format!("/v1/approvals/{call_id}"),
            Some(json!({"decision": "approve", "scope": scope})),
            &[(k, v.as_str()), ("x-praxi-channel", "telegram")],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{scope}");
        assert_eq!(
            r.json()["error"]["code"],
            "phone_approval_required",
            "{scope}"
        );
    }
    assert!(
        approval_state(&t, &run, &call_id).await.is_null(),
        "still pending"
    );
    // The link alone (what Telegram carries) cannot decide: the decision needs the PIN step.
    let nonce = mint(&t, &call_id).await;
    for confirm in ["", "AAAA", &"A".repeat(43)] {
        let nonce = if confirm.is_empty() {
            nonce.clone()
        } else {
            mint(&t, &call_id).await
        };
        let r = decide(&t, &nonce, confirm, "approve").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{confirm}");
        // The failed press burned the link: the right PIN no longer opens it.
        let r = unlock(&t, &nonce, PIN).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{confirm}");
    }
    assert!(
        approval_state(&t, &run, &call_id).await.is_null(),
        "still pending"
    );
    // The desktop app (owner key, no channel mark) still decides as before.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "once"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
}

#[tokio::test]
async fn a_channel_still_decides_an_ordinary_card() {
    let t = setup_money();
    let (call_id, _) = ask(&t, "danger_run").await;
    let (k, v) = auth();
    let r = send(
        &t.app,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "approve", "scope": "once"})),
        &[(k, v.as_str()), ("x-praxi-channel", "telegram")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
}

#[tokio::test]
async fn wrong_nonces_wrong_pins_and_expired_links_are_refused() {
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, run) = ask(&t, "pay_now").await;
    // A nonce nobody minted, and malformed ones.
    let fake = "B".repeat(43);
    for n in [fake.as_str(), "short", "%3Cscript%3E"] {
        let r = phone(
            &t,
            Method::GET,
            &format!("/v1/phone-approval/p/{n}"),
            None,
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{n}");
        assert_eq!(
            unlock(&t, n, PIN).await.status,
            StatusCode::NOT_FOUND,
            "{n}"
        );
    }
    // Wrong PINs: three per link, then the link is burned even for the right PIN.
    let nonce = mint(&t, &call_id).await;
    let r = unlock(&t, &nonce, "111111").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        text(&r).contains("Tries left on this link: 2"),
        "{}",
        text(&r)
    );
    assert!(!text(&r).contains("name=confirm"));
    assert_eq!(
        unlock(&t, &nonce, "1234567").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        unlock(&t, &nonce, "abc").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(unlock(&t, &nonce, PIN).await.status, StatusCode::NOT_FOUND);
    // Expired: ten minutes after minting the link is dead.
    let nonce = mint(&t, &call_id).await;
    t.state.phone_book().lock().skew_ms = crate::phone_approval::LINK_TTL_MS;
    let r = phone(
        &t,
        Method::GET,
        &format!("/v1/phone-approval/p/{nonce}"),
        None,
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(unlock(&t, &nonce, PIN).await.status, StatusCode::NOT_FOUND);
    t.state.phone_book().lock().skew_ms = 0;
    // Unlocked, then expired before the press.
    let nonce = mint(&t, &call_id).await;
    let confirm = confirm_of(&unlock(&t, &nonce, PIN).await);
    t.state.phone_book().lock().skew_ms = crate::phone_approval::LINK_TTL_MS;
    assert_eq!(
        decide(&t, &nonce, &confirm, "approve").await.status,
        StatusCode::NOT_FOUND
    );
    t.state.phone_book().lock().skew_ms = 0;
    // A newer link replaces the older one for the same ask.
    let old = mint(&t, &call_id).await;
    let new = mint(&t, &call_id).await;
    assert_eq!(unlock(&t, &old, PIN).await.status, StatusCode::NOT_FOUND);
    assert_eq!(unlock(&t, &new, PIN).await.status, StatusCode::OK);
    assert!(
        approval_state(&t, &run, &call_id).await.is_null(),
        "still pending"
    );
}

#[tokio::test]
async fn five_wrong_pins_lock_phone_approval() {
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, _) = ask(&t, "pay_now").await;
    let a = mint(&t, &call_id).await;
    for _ in 0..3 {
        unlock(&t, &a, "000000").await;
    }
    let b = mint(&t, &call_id).await;
    unlock(&t, &b, "000000").await;
    let r = unlock(&t, &b, "000000").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", text(&r));
    let r = call(
        &t,
        Method::POST,
        "/v1/phone-approval/links",
        Some(json!({"call_id": call_id})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "phone_approval_locked");
    let r = call(&t, Method::GET, "/v1/phone-approval", None).await;
    assert!(r.json()["locked_until_ms"].as_i64().is_some());
}

#[tokio::test]
async fn links_are_minted_only_for_pending_money_asks_by_the_owner() {
    let t = setup_money();
    let (call_id, run) = ask(&t, "danger_run").await;
    let body = || Some(json!({"call_id": call_id}));
    // Off: no link.
    let r = call(&t, Method::POST, "/v1/phone-approval/links", body()).await;
    assert_eq!(r.json()["error"]["code"], "phone_approval_off");
    turn_on(&t).await;
    // Not a money ask.
    let r = call(&t, Method::POST, "/v1/phone-approval/links", body()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "not_a_money_ask");
    // Not the owner.
    let r = call_token_only(&t, Method::POST, "/v1/phone-approval/links", body()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "owner_key_required");
    for (m, b) in [
        (Method::GET, None),
        (
            Method::PUT,
            Some(json!({"address": ADDRESS, "pin": "999999"})),
        ),
        (Method::DELETE, None),
    ] {
        let r = call_token_only(&t, m.clone(), "/v1/phone-approval", b).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m}");
    }
    // Not pending.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "deny"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    wait_state(&t, &run, "succeeded").await;
    let r = call(&t, Method::POST, "/v1/phone-approval/links", body()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_link_dies_with_its_ask_and_with_a_new_pin() {
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, _) = ask(&t, "pay_now").await;
    // Changing the PIN drops every open link.
    let nonce = mint(&t, &call_id).await;
    let r = call(
        &t,
        Method::PUT,
        "/v1/phone-approval",
        Some(json!({"address": ADDRESS, "pin": "13579135"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        unlock(&t, &nonce, "13579135").await.status,
        StatusCode::NOT_FOUND
    );
    // The old PIN no longer works, the new one does.
    let nonce = mint(&t, &call_id).await;
    assert_eq!(unlock(&t, &nonce, PIN).await.status, StatusCode::FORBIDDEN);
    let confirm = confirm_of(&unlock(&t, &nonce, "13579135").await);
    // Decided in the app meanwhile: the phone link is dead.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{call_id}"),
        Some(json!({"decision": "deny"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        decide(&t, &nonce, &confirm, "approve").await.status,
        StatusCode::NOT_FOUND
    );
    // Turned off: the settings say so and the PIN file is gone.
    let r = call(&t, Method::DELETE, "/v1/phone-approval", None).await;
    assert_eq!(r.json()["enabled"], false);
    assert!(!t.state.data_dir().join("phone-approval.json").exists());
}

#[tokio::test]
async fn the_page_accepts_only_its_own_origin_and_the_pin_is_never_shown() {
    let t = setup_money();
    turn_on(&t).await;
    let stored = std::fs::read_to_string(t.state.data_dir().join("phone-approval.json")).unwrap();
    assert!(
        stored.contains("$argon2id$") && !stored.contains(PIN),
        "{stored}"
    );
    let r = call(&t, Method::GET, "/v1/phone-approval", None).await;
    assert!(!r.json().to_string().contains("argon2"), "{:?}", r.json());
    let (call_id, _) = ask(&t, "pay_now").await;
    let nonce = mint(&t, &call_id).await;
    let r = phone(
        &t,
        Method::POST,
        &format!("/v1/phone-approval/p/{nonce}/unlock"),
        Some(&format!("pin={PIN}")),
        "https://evil.example",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // The bearer-free exemption covers the page routes only.
    let r = phone(&t, Method::GET, "/v1/phone-approval", None, "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = phone(
        &t,
        Method::POST,
        "/v1/phone-approval/links",
        Some("call_id=x"),
        ADDRESS,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "foreign origin on an API route"
    );
    // The link still works for its owner.
    assert_eq!(unlock(&t, &nonce, PIN).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_checkout_shows_its_facts_and_the_screenshot_only_after_the_pin() {
    use base64::Engine as _;
    let t = setup_money();
    turn_on(&t).await;
    let (call_id, run) = ask(&t, "pay_now").await;
    let cid = protocol::ToolCallId(call_id.parse().unwrap());
    // The card carries the facts (the app shows them too); the screenshot stays in memory.
    let r = call(
        &t,
        Method::GET,
        &format!("/v1/approvals?run_id={run}"),
        None,
    )
    .await;
    let card = r.json()[0]["details"].clone();
    assert_eq!(card["money_movement"], true, "{card}");
    assert_eq!(card["risk"], "high", "a payment is never a low-risk card");
    assert_eq!(card["checkout"]["domain"], "shop.example");
    assert_eq!(card["checkout"]["amount"], "€12.00");
    assert!(!card.to_string().contains("PNG"), "{card}");
    assert!(t.agent.checkout_screenshot(cid).is_some());

    let shot = base64::engine::general_purpose::STANDARD.encode(SHOT);
    let nonce = mint(&t, &call_id).await;
    let page = text(
        &phone(
            &t,
            Method::GET,
            &format!("/v1/phone-approval/p/{nonce}"),
            None,
            "",
        )
        .await,
    );
    assert!(
        page.contains("shop.example") && page.contains("€12.00"),
        "{page}"
    );
    // Page text is escaped: a merchant name cannot inject markup.
    assert!(
        page.contains("&lt;b&gt;Evil&lt;/b&gt; Shop") && !page.contains("<b>Evil"),
        "{page}"
    );
    assert!(
        !page.contains(&shot) && !page.contains("data:image"),
        "no screenshot before the PIN"
    );
    // A wrong PIN does not show it either.
    let wrong = text(&unlock(&t, &nonce, "111111").await);
    assert!(
        !wrong.contains(&shot) && !wrong.contains("data:image"),
        "{wrong}"
    );
    let r = unlock(&t, &nonce, PIN).await;
    assert!(
        text(&r).contains(&format!("data:image/png;base64,{shot}")),
        "{}",
        text(&r)
    );
    let confirm = confirm_of(&r);
    decide(&t, &nonce, &confirm, "deny").await;
    wait_state(&t, &run, "succeeded").await;
    // The screenshot is dropped with the ask.
    assert!(t.agent.checkout_screenshot(cid).is_none());
}

#[tokio::test]
async fn a_link_is_bound_to_its_own_ask() {
    let t = setup_money();
    turn_on(&t).await;
    let (_, chat) = make_chat(&t).await;
    send_msg(&t, &chat, "call:pay_now {}", "queue").await;
    let (first, first_run) = pending_call(&t).await;
    let nonce = mint(&t, &first).await;
    let confirm = confirm_of(&unlock(&t, &nonce, PIN).await);
    // The first ask is decided in the app; a second payment waits now. The old link and its
    // confirm value do not carry over to it.
    let r = call(
        &t,
        Method::POST,
        &format!("/v1/approvals/{first}"),
        Some(json!({"decision": "deny"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    wait_state(&t, &first_run, "succeeded").await;
    send_msg(&t, &chat, "call:pay_now {}", "queue").await;
    let (second, run) = pending_call(&t).await;
    assert_ne!(first, second);
    assert_eq!(
        decide(&t, &nonce, &confirm, "approve").await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        approval_state(&t, &run, &second).await.is_null(),
        "still pending"
    );
    // A confirm value from one link does not open another link.
    let a = mint(&t, &second).await;
    let confirm = confirm_of(&unlock(&t, &a, PIN).await);
    let b = mint(&t, &second).await;
    assert_eq!(
        decide(&t, &b, &confirm, "approve").await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        approval_state(&t, &run, &second).await.is_null(),
        "still pending"
    );
}
