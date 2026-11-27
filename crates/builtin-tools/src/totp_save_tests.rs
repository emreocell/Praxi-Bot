//! `account_totp_save`: the host reads the two-factor setup key off the page, so the
//! key is never model text. Each test names the guard it protects.

use std::sync::Arc;

use parking_lot::Mutex;
use secret_store::{FillSecrets, MemoryStore, NewAccount, Redactor};
use serde_json::json;
use tool_registry::{ContentPart, Tool, ToolContext, ToolOutput};

use crate::browser::fake::FakeBrowser;
use crate::browser::{self, ACCOUNT_TOTP_SAVE};

const SITE: &str = "https://example.com";
const BOT_A: &str = "bot-a";
const BOT_B: &str = "bot-b";
const ACCOUNT: &str = "login/example.com-aaaa1111";
/// RFC 6238 appendix B SHA-1 key, base32.
const KEY: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

fn ctx(bot: &str) -> ToolContext {
    ToolContext {
        bot_id: Some(bot.to_owned()),
        ..ToolContext::default()
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    secrets: Arc<FillSecrets>,
    redactor: Arc<Mutex<Redactor>>,
    fake: Arc<FakeBrowser>,
    tools: Vec<Arc<dyn Tool>>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let redactor = Arc::new(Mutex::new(Redactor::new()));
    let sink = Arc::clone(&redactor);
    let secrets = Arc::new(
        FillSecrets::in_memory(Arc::new(MemoryStore::new()))
            .with_value_hook(Arc::new(move |v| sink.lock().add_secret(v))),
    );
    secrets
        .register_account(&NewAccount {
            name: ACCOUNT,
            site: SITE,
            login_id: "bot@example.com",
            email_account: None,
            bot_id: BOT_A,
            note: None,
            password_len: 20,
            symbols: true,
        })
        .unwrap();
    secrets
        .upsert("login/owner-made", Some("x"), None, None)
        .unwrap();
    let fake = Arc::new(FakeBrowser::default());
    *fake.url.lock() = Some(format!("{SITE}/settings/2fa"));
    let tools =
        browser::tools_with_secrets(fake.clone(), dir.path(), None, Some(Arc::clone(&secrets)));
    Rig {
        _dir: dir,
        secrets,
        redactor,
        fake,
        tools,
    }
}

fn tool(r: &Rig, name: &str) -> Arc<dyn Tool> {
    r.tools
        .iter()
        .find(|t| t.spec().name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
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

fn page_has(r: &Rig, key: &str, text: &str, frame: &str, top: &str) {
    r.fake.reads.lock().insert(
        key.to_owned(),
        (
            text.to_owned(),
            Some(frame.to_owned()),
            Some(top.to_owned()),
        ),
    );
}

fn grouped(text: &str, size: usize, sep: &str) -> String {
    text.as_bytes()
        .chunks(size)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(sep)
}

/// Guard (finding 1): the key is read from the page by the host. The call carries only a
/// ref, the card and the result never carry the key, the redactor learns it, the key is
/// not in the backend call log, and a code can be typed afterwards.
#[tokio::test]
async fn the_key_goes_from_the_page_into_the_vault_without_passing_through_the_model() {
    let r = rig();
    page_has(
        &r,
        "ref:e9",
        &format!("Setup key: {}", grouped(KEY, 4, " ")),
        SITE,
        SITE,
    );
    let save = tool(&r, ACCOUNT_TOTP_SAVE);
    assert!(
        save.always_ask_for(&json!({"name": ACCOUNT, "ref": "e9"})),
        "every save is confirmed, except where the Full level waives it"
    );
    let input = json!({"name": ACCOUNT, "ref": "e9"});
    let card = save
        .approval_details(&ctx(BOT_A), &input)
        .await
        .unwrap()
        .expect("a card");
    assert_eq!(card["kind"], "account_totp_save");
    assert_eq!(card["secret"], ACCOUNT);
    assert_eq!(card["replaces_existing"], false);
    assert!(
        r.fake
            .calls
            .lock()
            .iter()
            .all(|c| !c.contains("read_value")),
        "nothing is read before the owner agrees"
    );
    let out = save.call(&ctx(BOT_A), input).await.unwrap();
    let shown = format!("{card}{}", text(&out));
    for spelling in [
        KEY.to_owned(),
        grouped(KEY, 4, " "),
        KEY.to_ascii_lowercase(),
    ] {
        assert!(!shown.contains(&spelling), "{shown}");
    }
    assert!(shown.contains(&format!("totp:{ACCOUNT}")), "{shown}");
    let log = r.fake.calls.lock().join("\n");
    assert!(log.contains("read_value:ref:e9"), "{log}");
    assert!(!log.contains(&KEY[..12]), "{log}");
    assert!(r.secrets.accounts(Some(BOT_A)).unwrap()[0].has_totp);
    // The redactor now masks every spelling of the key in any later output.
    for spelling in [
        KEY.to_owned(),
        KEY.to_ascii_lowercase(),
        grouped(KEY, 4, " "),
        grouped(KEY, 4, "-"),
        format!("otpauth://totp/Example:me?secret={KEY}&issuer=Example"),
    ] {
        let out = r.redactor.lock().redact(&format!("key {spelling} end"));
        assert!(!out.contains(&KEY[..8]), "{spelling} -> {out}");
    }
    // A second save is shown as a replacement.
    let card = save
        .approval_details(&ctx(BOT_A), &json!({"name": ACCOUNT, "ref": "e9"}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card["replaces_existing"], true);
    // The code can now be typed into a field of the account's own site, never read back.
    r.fake.targets.lock().insert(
        "e5".into(),
        browser::FieldTarget {
            name: "Code".into(),
            role: "textbox".into(),
            frame_origin: Some(SITE.into()),
            top_origin: Some(SITE.into()),
            sensitive: false,
        },
    );
    let fill = tool(&r, "browser_fill");
    let out = fill
        .call(
            &ctx(BOT_A),
            json!({"ref": "e5", "secret": format!("totp:{ACCOUNT}")}),
        )
        .await
        .unwrap();
    let typed = r.fake.typed_secrets.lock().clone();
    assert_eq!(typed.len(), 1);
    assert!(!text(&out).contains(&typed[0]));
}

/// Guard (finding 1): the old way, key text in the model's input, is gone: the schema has
/// no such field and the call refuses it without echoing the text.
#[tokio::test]
async fn a_key_in_the_models_input_is_refused_and_never_echoed() {
    let r = rig();
    let save = tool(&r, ACCOUNT_TOTP_SAVE);
    let props = save.spec().input_schema["properties"].to_string();
    assert!(
        !props.contains("setup") && !props.contains("secret"),
        "{props}"
    );
    let typed = json!({"name": ACCOUNT, "setup": KEY});
    for err in [
        save.approval_details(&ctx(BOT_A), &typed)
            .await
            .unwrap_err(),
        save.call(&ctx(BOT_A), typed.clone()).await.unwrap_err(),
    ] {
        assert!(!err.to_string().contains(&KEY[..10]), "{err}");
    }
    // With a ref AND key text, the extra field still fails the call.
    let both = json!({"name": ACCOUNT, "ref": "e9", "setup": KEY});
    assert!(save.call(&ctx(BOT_A), both).await.is_err());
    // Neither or both of ref / selector: refused.
    for bad in [
        json!({"name": ACCOUNT}),
        json!({"name": ACCOUNT, "ref": "e9", "selector": "#k"}),
    ] {
        assert!(save.call(&ctx(BOT_A), bad).await.is_err());
    }
    assert!(!r.secrets.accounts(None).unwrap()[0].has_totp);
}

/// Guard: a CSS selector reads the top page the same way.
#[tokio::test]
async fn a_selector_is_read_the_same_way() {
    let r = rig();
    page_has(&r, "sel:#setup-key", KEY, SITE, SITE);
    let save = tool(&r, ACCOUNT_TOTP_SAVE);
    save.call(
        &ctx(BOT_A),
        json!({"name": ACCOUNT, "selector": "#setup-key"}),
    )
    .await
    .unwrap();
    assert!(r.secrets.accounts(Some(BOT_A)).unwrap()[0].has_totp);
}

/// Guard: the element, the page and the account must agree. A frame of another site, a
/// page of another site, another bot, the owner's own secret: nothing is read or saved.
#[tokio::test]
async fn nothing_is_read_for_the_wrong_page_bot_or_account() {
    let r = rig();
    let save = tool(&r, ACCOUNT_TOTP_SAVE);
    let evil = "https://evil.example";
    // The element sits in a frame of another site inside the account's page.
    page_has(&r, "ref:e1", KEY, evil, SITE);
    // The whole page is another site.
    page_has(&r, "ref:e2", KEY, SITE, evil);
    page_has(&r, "ref:e3", KEY, SITE, SITE);
    let mine = |r: &str| json!({"name": ACCOUNT, "ref": r});
    for el in ["e1", "e2"] {
        assert!(save.call(&ctx(BOT_A), mine(el)).await.is_err(), "{el}");
    }
    assert!(!r.secrets.accounts(None).unwrap()[0].has_totp);
    // The browser is on another site: refused before a card is offered, and before a read.
    *r.fake.url.lock() = Some(format!("{evil}/2fa"));
    assert!(
        save.approval_details(&ctx(BOT_A), &mine("e3"))
            .await
            .is_err()
    );
    assert!(save.call(&ctx(BOT_A), mine("e3")).await.is_err());
    *r.fake.url.lock() = Some(format!("{SITE}/2fa"));
    let reads_before = r
        .fake
        .calls
        .lock()
        .iter()
        .filter(|c| c.contains("read_value"))
        .count();
    // Another bot, no bot, the owner's own secret.
    for who in [Some(BOT_B), None] {
        let c = ToolContext {
            bot_id: who.map(str::to_owned),
            ..ToolContext::default()
        };
        assert!(save.approval_details(&c, &mine("e3")).await.is_err());
        assert!(save.call(&c, mine("e3")).await.is_err());
    }
    let owner = json!({"name": "login/owner-made", "ref": "e3"});
    assert!(save.approval_details(&ctx(BOT_A), &owner).await.is_err());
    assert!(save.call(&ctx(BOT_A), owner).await.is_err());
    let reads_after = r
        .fake
        .calls
        .lock()
        .iter()
        .filter(|c| c.contains("read_value"))
        .count();
    assert_eq!(reads_before, reads_after, "a refused call read the page");
    assert!(!r.secrets.accounts(None).unwrap()[0].has_totp);
}

/// Guard: an element that does not hold a key (a label, a hint) saves nothing and is
/// not echoed, whatever the page put in it.
#[tokio::test]
async fn an_element_without_a_key_saves_nothing_and_is_not_echoed() {
    let r = rig();
    let save = tool(&r, ACCOUNT_TOTP_SAVE);
    for junk in [
        "Enter the 6-digit code from your app, secret-phrase: tulip",
        "",
        "otpauth://hotp/x?secret=GEZDGNBVGY3TQOJQ",
    ] {
        page_has(&r, "ref:e7", junk, SITE, SITE);
        let err = save
            .call(&ctx(BOT_A), json!({"name": ACCOUNT, "ref": "e7"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(junk.is_empty() || !err.contains(junk), "{err}");
    }
    assert!(!r.secrets.accounts(None).unwrap()[0].has_totp);
}

/// Guard (finding 1, the page side): a key the page shows is masked in what a browser
/// tool returns to the model, before anything was saved.
#[tokio::test]
async fn a_key_on_the_page_is_masked_in_what_the_model_reads() {
    let r = rig();
    let wait = tool(&r, "browser_wait");
    for shown in [
        KEY.to_owned(),
        grouped(KEY, 4, " "),
        grouped(KEY, 4, "-").to_ascii_lowercase(),
        format!("otpauth://totp/Example:me?secret={KEY}&issuer=Example"),
    ] {
        let out = wait
            .call(&ctx(BOT_A), json!({"text": shown}))
            .await
            .unwrap();
        let out = text(&out);
        assert!(
            !out.contains(&KEY[..8])
                && !out
                    .to_ascii_lowercase()
                    .contains(&KEY[..8].to_ascii_lowercase()),
            "{shown} -> {out}"
        );
        assert!(out.contains("REDACTED"), "{out}");
    }
}

/// Without a vault there is no such tool.
#[tokio::test]
async fn the_tool_exists_only_with_a_vault() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBrowser::default());
    let tools = browser::tools_with_secrets(fake, dir.path(), None, None);
    assert!(tools.iter().all(|t| t.spec().name != ACCOUNT_TOTP_SAVE));
}
