//! The tool plumbing of pointer mode, state-preserving recovery and attaching to the owner's
//! browser, over the fake driver (the real-browser flow is `tests/browser_pointer.rs`).

use super::fake::FakeBrowser;
use super::*;
use tool_registry::{ContentPart, Tool};

fn build() -> (tempfile::TempDir, Arc<FakeBrowser>, Vec<Arc<dyn Tool>>) {
    let d = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBrowser::default());
    let t = tools(fake.clone(), d.path(), None);
    (d, fake, t)
}

fn get(t: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    t.iter().find(|x| x.spec().name == name).unwrap().clone()
}

fn text(o: &ToolOutput) -> String {
    match &o.content[0] {
        ContentPart::Text { text } => text.clone(),
        _ => panic!(),
    }
}

fn calls(fake: &FakeBrowser) -> Vec<String> {
    fake.calls.lock().clone()
}

#[tokio::test]
async fn a_pointer_click_reaches_the_driver_and_ends_with_a_fresh_screenshot() {
    let (d, fake, t) = build();
    let out = get(&t, "browser_pointer")
        .call(
            &ToolContext::default(),
            json!({"action": "click", "x": 412, "y": 238.5}),
        )
        .await
        .unwrap();
    let s = text(&out);
    assert!(
        calls(&fake)
            .iter()
            .any(|c| c == "pointer:Click { right: false, double: false }:Some((412.0, 238.5))"),
        "{:?}",
        calls(&fake)
    );
    // The driver's view, then a fresh picture to read the next point off.
    assert!(s.starts_with("ok pointer:"), "{s}");
    let shot = s
        .lines()
        .find_map(|l| l.strip_prefix("screenshot: "))
        .unwrap_or_else(|| panic!("no screenshot line in {s}"));
    let path = shot.split(" (").next().unwrap();
    assert!(std::path::Path::new(path).starts_with(d.path().join("screenshots")));
    assert!(std::path::Path::new(path).exists());
    assert!(shot.contains("(1000x600)"), "{shot}");
    assert!(
        s.contains("browser_pointer takes x,y in these pixels"),
        "{s}"
    );
}

#[tokio::test]
async fn pointer_typing_and_keys_need_no_point_and_bad_input_never_reaches_the_driver() {
    let (_d, fake, t) = build();
    let tool = get(&t, "browser_pointer");
    let ctx = ToolContext::default();
    tool.call(&ctx, json!({"action": "type", "text": "hello"}))
        .await
        .unwrap();
    tool.call(&ctx, json!({"action": "key", "key": "Enter"}))
        .await
        .unwrap();
    let seen = calls(&fake);
    assert!(
        seen.contains(&"pointer:Type(\"hello\"):None".to_owned()),
        "{seen:?}"
    );
    assert!(
        seen.contains(&"pointer:Key(\"Enter\"):None".to_owned()),
        "{seen:?}"
    );
    let before = seen.len();
    for bad in [
        json!({"action": "click"}),
        json!({"action": "click", "x": -4, "y": 2}),
        json!({"action": "drag", "x": 1, "y": 2}),
        json!({"action": "scroll", "x": 1, "y": 2}),
        json!({"action": "teleport", "x": 1, "y": 2}),
        // A stored secret is typed with browser_fill, with its own card on every call.
        json!({"action": "type", "text": "x", "secret": "login/github"}),
    ] {
        let err = tool.call(&ctx, bad.clone()).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad}: {err}");
    }
    assert_eq!(
        calls(&fake).len(),
        before,
        "nothing was sent to the browser"
    );
}

#[tokio::test]
async fn pointer_actions_are_page_actions_and_asked_like_a_click() {
    let (_d, _fake, t) = build();
    let spec = get(&t, "browser_pointer").spec().clone();
    assert_eq!(spec.effect, Effect::Write);
    assert_eq!(spec.category, PermissionCategory::Browser);
    // Typed or clicked content can leave the machine, like browser_click / browser_press.
    assert!(spec.content.egress && spec.content.ingests_untrusted);
    assert!(spec.description.contains("browser_screenshot"));
    assert!(spec.description.contains("browser_handover"));
    assert!(!get(&t, "browser_pointer").always_ask_for(&json!({"action":"click","x":1,"y":1})));
}

