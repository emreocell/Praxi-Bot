//! Integration tests that need an installed Chrome/Edge. Run with:
//! `cargo test -p browser-cdp -- --ignored --test-threads=1`

use std::time::Duration;

use base64::Engine as _;
use browser_cdp::{
    Browser, DialogOptions, LaunchOptions, Page, ScreenshotOptions, ScreenshotTarget,
    SnapshotOptions, StepGuard, WaitCondition,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod common;

fn data_url(html: &str) -> String {
    format!(
        "data:text/html;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html)
    )
}

fn ref_of(snap: &browser_cdp::Snapshot, role: &str, name: &str) -> String {
    snap.nodes
        .iter()
        .find(|n| n.role == role && n.name == name)
        .and_then(|n| n.ref_id.clone())
        .unwrap_or_else(|| panic!("no {role} \"{name}\" in:\n{}", snap.render()))
}

const FORM: &str = r#"<html><head><title>Form</title></head><body>
<h1>Demo</h1>
<label>Email <input id=email type=text></label>
<label>Password <input id=pw type=password value="s3cret"></label>
<label>Country <select id=c><option value=tr>Turkey</option><option value=de>Germany</option></select></label>
<label><input id=cb type=checkbox> Agree</label>
<button id=go onclick="document.getElementById('out').textContent='clicked:'+document.getElementById('email').value+':'+document.getElementById('c').value">Go</button>
<div id=pointer style="cursor:pointer" onclick="document.getElementById('out').textContent='div-clicked'">Fancy div</div>
<p id=out>idle</p>
<div style="height:3000px"></div>
<button id=bottom onclick="document.getElementById('out').textContent='bottom'">Bottom</button>
</body></html>"#;

async fn open(b: &Browser, html: &str) -> Page {
    b.new_page(&data_url(html)).await.expect("open page")
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn checkout_detection_uses_visible_payment_fields() {
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open(
        &browser,
        r#"<input name="email" type="email"><input id="number" autocomplete="cc-number" hidden>"#,
    )
    .await;
    assert!(!page.has_payment_form().await.unwrap());
    page.evaluate("document.querySelector('#number').hidden = false")
        .await
        .unwrap();
    assert!(page.has_payment_form().await.unwrap());
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn checkout_detection_reads_child_frames_and_any_payment_provider_frame() {
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    // A card field one frame down (srcdoc frames share the page's process).
    let nested = open(
        &browser,
        r#"<iframe srcdoc="<input autocomplete='cc-number'>" width=300 height=60></iframe>"#,
    )
    .await;
    assert!(nested.has_payment_form().await.unwrap());
    // The card field itself is two frames down.
    let deeper = open(
        &browser,
        r#"<iframe srcdoc="<iframe srcdoc=&quot;<input autocomplete='cc-csc'>&quot; width=200 height=40></iframe>" width=300 height=80></iframe>"#,
    )
    .await;
    assert!(deeper.has_payment_form().await.unwrap());
    // A provider other than Stripe: the frame's host is the signal, its page need not load.
    for host in [
        "www.paypal.com",
        "checkoutshopper-live.adyen.com",
        "assets.braintreegateway.com",
        "web.squarecdn.com",
        "x.klarna.com",
    ] {
        let page = open(
            &browser,
            &format!(r#"<iframe src="https://{host}/frame" width=300 height=60></iframe>"#),
        )
        .await;
        assert!(page.has_payment_form().await.unwrap(), "{host}");
    }
    let plain = open(
        &browser,
        r#"<iframe srcdoc="<p>hello</p>" width=300 height=60></iframe><iframe src="https://example.org/embed" width=100 height=40></iframe>"#,
    )
    .await;
    assert!(!plain.has_payment_form().await.unwrap());
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn form_flow_fill_select_click_diff() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(&b, FORM).await;
    let opts = SnapshotOptions {
        compact: true,
        ..Default::default()
    };
    let snap = p.snapshot(&opts).await.unwrap();
    let text = snap.render();
    // Password value must never leak.
    assert!(!text.contains("s3cret"), "{text}");
    assert!(text.contains("[masked]"), "{text}");
    // cursor:pointer + onclick div is picked up although AX has no role for it.
    assert!(
        text.contains("clickable \"Fancy div\"") || text.contains("Fancy div"),
        "{text}"
    );

    let email = ref_of(&snap, "textbox", "Email");
    let country = ref_of(&snap, "combobox", "Country");
    let go = ref_of(&snap, "button", "Go");
    assert!(p.fill(&email, "a@b.c").await.unwrap().ok);
    assert!(p.select_option(&country, "Germany").await.unwrap().ok);
    let r = p.click(&go).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert!(
        r.new_snapshot_diff.render().contains("clicked:a@b.c:de"),
        "{}",
        r.new_snapshot_diff.render()
    );
    assert_eq!(
        p.evaluate("document.getElementById('out').textContent")
            .await
            .unwrap(),
        "clicked:a@b.c:de"
    );

    // Cursor-interactive div.
    let fancy = r
        .snapshot
        .nodes
        .iter()
        .find(|n| n.name == "Fancy div" && n.ref_id.is_some())
        .and_then(|n| n.ref_id.clone())
        .expect("fancy div ref");
    p.click(&fancy).await.unwrap();
    assert_eq!(
        p.evaluate("document.getElementById('out').textContent")
            .await
            .unwrap(),
        "div-clicked"
    );

    // Off-screen element: click scrolls into view first.
    let bottom = ref_of(&p.snapshot(&opts).await.unwrap(), "button", "Bottom");
    assert!(p.click(&bottom).await.unwrap().ok);
    assert_eq!(
        p.evaluate("document.getElementById('out').textContent")
            .await
            .unwrap(),
        "bottom"
    );

    // Keyboard.
    let snap = p.snapshot(&opts).await.unwrap();
    let email = ref_of(&snap, "textbox", "Email");
    p.click(&email).await.unwrap();
    p.press_key("Control+a").await.unwrap();
    p.press_key("Backspace").await.unwrap();
    assert_eq!(
        p.evaluate("document.getElementById('email').value")
            .await
            .unwrap(),
        ""
    );
    p.type_text(&email, "xyz").await.unwrap();
    assert_eq!(
        p.evaluate("document.getElementById('email').value")
            .await
            .unwrap(),
        "xyz"
    );

    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn navigation_invalidates_refs_and_history_works() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        "<title>One</title><a href='#x'>jump</a><button>B1</button>",
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let b1 = ref_of(&snap, "button", "B1");
    let url2 = data_url("<title>Two</title><button>B2</button>");
    let r = p.navigate(&url2).await.unwrap();
    assert!(r.ok);
    assert_eq!(r.snapshot.title, "Two");
    let stale = p.click(&b1).await.unwrap();
    assert!(!stale.ok, "stale ref must not be actionable");
    assert!(stale.error.unwrap().contains("stale"));
    let back = p.back().await.unwrap();
    assert_eq!(back.snapshot.title, "One");
    let fwd = p.forward().await.unwrap();
    assert_eq!(fwd.snapshot.title, "Two");
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn waits_screenshots_evaluate_and_tabs() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        "<title>W</title><button id=b>Go</button><script>\
         document.getElementById('b').onclick=()=>setTimeout(()=>{\
         document.body.append(Object.assign(document.createElement('h2'),{id:'late',textContent:'Done!'}))},300)</script>",
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    p.click(&ref_of(&snap, "button", "Go")).await.unwrap();
    let r = p
        .wait_for(
            &WaitCondition::Selector("#late".into()),
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    assert!(r.ok);
    let r = p
        .wait_for(
            &WaitCondition::Text("Done!".into()),
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    assert!(r.ok);
    let r = p
        .wait_for(
            &WaitCondition::Text("never".into()),
            Some(Duration::from_millis(300)),
        )
        .await
        .unwrap();
    assert!(!r.ok);
    assert!(
        p.wait_for(&WaitCondition::NetworkIdle, Some(Duration::from_secs(5)))
            .await
            .unwrap()
            .ok
    );

    assert_eq!(p.evaluate("1+2").await.unwrap(), 3);
    assert_eq!(p.evaluate("({a:[1,2]})").await.unwrap()["a"][1], 2);
    assert!(p.evaluate("throw new Error('boom')").await.is_err());

    let png = p
        .screenshot(&ScreenshotOptions {
            target: ScreenshotTarget::Viewport,
            jpeg_quality: None,
        })
        .await
        .unwrap();
    assert_eq!(&png[..4], b"\x89PNG");
    let jpg = p.screenshot(&ScreenshotOptions::default()).await.unwrap();
    assert_eq!(&jpg[..2], b"\xFF\xD8");
    let full = p
        .screenshot(&ScreenshotOptions {
            target: ScreenshotTarget::FullPage,
            jpeg_quality: Some(50),
        })
        .await
        .unwrap();
    assert_eq!(&full[..2], b"\xFF\xD8");
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let el = p
        .screenshot(&ScreenshotOptions {
            target: ScreenshotTarget::Element(ref_of(&snap, "button", "Go")),
            jpeg_quality: Some(70),
        })
        .await
        .unwrap();
    assert!(el.len() < jpg.len());

    // Multiple tabs + attach by id + a second client attaching over the websocket URL.
    let p2 = b
        .new_page(&data_url("<title>Second</title>"))
        .await
        .unwrap();
    assert_eq!(p2.title().await.unwrap(), "Second");
    assert_eq!(p.title().await.unwrap(), "W");
    let pages = b.pages().await.unwrap();
    assert!(pages.len() >= 3, "{pages:?}"); // about:blank + two tabs
    let other = Browser::connect(b.ws_url()).await.unwrap();
    let again = other.attach(p2.target_id()).await.unwrap();
    assert_eq!(again.title().await.unwrap(), "Second");
    again.detach().await.unwrap();
    p2.close().await.unwrap();
    b.close().await.unwrap();
}

/// Tiny HTTP server: `/` embeds `/inner` from a different site (localhost vs 127.0.0.1) -> OOPIF.
async fn serve() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /inner") {
                    "<title>inner</title><button onclick=\"parent.postMessage('paid','*')\">Pay</button>".to_string()
                } else if req.starts_with("GET /same") {
                    "<title>same</title><button onclick=\"parent.postMessage('same-clicked','*')\">Same</button>".to_string()
                } else {
                    format!(
                        "<title>outer</title><script>onmessage=e=>document.title=e.data</script><h1 style='margin-top:120px'>Outer</h1>\
                         <iframe style='margin-left:60px' width=400 height=200 src='http://localhost:{port}/inner'></iframe>\
                         <iframe style='margin-left:60px' width=400 height=200 src='/same'></iframe>"
                    )
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn iframes_including_out_of_process() {
    let port = serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await; // let the OOPIF attach and load
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let text = snap.render();
    println!("{text}");
    let pay = ref_of(&snap, "button", "Pay");
    let same = ref_of(&snap, "button", "Same");
    // Same-process iframe click lands correctly.
    let r = p.click(&same).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(p.title().await.unwrap(), "same-clicked");
    // Out-of-process iframe: coordinates must be translated through the owner element.
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let pay2 = ref_of(&snap, "button", "Pay");
    assert_eq!(
        pay, pay2,
        "refs are stable across snapshots of the same documents"
    );
    let r = p.click(&pay2).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(p.title().await.unwrap(), "paid");
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_typed_secret_stays_out_of_every_page_read_until_navigation() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    // The page copies the typed value into a sibling box and a plain-text mirror, and a click
    // re-creates the input (a new node that keeps the value), as an OTP widget or an SPA step may.
    let p = open(
        &b,
        r#"<html><body><label>Enter code <input id=a name=code aria-label="Enter code" autocomplete=off type=text></label>
<input id=d2 aria-label="Digit 2" autocomplete=off type=text>
<div id=mirror></div>
<button id=go onclick="var o=document.getElementById('a');var n=o.cloneNode();n.value=o.value;n.id='b';o.replaceWith(n)">Next</button><p>plain</p>
<script>document.getElementById('a').addEventListener('input',function(e){
 document.getElementById('d2').value=e.target.value;document.getElementById('mirror').textContent=e.target.value;});</script>
</body></html>"#,
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let field = ref_of(&snap, "textbox", "Enter code");
    let button = ref_of(&snap, "button", "Next");
    p.fill_secret(&field, "834512", |_| true).await.unwrap();
    // A fresh snapshot, a cached one and the page text an action returns all hide the digits,
    // wherever the page copied them.
    for _ in 0..2 {
        let text = p
            .snapshot(&SnapshotOptions::default())
            .await
            .unwrap()
            .render();
        assert!(!text.contains("834512"), "{text}");
        assert!(text.contains("[masked]"), "{text}");
    }
    let after = p.click(&button).await.unwrap();
    let rendered = after.snapshot.render();
    assert!(!rendered.contains("834512"), "re-created input: {rendered}");
    let text = p
        .snapshot(&SnapshotOptions::default())
        .await
        .unwrap()
        .render();
    assert!(!text.contains("834512"), "{text}");
    assert!(
        p.screenshot(&ScreenshotOptions::default()).await.is_err(),
        "screenshots stay refused"
    );
    assert!(
        p.evaluate("document.querySelector('input').value")
            .await
            .is_err(),
        "a script cannot read the value either"
    );
    // Another tab asking for things changes nothing for this one.
    let other = open(&b, "<html><body><p>other tab</p></body></html>").await;
    assert!(other.evaluate("1+1").await.is_ok());
    assert!(
        other
            .screenshot(&ScreenshotOptions::default())
            .await
            .is_ok()
    );
    assert!(
        p.evaluate("document.querySelector('input').value")
            .await
            .is_err(),
        "still refused after the other tab was used"
    );
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn field_that_turns_into_a_password_after_the_check_is_refused() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r"<html><body><label>Note <input id=n type=text></label></body></html>",
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let r = ref_of(&snap, "textbox", "Note");
    // The earlier check saw an ordinary field ...
    assert!(!p.field_target(&r).await.unwrap().sensitive);
    // ... then the page turns it into a password input, and it has focus when the key is sent.
    p.evaluate("document.getElementById('n').type = 'password'")
        .await
        .unwrap();
    assert!(p.click(&r).await.unwrap().ok);
    for res in [p.fill(&r, "hunter2").await, p.press_key("a").await] {
        let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
        assert!(err.contains("refused"), "{err}");
    }
    let typed = p
        .evaluate("document.getElementById('n').value")
        .await
        .unwrap();
    assert_eq!(typed.as_str(), Some(""), "nothing may have been typed");
    // Navigation keys stay allowed.
    p.press_key("Tab").await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn contenteditable_editor_and_select_are_plain_fields() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r#"<html><body><div id=ed contenteditable=true role=textbox aria-label="Message"></div>
<label>Country <select id=c><option>Turkey</option><option>Germany</option></select></label></body></html>"#,
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let ed = ref_of(&snap, "textbox", "Message");
    assert!(!p.field_target(&ed).await.unwrap().sensitive);
    let sel = ref_of(&snap, "combobox", "Country");
    assert!(!p.field_target(&sel).await.unwrap().sensitive);
    let r = p.fill(&ed, "hello").await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    let typed = p
        .evaluate("document.getElementById('ed').textContent")
        .await
        .unwrap();
    assert_eq!(typed.as_str(), Some("hello"));
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn editable_div_named_password_and_type_text_are_refused() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r#"<html><body><div id=ed contenteditable=true role=textbox aria-label="Password"></div>
<label>Note <input id=n type=password></label></body></html>"#,
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let ed = ref_of(&snap, "textbox", "Password");
    assert!(p.field_target(&ed).await.unwrap().sensitive);
    let note = ref_of(&snap, "textbox", "Note");
    let res = p.type_text(&note, "hunter2").await;
    let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
    assert!(err.contains("refused"), "{err}");
    let typed = p
        .evaluate("document.getElementById('n').value")
        .await
        .unwrap();
    assert_eq!(typed.as_str(), Some(""), "nothing may have been typed");
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_page_cannot_hide_the_focused_password_field_from_the_key_check() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r#"<html><body><input id=pw type=password aria-label="Secret">
<script>Object.defineProperty(Document.prototype,'activeElement',{get(){return null}});
Object.defineProperty(document,'activeElement',{get(){return null}});
document.getElementById('pw').focus();</script></body></html>"#,
    )
    .await;
    let res = p.press_key("a").await;
    let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
    assert!(err.contains("refused"), "{err}");
    let typed = p
        .evaluate("document.getElementById('pw').value")
        .await
        .unwrap();
    assert_eq!(typed.as_str(), Some(""), "nothing may have been typed");
    b.close().await.unwrap();
}

/// A request that was already open before the click (long poll, beacon, stream) must not make
/// the click wait for network idle: the action started no network activity, so there is nothing
/// to wait for.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn click_does_not_wait_for_a_request_that_was_open_before_it() {
    // A server that accepts and never answers.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = l.accept().await {
            held.push(s);
        }
    });
    let html = format!(
        "<title>Poll</title><button onclick=\"document.title='clicked'\">Go</button>\
<script>fetch('http://127.0.0.1:{port}/hold',{{mode:'no-cors'}}).catch(()=>{{}});</script>"
    );
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b.new_page(&data_url(&html)).await.unwrap();
    let snap = page.snapshot(&SnapshotOptions::default()).await.unwrap();
    let go = ref_of(&snap, "button", "Go");
    // Let the page's own request start.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t = std::time::Instant::now();
    let r = page.click(&go).await.unwrap();
    let took = t.elapsed();
    assert!(r.ok, "{:?}", r.error);
    assert!(r.settle_note.is_none(), "{:?}", r.settle_note);
    assert!(
        took < Duration::from_millis(1200),
        "click waited {took:?} for a request that predates it"
    );
    let _ = b.close().await;
}

fn batch_guard(focus_ref: Option<&str>) -> StepGuard {
    StepGuard {
        batch: true,
        focus_ref: focus_ref.map(str::to_owned),
    }
}

/// A modal that sits over an approved button must not receive the click: the ref's role and
/// name are unchanged, only what is on top of it is not.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_batch_click_is_refused_when_another_element_covers_the_target() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r#"<html><body>
<button id=nt style="position:absolute;left:20px;top:20px;width:120px;height:40px" onclick="document.getElementById('out').textContent='no-thanks'">No thanks</button>
<button id=ok style="position:absolute;left:20px;top:100px;width:120px;height:40px" onclick="document.getElementById('out').textContent='ok'"><span id=inner>Fine</span></button>
<button id=pay style="position:fixed;left:0;top:0;width:200px;height:70px;z-index:9" onmouseover="document.getElementById('out').textContent='HOVERED'" onclick="document.getElementById('out').textContent='PAID'">Pay</button>
<p id=out style="position:absolute;top:300px">idle</p>
<script>
// A hostile page: the covering node claims the approved button as its parent and host, so a
// walk in the page's own JS world would call it a descendant.
const nt = document.getElementById('nt');
for (const k of ['parentNode', 'host', 'parentElement']) {
  Object.defineProperty(document.getElementById('pay'), k, { get: () => nt });
}
</script></body></html>"#,
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let nt = ref_of(&snap, "button", "No thanks");
    let fine = ref_of(&snap, "button", "Fine");
    let guard = batch_guard(None);
    let res = p
        .click_with_guard(&nt, &DialogOptions::default(), None, &guard)
        .await;
    let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
    assert!(err.contains("on top of"), "{err}");
    let out = p
        .evaluate("document.getElementById('out').textContent")
        .await
        .unwrap();
    assert_eq!(
        out.as_str(),
        Some("idle"),
        "no mouse event of any kind (not even a move/hover) may reach the covering element"
    );
    // Not covered, and the point lands on a child (the span) of the button: clicked.
    let r = p
        .click_with_guard(&fine, &DialogOptions::default(), None, &guard)
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    let out = p
        .evaluate("document.getElementById('out').textContent")
        .await
        .unwrap();
    assert_eq!(out.as_str(), Some("ok"));
    b.close().await.unwrap();
}

/// The first hit test passes, then the pointer move runs the page's mousemove handler, which
/// lays "Pay" over the target. The press must not be sent: no mousedown/click/mouseup at all.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_batch_click_is_refused_when_the_hover_handler_covers_the_target() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r#"<html><body>
<button id=nt style="position:absolute;left:20px;top:20px;width:120px;height:40px" onmousemove="if(!document.getElementById('pay')){const d=document.createElement('button');d.id='pay';d.textContent='Pay';d.style.cssText='position:fixed;left:0;top:0;width:200px;height:70px;z-index:9';d.onmousedown=d.onmouseup=d.onclick=()=>{document.getElementById('log').textContent+='pay '};document.body.appendChild(d)}" onmousedown="document.getElementById('log').textContent+='nt-down '" onclick="document.getElementById('log').textContent+='nt-click '">No thanks</button>
<p id=log style="position:absolute;top:300px"></p>
</body></html>"#,
    )
    .await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let nt = ref_of(&snap, "button", "No thanks");
    let res = p
        .click_with_guard(&nt, &DialogOptions::default(), None, &batch_guard(None))
        .await;
    let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
    assert!(err.contains("on top of"), "{err}");
    let log = p
        .evaluate("document.getElementById('log').textContent")
        .await
        .unwrap();
    assert_eq!(
        log.as_str(),
        Some(""),
        "the press and release must not be sent once the hover covered the target"
    );
    b.close().await.unwrap();
}

