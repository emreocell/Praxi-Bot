//! Integration tests that need an installed Chrome/Edge. Run with:
//! `cargo test -p browser-cdp -- --ignored --test-threads=1`

use std::time::Duration;

use base64::Engine as _;
use browser_cdp::{
    Browser, DialogOptions, LaunchOptions, Page, ScreenshotOptions, ScreenshotTarget,
    SnapshotOptions, StepGuard, WaitCondition,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    assert!(r.settle_note.is_none(), "{:?}", r.settle_note);
    assert!(
        took < Duration::from_millis(1700),
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