#[tokio::test]
async fn a_plain_screenshot_is_grounded_and_the_other_forms_save_the_picture_as_before() {
    let (_d, fake, t) = build();
    let shot = get(&t, "browser_screenshot");
    let ctx = ToolContext::default();
    let grounded = text(&shot.call(&ctx, json!({})).await.unwrap());
    assert!(grounded.starts_with("screenshot: "), "{grounded}");
    assert!(grounded.contains("(1000x600)"), "{grounded}");
    assert!(grounded.contains("browser_pointer takes x,y"), "{grounded}");
    // A full-page or element shot is not the picture pointer coordinates refer to.
    let before = calls(&fake)
        .iter()
        .filter(|c| *c == "grounded_screenshot")
        .count();
    let full = text(&shot.call(&ctx, json!({"full_page": true})).await.unwrap());
    assert!(full.starts_with("screenshot: "), "{full}");
    assert!(!full.contains("browser_pointer takes x,y"), "{full}");
    let after = calls(&fake)
        .iter()
        .filter(|c| *c == "grounded_screenshot")
        .count();
    assert_eq!(before, after);
}

#[tokio::test]
async fn a_failed_step_on_an_open_page_returns_its_state_and_how_to_recover_in_place() {
    let (_d, fake, t) = build();
    *fake.url.lock() = Some("https://shop.example/checkout".into());
    let click = get(&t, "browser_click");
    let ctx = ToolContext {
        run_id: "run-recovery-1".into(),
        ..ToolContext::default()
    };
    let err = click
        .call(&ctx, json!({"ref": "e404"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown ref e404"),
        "the real error stays first: {err}"
    );
    assert!(
        err.contains("page now: https://shop.example/checkout"),
        "{err}"
    );
    // The state is read from the browser (interactive elements), plus a screenshot.
    assert!(err.contains("ok snapshot:true"), "{err}");
    assert!(err.contains("screenshot: "), "{err}");
    assert!(err.contains("[Praxi notice] The step failed"), "{err}");
    assert!(err.contains("Do NOT call browser_open"), "{err}");
    assert!(err.contains("browser_pointer"), "{err}");
    // The page is never reopened by recovery.
    assert!(
        !calls(&fake).iter().any(|c| c.starts_with("open:")),
        "{:?}",
        calls(&fake)
    );
    // The same step failing again escalates, then tells the owner / blocks the item.
    let second = click
        .call(&ctx, json!({"ref": "e404"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        second.contains("do not send it again unchanged"),
        "{second}"
    );
    let third = click
        .call(&ctx, json!({"ref": "e404"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(third.contains("tell the owner"), "{third}");
    // A step that works ends the streak.
    click.call(&ctx, json!({"ref": "e1"})).await.unwrap();
    let fresh = click
        .call(&ctx, json!({"ref": "e404"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(!fresh.contains("in a row"), "{fresh}");
}

#[tokio::test]
async fn recovery_is_only_for_steps_that_failed_on_an_open_page() {
    let (_d, fake, t) = build();
    let ctx = ToolContext {
        run_id: "run-recovery-2".into(),
        ..ToolContext::default()
    };
    // No page open (the driver cannot say where it is): the plain error, no state to keep.
    let err = get(&t, "browser_click")
        .call(&ctx, json!({"ref": "e404"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("[Praxi notice]"), "{err}");
    *fake.url.lock() = Some("https://shop.example/".into());
    // Bad input is not a failed step.
    let err = get(&t, "browser_click")
        .call(&ctx, json!({"wrong": 1}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err}");
    // Reading and navigating are not steps on the page.
    let err = get(&t, "browser_open")
        .call(&ctx, json!({"url": "http://localhost/"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("page now:"), "{err}");
}

#[tokio::test]
async fn attaching_names_the_owners_browser_on_every_result_and_marks_it_private() {
    let (_d, fake, t) = build();
    let ctx = ToolContext::default();
    // Nothing says "owner's browser" while the assistant's own is in use.
    let own = get(&t, "browser_snapshot")
        .call(&ctx, json!({}))
        .await
        .unwrap();
    assert!(!text(&own).contains("[browser:"), "{}", text(&own));
    assert!(own.exposure.is_none());

    let attach = get(&t, "browser_attach");
    // Never a debugging port that is not on this computer, before anything is asked.
    for remote in ["http://192.168.1.2:9222", "http://example.com:9222"] {
        let err = attach
            .call(&ctx, json!({"endpoint": remote}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{remote}: {err}");
    }
    assert!(fake.attached.lock().is_none());
    let out = text(
        &attach
            .call(&ctx, json!({"endpoint": "http://127.0.0.1:9333"}))
            .await
            .unwrap(),
    );
    assert!(out.contains("attached to the owner's own browser"), "{out}");
    assert!(out.contains("127.0.0.1:9333"), "{out}");
    assert_eq!(
        fake.attached.lock().as_deref(),
        Some("http://127.0.0.1:9333")
    );

    // Now every browser result names the browser (the timeline shows which one is in use) and
    // counts as the owner's private data, even a plain read.
    let snap = get(&t, "browser_snapshot")
        .call(&ctx, json!({}))
        .await
        .unwrap();
    let s = text(&snap);
    assert!(
        s.ends_with("[browser: the OWNER's own browser, attached at http://127.0.0.1:9333; what it shows is private to the owner]"),
        "{s}"
    );
    let exposure = snap
        .exposure
        .expect("the owner's browser is private content");
    assert!(exposure.reads_private && exposure.ingests_untrusted);
    assert_eq!(exposure.label, "signed-in browser");

    // The line comes before the live-view line, which stays last.
    *fake.watched.lock() = Some(true);
    let s = text(
        &get(&t, "browser_snapshot")
            .call(&ctx, json!({}))
            .await
            .unwrap(),
    );
    assert!(
        s.ends_with("[view: visible to the user in the live view]"),
        "{s}"
    );
    assert!(s.contains("[browser: the OWNER's own browser"), "{s}");

    // Back to the assistant's own browser.
    let out = text(&attach.call(&ctx, json!({"detach": true})).await.unwrap());
    assert!(out.contains("detached"), "{out}");
    let own = get(&t, "browser_snapshot")
        .call(&ctx, json!({}))
        .await
        .unwrap();
    assert!(!text(&own).contains("[browser:"), "{}", text(&own));
}

#[tokio::test]
async fn every_attach_asks_the_owner_and_the_card_says_what_it_opens_up() {
    let (_d, _fake, t) = build();
    let attach = get(&t, "browser_attach");
    assert!(attach.always_ask_for(&json!({})));
    assert!(attach.always_ask_for(&json!({"endpoint": "http://127.0.0.1:9222"})));
    assert!(!attach.always_ask_for(&json!({"detach": true})));
    let line = attach
        .egress_extra(
            &ToolContext::default(),
            &json!({"endpoint": "http://127.0.0.1:9222"}),
        )
        .unwrap();
    assert!(
        line.contains("owner's own browser") && line.contains("signed in"),
        "{line}"
    );
    assert!(
        attach
            .egress_extra(&ToolContext::default(), &json!({"detach": true}))
            .is_none()
    );
    assert_eq!(attach.spec().category, PermissionCategory::Browser);
    assert!(attach.spec().content.egress && attach.spec().content.reads_private);
}

#[tokio::test]
async fn a_pointer_action_is_judged_by_the_page_it_acts_on_and_stops_when_the_page_moved() {
    let (_d, fake, t) = build();
    *fake.url.lock() = Some("https://shop.example/checkout".into());
    let pointer = get(&t, "browser_pointer");
    let input = json!({"action": "click", "x": 10, "y": 10});
    // Site rules and the origin an approval is bound to are the page's own.
    let origin = pointer
        .current_origin(&ToolContext::default(), &input)
        .await;
    assert_eq!(origin.as_deref(), Some("https://shop.example:443"));
    // Approved on one site, the page is now on another: nothing is sent.
    let moved = ToolContext {
        expected_origin: Some("https://bank.example:443".into()),
        ..ToolContext::default()
    };
    let err = pointer.call(&moved, input).await.unwrap_err().to_string();
    assert!(err.contains("no longer on"), "{err}");
    assert!(
        !calls(&fake).iter().any(|c| c.starts_with("pointer:")),
        "{:?}",
        calls(&fake)
    );
}

#[test]
fn a_click_that_changed_nothing_says_so_and_points_at_the_pointer() {
    let quiet = browser_cdp::SnapshotDiff {
        unchanged: 12,
        ..browser_cdp::SnapshotDiff::default()
    };
    let note = silent_click_note(&quiet).expect("nothing changed");
    assert!(
        note.contains("browser_pointer") && note.contains("screenshot"),
        "{note}"
    );
    for changed in [
        browser_cdp::SnapshotDiff {
            added: vec!["[e3] checkbox \"Agree\" checked".into()],
            ..browser_cdp::SnapshotDiff::default()
        },
        browser_cdp::SnapshotDiff {
            url_changed: Some("https://a.example/next".into()),
            ..browser_cdp::SnapshotDiff::default()
        },
        browser_cdp::SnapshotDiff {
            title_changed: Some("Next".into()),
            ..browser_cdp::SnapshotDiff::default()
        },
    ] {
        assert!(silent_click_note(&changed).is_none());
    }
}