/// Enter goes to whatever has focus. A page script that moves focus to another control after
/// the fill must stop the batch's Enter.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_batch_enter_is_refused_when_focus_moved_off_the_filled_field() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let html = |steal: bool| {
        format!(
            r#"<html><body><label>Email <input id=email type=text {}></label>
<button id=del onclick="document.getElementById('out').textContent='DELETED'">Delete account</button>
<p id=out>idle</p>
<script>document.getElementById('email').addEventListener('keydown',e=>{{if(e.key==='Enter')document.getElementById('out').textContent='submitted'}});</script>
</body></html>"#,
            if steal {
                r#"oninput="document.getElementById('del').focus()""#
            } else {
                ""
            }
        )
    };
    // Focus stays on the field: Enter is pressed.
    let p = open(&b, &html(false)).await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let email = ref_of(&snap, "textbox", "Email");
    assert!(p.fill(&email, "a@b.c").await.unwrap().ok);
    let r = p
        .press_key_with_guard("Enter", None, &batch_guard(Some(&email)))
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    let out = p
        .evaluate("document.getElementById('out').textContent")
        .await
        .unwrap();
    assert_eq!(out.as_str(), Some("submitted"));
    // The script moved focus to "Delete account": refused, nothing activated.
    let p = open(&b, &html(true)).await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let email = ref_of(&snap, "textbox", "Email");
    assert!(p.fill(&email, "a@b.c").await.unwrap().ok);
    let res = p
        .press_key_with_guard("Enter", None, &batch_guard(Some(&email)))
        .await;
    let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
    assert!(err.contains("NOT pressed"), "{err}");
    let out = p
        .evaluate("document.getElementById('out').textContent")
        .await
        .unwrap();
    assert_eq!(out.as_str(), Some("idle"));
    // No earlier node to compare with: Enter is refused as well.
    let res = p
        .press_key_with_guard("Enter", None, &batch_guard(None))
        .await;
    let err = res.map_or_else(|e| e.to_string(), |a| a.error.unwrap_or_default());
    assert!(err.contains("NOT pressed"), "{err}");
    b.close().await.unwrap();
}

/// Navigating to a page that opens a long-lived request (long poll, SSE, stream) must not wait
/// the full 2 s network-idle cap once `load` + quiet window hold.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn navigate_does_not_wait_for_long_lived_request() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut s, _)) = l.accept().await {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            if req.starts_with("GET /hold") {
                held.push(s);
            } else {
                let body = "<title>Poll</title><h1>Poll</h1><button>Ok</button>\
                     <script>fetch('/hold').catch(()=>{});</script>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            }
        }
    });
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b.new_page("about:blank").await.unwrap();
    let t = std::time::Instant::now();
    let r = page
        .navigate(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let took = t.elapsed();
    assert!(r.ok, "{:?}", r.error);
    // The real guard: if the held request still blocked the settle, it would run into the
    // 2 s cap and report "network still busy". The request is only ignored once it has
    // been open for `LONG_LIVED_AGE` (1 s), so the floor is about 1 s; the time bound
    // leaves room for slow CI runners and only catches a much larger stall.
    assert!(r.settle_note.is_none(), "{:?}", r.settle_note);
    assert!(
        took < Duration::from_secs(4),
        "navigate waited {took:?} for a long-lived request"
    );
    let _ = b.close().await;
}

/// A finite fetch that starts after `load` and replaces a skeleton must be waited for: the
/// action snapshot has to show the final content, not the skeleton.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn navigate_waits_for_finite_fetch_after_load() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /data") {
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    "FinalRows".to_string()
                } else {
                    "<title>Dash</title><p id=o>Skeleton</p>                     <script>addEventListener('load',()=>setTimeout(()=>                     fetch('/data').then(r=>r.text()).then(t=>{o.textContent=t}),20));</script>"
                        .to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK
Content-Type: text/html
Content-Length: {}
Connection: close

{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = b.new_page("about:blank").await.unwrap();
    let r = page
        .navigate(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert!(r.ok, "{:?}", r.error);
    let snap = page.snapshot(&SnapshotOptions::default()).await.unwrap();
    assert!(
        snap.render().contains("FinalRows"),
        "snapshot taken while fetch was still open:
{}",
        snap.render()
    );
    let _ = b.close().await;
}

/// Unchanged DOM reuses the cached capture without extra CDP AX/DOMSnapshot rounds, while
/// background DOM mutations invalidate the cache.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn snapshot_cache_hits_when_unchanged_and_invalidates_on_dom_mutation() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let page = open(
        &b,
        "<title>Cache</title><button id=b>Initial</button><input id=i type=text value=v1>\
         <script>setTimeout(()=>{\
         const p=document.createElement('p');p.textContent='AsyncAdded';document.body.append(p);\
         document.getElementById('i').value='v2';\
         },250);</script>",
    )
    .await;
    let opts = SnapshotOptions::default();
    page.invalidate_refs().await;
    let r0 = page.snapshot_rounds();
    let s1 = page.snapshot(&opts).await.unwrap();
    assert_eq!(page.snapshot_rounds() - r0, 1);
    assert!(s1.render().contains("Initial"));

    // Immediate repeat with same and different options reuses cached capture (0 extra CDP rounds).
    let s2 = page.snapshot(&opts).await.unwrap();
    assert_eq!(page.snapshot_rounds() - r0, 1);
    assert_eq!(s1.render(), s2.render());

    let interactive = page
        .snapshot(&SnapshotOptions {
            interactive_only: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.snapshot_rounds() - r0, 1);
    assert!(interactive.render().contains("button \"Initial\""));

    // After the background timer mutates the DOM, snapshot must observe the new content.
    tokio::time::sleep(Duration::from_millis(450)).await;
    let s3 = page.snapshot(&opts).await.unwrap();
    assert!(
        s3.render().contains("AsyncAdded") && s3.render().contains("v2"),
        "cache served stale snapshot after DOM/value mutation:\n{}",
        s3.render()
    );
    let _ = b.close().await;
}

/// Cookie / consent banners: four layouts that must be answered with the privacy-preserving
/// button, one that only offers "accept", and a page without a banner.
const CONSENT_FIXTURES: [(&str, &str, &str, &str); 5] = [
    (
        "onetrust",
        r#"<html><head><title>start</title></head><body><p>Shop</p>
<div id="onetrust-banner-sdk" style="position:fixed;bottom:0;left:0;right:0;height:120px;background:#ddd">
<button id="onetrust-accept-btn-handler" onclick="document.title='accepted'">Accept All Cookies</button>
<button id="onetrust-reject-all-handler" onclick="document.title='rejected';this.parentNode.remove()">Reject All</button></div></body></html>"#,
        "rejected",
        "OneTrust",
    ),
    (
        "cookiebot",
        r#"<html><head><title>start</title></head><body><p>News</p>
<div id="CybotCookiebotDialog" style="position:fixed;top:20%;left:20%;width:60%;height:200px;background:#ddd">
<button id="CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll" onclick="document.title='accepted'">Allow all</button>
<button id="CybotCookiebotDialogBodyButtonDecline" onclick="document.title='rejected'">Use necessary cookies only</button></div></body></html>"#,
        "rejected",
        "Cookiebot",
    ),
    (
        "usercentrics-shadow",
        r#"<html><head><title>start</title></head><body><p>Blog</p><div id="usercentrics-root"></div>
<script>const r=document.getElementById('usercentrics-root').attachShadow({mode:'open'});
r.innerHTML='<div style="position:fixed;bottom:0;left:0;right:0;height:100px;background:#ddd"><button data-testid="uc-accept-all-button" onclick="document.title=\'accepted\'">Accept all</button><button data-testid="uc-deny-all-button" onclick="document.title=\'rejected\'">Deny</button></div>';
</script></body></html>"#,
        "rejected",
        "Usercentrics",
    ),
    (
        "generic-turkish",
        r#"<html><head><meta charset="utf-8"><title>start</title></head><body><p>Magaza</p>
<div class="uyari" style="position:fixed;bottom:0;left:0;right:0;height:100px;background:#ddd">Bu site cerez kullanir (cookie).
<a href="https://elsewhere.invalid/politika" onclick="document.title='followed'">Cerez politikasi</a>
<button onclick="document.title='accepted'">Tümünü kabul et</button>
<button onclick="document.title='rejected'">Yalnızca gerekli</button></div></body></html>"#,
        "rejected",
        "generic",
    ),
    (
        "accept-only",
        r#"<html><head><title>start</title></head><body><p>Docs</p>
<div role="dialog" style="position:fixed;bottom:0;left:0;right:0;height:100px;background:#ddd">We use cookies and similar tools.
<a href="https://elsewhere.invalid/privacy" onclick="document.title='followed'">Privacy policy</a>
<button onclick="document.title='accepted'">Accept</button></div></body></html>"#,
        "accepted",
        "generic",
    ),
];

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn dismiss_consent_picks_the_privacy_preserving_button_in_each_layout() {
    use browser_cdp::{ConsentChoice, Feel};
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    for (name, html, title, cmp) in CONSENT_FIXTURES {
        let p = open(&b, html).await;
        let outcome = p.dismiss_consent(None, Feel::Instant).await.unwrap();
        let (choice, found_cmp, label) = outcome
            .clicked
            .clone()
            .unwrap_or_else(|| panic!("{name}: nothing clicked: {:?}", outcome.note));
        assert_eq!(found_cmp, cmp, "{name}");
        let want = if title == "rejected" {
            ConsentChoice::Reject
        } else {
            ConsentChoice::Accept
        };
        assert_eq!(choice, want, "{name}: clicked {label:?}");
        assert_eq!(
            p.evaluate("document.title").await.unwrap(),
            serde_json::json!(title),
            "{name}: clicked {label:?}"
        );
    }
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn dismiss_consent_does_nothing_on_a_page_without_a_banner() {
    use browser_cdp::Feel;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(
        &b,
        r#"<html><head><title>start</title></head><body><h1>Plain</h1>
<button onclick="document.title='clicked'">Accept the invitation</button>
<button onclick="document.title='clicked'">Decline the invitation</button></body></html>"#,
    )
    .await;
    let outcome = p.dismiss_consent(None, Feel::Instant).await.unwrap();
    assert!(outcome.clicked.is_none(), "{outcome:?}");
    assert!(
        outcome
            .note
            .unwrap()
            .contains("No cookie or consent dialog")
    );
    assert_eq!(
        p.evaluate("document.title").await.unwrap(),
        serde_json::json!("start")
    );
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn dismiss_consent_refuses_a_page_on_another_origin_than_the_approved_one() {
    use browser_cdp::Feel;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let port = common::serve().await;
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/consent"))
        .await
        .unwrap();
    let err = p
        .dismiss_consent(Some("https://example.com"), Feel::Instant)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("origin changed"), "{err}");
    assert_eq!(
        p.evaluate("document.title").await.unwrap(),
        serde_json::json!("consent-start")
    );
    // The right origin goes through.
    let outcome = p
        .dismiss_consent(Some(&format!("http://127.0.0.1:{port}")), Feel::Instant)
        .await
        .unwrap();
    assert!(outcome.clicked.is_some(), "{outcome:?}");
    let _ = b.close().await;
}

const FIX: &str = "position:fixed;bottom:0;left:0;right:0;height:120px;background:#ddd";
const CLICK: &str = "document.title='clicked';return false";

/// Dialogs the consent tool must NOT click anything in: a link or form button that would leave
/// the page, dialogs that hold a payment, a login, a form or a permission request, a question
/// that is not about cookies, and two dialogs that both fit.
fn hostile_consent_fixtures() -> Vec<(&'static str, String)> {
    vec![
        (
            "role=button link to another site",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <a role="button" href="https://evil.invalid/x" onclick="{CLICK}">Reject all</a></div>"#
            ),
        ),
        (
            "button with a formaction to another site",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <form action="/plain"><button formaction="https://evil.invalid/p" onclick="{CLICK}">Reject all</button></form></div>"#
            ),
        ),
        (
            "submit input with a formaction to another site",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <form><input type="submit" formaction="https://evil.invalid/p" value="Reject all" onclick="{CLICK}"></form></div>"#
            ),
        ),
        (
            "button nested in a link",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <a href="https://evil.invalid/"><button onclick="{CLICK}">Reject all</button></a></div>"#
            ),
        ),
        (
            "link that opens a new tab",
            format!(
                r##"<div role="dialog" style="{FIX}">We use cookies. <a href="#" target="_blank" onclick="{CLICK}">Reject all</a></div>"##
            ),
        ),
        (
            "payment dialog with Agree and pay",
            format!(
                r#"<div role="dialog" style="{FIX}">Pay 499 USD now. By continuing you accept our privacy policy and cookies. <button onclick="{CLICK}">Agree and pay</button><button onclick="{CLICK}">Accept</button></div>"#
            ),
        ),
        (
            "permission dialog",
            format!(
                r#"<div role="dialog" style="{FIX}">Acme wants to access your account (cookies, privacy). <button onclick="{CLICK}">Allow</button><button onclick="{CLICK}">Deny</button></div>"#
            ),
        ),
        (
            "yes to a destructive question",
            format!(
                r#"<div role="dialog" style="{FIX}">Delete all cookie and tracking data? <button onclick="{CLICK}">Yes</button><button onclick="{CLICK}">Cancel</button></div>"#
            ),
        ),
        (
            "dialog with a password field",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <input type="password"> <button onclick="{CLICK}">Accept</button></div>"#
            ),
        ),
        (
            "dialog with a form that takes text",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <form><input type="text" name="email"><button onclick="{CLICK}">Accept</button></form></div>"#
            ),
        ),
        (
            "two separate dialogs",
            format!(
                r#"<div role="dialog" style="{FIX}">We use cookies. <button onclick="{CLICK}">Accept</button></div><div role="dialog" style="position:fixed;top:0;left:0;right:0;height:120px;background:#eee">Cookies for ads. <button onclick="{CLICK}">Accept</button></div>"#
            ),
        ),
    ]
}

async fn plain_page(b: &Browser, port: u16, html: &str) -> Page {
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/plain"))
        .await
        .unwrap();
    p.evaluate(&format!(
        "document.title='start';document.body.innerHTML={}",
        serde_json::to_string(html).unwrap()
    ))
    .await
    .unwrap();
    p
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn dismiss_consent_clicks_nothing_in_hostile_dialogs() {
    use browser_cdp::Feel;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let port = common::serve().await;
    for (name, html) in hostile_consent_fixtures() {
        let p = plain_page(&b, port, &html).await;
        let outcome = p.dismiss_consent(None, Feel::Instant).await.unwrap();
        assert!(outcome.clicked.is_none(), "{name}: {outcome:?}");
        assert_eq!(
            p.evaluate("document.title").await.unwrap(),
            serde_json::json!("start"),
            "{name}: something was clicked"
        );
        p.close().await.ok();
    }
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn dismiss_consent_reads_negated_accept_phrases_as_rejecting() {
    use browser_cdp::{ConsentChoice, Feel};
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let port = common::serve().await;
    for label in [
        "Do not accept",
        "Don't allow",
        "I do not agree",
        "Nicht akzeptieren",
    ] {
        let html = format!(
            r#"<div role="dialog" style="{FIX}">We use cookies. <button onclick="document.title='rejected'">{label}</button></div>"#
        );
        let p = plain_page(&b, port, &html).await;
        let outcome = p.dismiss_consent(None, Feel::Instant).await.unwrap();
        let (choice, _, _) = outcome
            .clicked
            .unwrap_or_else(|| panic!("{label}: nothing clicked"));
        assert_eq!(choice, ConsentChoice::Reject, "{label}");
        p.close().await.ok();
    }
    let _ = b.close().await;
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn dismiss_consent_lists_the_options_when_two_dialogs_fit() {
    use browser_cdp::Feel;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let port = common::serve().await;
    let (_, html) = hostile_consent_fixtures().pop().unwrap();
    let p = plain_page(&b, port, &html).await;
    let outcome = p.dismiss_consent(None, Feel::Instant).await.unwrap();
    let note = outcome.note.unwrap();
    assert!(
        note.contains("More than one") && note.contains("\"accept\""),
        "{note}"
    );
    let _ = b.close().await;
}

// ---- pointer mode: what has no ref -------------------------------------------------------------

async fn text_of(p: &Page, js: &str) -> String {
    p.evaluate(js)
        .await
        .unwrap()
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A click by screenshot coordinates, the way the tool does it.
async fn point_click(p: &Page, x: f64, y: f64) -> (browser_cdp::ActionResult, String) {
    use browser_cdp::{Feel, PointerButton, PointerOp, PointerRequest};
    p.pointer(
        &PointerRequest {
            op: PointerOp::Click {
                button: PointerButton::Left,
                count: 1,
            },
            at: (x, y),
            feel: Feel::Instant,
            expected: None,
        },
        &DialogOptions::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn pointer_reaches_a_canvas_radio_and_a_box_in_another_origins_frame() {
    let port = common::serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/widgets"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await; // let the OOPIF attach and load

    // The picture is made for pointing at: JPEG, sized like the viewport (scaled), and it lists
    // the canvas and both frames, which the accessibility snapshot cannot click into.
    let shot = p.grounded_screenshot().await.unwrap();
    assert!(shot.bytes.starts_with(&[0xff, 0xd8]), "a JPEG");
    assert_eq!(shot.image, shot.view.image_size());
    let kinds: Vec<_> = shot.regions.iter().map(|r| r.kind.as_str()).collect();
    assert!(
        kinds.contains(&"canvas") && kinds.iter().filter(|k| **k == "iframe").count() == 2,
        "{kinds:?}"
    );
    println!("{}", shot.describe());
    let canvas = shot.regions.iter().find(|r| r.kind == "canvas").unwrap();
    let human = shot
        .regions
        .iter()
        .find(|r| r.label == "Human check")
        .expect("the cross-origin frame is listed by its title");

    // A radio drawn on a canvas: no element, so only a point can choose it.
    let (acted, hit) = point_click(
        &p,
        f64::from(canvas.x) + 1.0 + 180.0,
        f64::from(canvas.y) + 1.0 + 30.0,
    )
    .await;
    assert!(acted.ok, "{:?}", acted.error);
    assert!(hit.starts_with("<canvas"), "{hit}");
    assert_eq!(
        text_of(&p, "document.getElementById('picked').textContent").await,
        "radio-1"
    );

    // A checkbox drawn with a div inside a frame of another origin: the click is routed there.
    let (acted, hit) = point_click(&p, f64::from(human.x) + 37.0, f64::from(human.y) + 37.0).await;
    assert!(acted.ok, "{:?}", acted.error);
    assert!(hit.contains("a frame"), "{hit}");
    let mut seen = String::new();
    for _ in 0..20 {
        seen = text_of(&p, "document.getElementById('picked').textContent").await;
        if seen.contains("|human") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        seen.contains("|human"),
        "the frame's checkbox was not clicked: {seen}"
    );

    // A point outside the picture is refused, nothing is clicked.
    let err = p
        .pointer(
            &browser_cdp::PointerRequest {
                op: browser_cdp::PointerOp::Move,
                at: (99_999.0, 5.0),
                feel: browser_cdp::Feel::Instant,
                expected: None,
            },
            &DialogOptions::default(),
        )
        .await
        .unwrap();
    assert!(!err.0.ok);
    assert!(err.0.error.unwrap().contains("outside the screenshot"));
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
#[allow(clippy::many_single_char_names)]
async fn pointer_typing_and_drag_and_wheel_work_and_secret_fields_are_refused() {
    use browser_cdp::{Feel, PointerOp, PointerRequest};
    let port = common::serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/widgets"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let view = p.pointer_view().await.unwrap();
    let s = view.scale();
    let centre = |id: &'static str| {
        let p = &p;
        async move {
            let r = p
                .evaluate(&format!(
                    "(()=>{{const r=document.getElementById('{id}').getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2]}})()"
                ))
                .await
                .unwrap();
            (r[0].as_f64().unwrap() * s, r[1].as_f64().unwrap() * s)
        }
    };
    // Click the field by point, then type into the focused element.
    let (x, y) = centre("free").await;
    assert!(point_click(&p, x, y).await.0.ok);
    let typed = p.pointer_type("hi there", None).await.unwrap();
    assert!(typed.ok, "{:?}", typed.error);
    assert_eq!(
        text_of(&p, "document.getElementById('free').value").await,
        "hi there"
    );

    // A password field takes the click but never the typing.
    let (x, y) = centre("pw").await;
    assert!(point_click(&p, x, y).await.0.ok);
    let refused = p.pointer_type("hunter2", None).await.unwrap();
    assert!(!refused.ok);
    assert!(refused.error.unwrap().contains("refused"));
    assert_eq!(text_of(&p, "document.getElementById('pw').value").await, "");

    // Wheel over the page scrolls it; a drag fires the mouse events a drag needs.
    p.evaluate("document.body.style.height='4000px'")
        .await
        .unwrap();
    let acted = p
        .pointer(
            &PointerRequest {
                op: PointerOp::Wheel { dx: 0.0, dy: 400.0 },
                at: (50.0, 50.0),
                feel: Feel::Instant,
                expected: None,
            },
            &DialogOptions::default(),
        )
        .await
        .unwrap();
    assert!(acted.0.ok, "{:?}", acted.0.error);
    let y = p
        .evaluate("window.scrollY")
        .await
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(y > 100.0, "the wheel scrolled the page: {y}");
    p.evaluate(
        "window.__drag=[];for(const t of ['mousedown','mousemove','mouseup'])document.addEventListener(t,e=>window.__drag.push(t))",
    )
    .await
    .unwrap();
    let acted = p
        .pointer(
            &PointerRequest {
                op: PointerOp::Drag { to: (200.0, 120.0) },
                at: (40.0, 60.0),
                feel: Feel::Instant,
                expected: None,
            },
            &DialogOptions::default(),
        )
        .await
        .unwrap();
    assert!(acted.0.ok, "{:?}", acted.0.error);
    let events = p.evaluate("window.__drag.join(',')").await.unwrap();
    let events = events.as_str().unwrap();
    assert!(
        events.contains("mousedown,mousemove,mousemove") && events.ends_with("mouseup"),
        "a drag presses, moves several times, releases: {events}"
    );
    b.close().await.unwrap();
}

/// Review finding 1: a point over another site's frame is judged by THAT frame's origin (not the
/// top page's), for a click, a hover and both ends of a drag.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_pointer_action_into_another_origins_frame_is_judged_by_that_frame() {
    use browser_cdp::{Feel, PointerButton, PointerOp, PointerRequest};
    let port = common::serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/widgets"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let shot = p.grounded_screenshot().await.unwrap();
    let human = shot
        .regions
        .iter()
        .find(|r| r.label == "Human check")
        .expect("the cross-origin frame is listed");
    let canvas = shot.regions.iter().find(|r| r.kind == "canvas").unwrap();
    let in_frame = (f64::from(human.x) + 37.0, f64::from(human.y) + 37.0);
    let on_top = (f64::from(canvas.x) + 20.0, f64::from(canvas.y) + 20.0);
    let top_origin = format!("http://127.0.0.1:{port}");
    let frame_origin = format!("http://localhost:{port}");

    // The browser says which origin receives an event at each point.
    assert_eq!(
        p.pointer_origin(on_top, None).await.unwrap().as_deref(),
        Some(top_origin.as_str())
    );
    assert_eq!(
        p.pointer_origin(in_frame, None).await.unwrap().as_deref(),
        Some(frame_origin.as_str())
    );
    // A drag whose two ends are in different origins has no single origin.
    assert_eq!(
        p.pointer_origin(on_top, Some(in_frame)).await.unwrap(),
        None
    );

    let act = |op: PointerOp, at: (f64, f64), expected: Option<&str>| {
        let p = &p;
        let expected = expected.map(str::to_owned);
        async move {
            p.pointer(
                &PointerRequest {
                    op,
                    at,
                    feel: Feel::Instant,
                    expected: expected.as_deref(),
                },
                &DialogOptions::default(),
            )
            .await
            .unwrap()
            .0
        }
    };
    let click = PointerOp::Click {
        button: PointerButton::Left,
        count: 1,
    };
    // Approved for the TOP page only: the click into the frame, the hover and the drag end are
    // all refused, and the frame saw nothing.
    for (op, what) in [
        (click, "click"),
        (PointerOp::Move, "hover"),
        (PointerOp::Wheel { dx: 0.0, dy: 5.0 }, "wheel"),
        (PointerOp::Drag { to: in_frame }, "drag end"),
    ] {
        let at = if what == "drag end" { on_top } else { in_frame };
        let r = act(op, at, Some(top_origin.as_str())).await;
        assert!(!r.ok, "{what} into the frame must be refused");
        let e = r.error.unwrap_or_default();
        assert!(e.contains("receiving frame origin changed"), "{what}: {e}");
    }
    assert!(
        !text_of(&p, "document.getElementById('picked').textContent")
            .await
            .contains("|human"),
        "the refused click reached the frame"
    );
    // A drag that STARTS in the frame is refused for a top-page approval too.
    let r = act(
        PointerOp::Drag { to: on_top },
        in_frame,
        Some(top_origin.as_str()),
    )
    .await;
    assert!(!r.ok);
    // Approved for the frame's own origin the same click goes through.
    let r = act(click, in_frame, Some(frame_origin.as_str())).await;
    assert!(r.ok, "{:?}", r.error);
    let mut seen = String::new();
    for _ in 0..20 {
        seen = text_of(&p, "document.getElementById('picked').textContent").await;
        if seen.contains("|human") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen.contains("|human"), "{seen}");
    b.close().await.unwrap();
}

/// Review finding 2: a page script that moves focus to a password field while text is typed
/// stops the typing at that character.
#[tokio::test]
#[ignore = "needs Chrome/Edge"]
#[allow(clippy::many_single_char_names)]
async fn typing_stops_when_focus_moves_to_a_password_field_mid_string() {
    let port = common::serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/widgets"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let view = p.pointer_view().await.unwrap();
    let s = view.scale();
    let r = p
        .evaluate(
            "(()=>{const r=document.getElementById('free').getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2]})()",
        )
        .await
        .unwrap();
    assert!(
        point_click(&p, r[0].as_f64().unwrap() * s, r[1].as_f64().unwrap() * s)
            .await
            .0
            .ok
    );
    // After the first character the page hands focus to the password field.
    p.evaluate(
        "document.getElementById('free').addEventListener('input',()=>document.getElementById('pw').focus())",
    )
    .await
    .unwrap();
    let typed = p
        .pointer_type("abcdef", Some(&format!("http://127.0.0.1:{port}")))
        .await
        .unwrap();
    assert!(!typed.ok, "typing must stop");
    let e = typed.error.unwrap_or_default();
    assert!(e.contains("refused"), "{e}");
    assert_eq!(
        text_of(&p, "document.getElementById('pw').value").await,
        "",
        "nothing may reach the password field"
    );
    assert_eq!(
        text_of(&p, "document.getElementById('free').value").await,
        "a",
        "only the character before the shift was typed"
    );

    // Focus moving to another harmless field is stopped too: the text was meant for the first.
    p.evaluate(
        "(()=>{const o=document.createElement('input');o.id='other';document.body.append(o);const f=document.getElementById('free');f.value='';f.focus();f.addEventListener('input',()=>o.focus(),{once:true})})()",
    )
    .await
    .unwrap();
    let typed = p
        .pointer_type("xyz", Some(&format!("http://127.0.0.1:{port}")))
        .await
        .unwrap();
    assert!(!typed.ok, "typing must stop at the focus change");
    assert_eq!(
        text_of(&p, "document.getElementById('other').value").await,
        "",
        "the rest of the text must not reach the field focus moved to"
    );
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_failed_send_is_recovered_in_place_without_reloading_the_page() {
    let port = common::serve().await;
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = b
        .new_page(&format!("http://127.0.0.1:{port}/widgets"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let opts = SnapshotOptions {
        compact: true,
        ..Default::default()
    };
    let in_form =
        |js: &'static str| format!("document.getElementById('form').contentDocument.{js}");

    // A role=radio div has a ref and is chosen like any control: no pointer needed.
    let snap = p.snapshot(&opts).await.unwrap();
    let radio = ref_of(&snap, "radio", "Option A");
    assert!(p.click(&radio).await.unwrap().ok);
    assert!(
        text_of(&p, "document.getElementById('picked').textContent")
            .await
            .contains("|aria")
    );

    // The form lives in a same-origin iframe; its fields are reachable by ref.
    let email = ref_of(&snap, "textbox", "Email");
    let send = ref_of(&snap, "button", "Send");
    assert!(p.fill(&email, "ada@example.org").await.unwrap().ok);
    assert!(p.click(&send).await.unwrap().ok);
    assert_eq!(
        text_of(&p, &in_form("getElementById('msg').textContent")).await,
        "Server busy, try again"
    );

    // A step on a ref that no longer exists fails ...
    let stale = p.click("e9999").await;
    assert!(
        stale.is_err() || !stale.as_ref().unwrap().ok,
        "a stale ref must fail"
    );
    // ... and the page is exactly as it was: the typed value is still there and nothing reloaded.
    assert_eq!(
        text_of(&p, &in_form("getElementById('email').value")).await,
        "ada@example.org"
    );
    assert_eq!(
        p.evaluate("window.__marker").await.unwrap(),
        serde_json::json!(1)
    );

    // Recovery in place: read the page again, send again; the second send goes through.
    let snap = p.snapshot(&opts).await.unwrap();
    let send = ref_of(&snap, "button", "Send");
    assert!(p.click(&send).await.unwrap().ok);
    assert_eq!(
        text_of(&p, &in_form("getElementById('msg').textContent")).await,
        "Sent ada@example.org"
    );
    assert_eq!(
        p.evaluate("window.__marker").await.unwrap(),
        serde_json::json!(1),
        "the page was never reloaded"
    );
    b.close().await.unwrap();
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn the_owners_open_browser_can_be_attached_and_is_only_disconnected_afterwards() {
    // The "owner's" browser: started on its own with a debugging port, as a person would.
    let owner = Browser::launch(LaunchOptions::default()).await.unwrap();
    let port = common::serve().await;
    let tab = owner
        .new_page(&format!("http://127.0.0.1:{port}/form"))
        .await
        .unwrap();
    tab.bring_to_front().await.unwrap();
    let endpoint = owner.ws_url().to_owned();
    browser_cdp::attach::loopback_endpoint(&endpoint).unwrap();

    let (attached, found) = browser_cdp::attach::connect_first(std::slice::from_ref(&endpoint))
        .await
        .unwrap();
    assert_eq!(found, endpoint);
    let page = attached.visible_page().await.unwrap();
    assert!(
        page.url().await.unwrap().ends_with("/form"),
        "the tab the owner looks at is the one used"
    );
    // Closing the attached side only disconnects: the owner's browser and tab keep running.
    attached.close().await.unwrap();
    assert!(!owner.is_closed());
    assert_eq!(tab.title().await.unwrap(), "Form");
    owner.close().await.unwrap();
}
