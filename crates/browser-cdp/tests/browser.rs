//! Integration tests that need an installed Chrome/Edge. Run with:
//! `cargo test -p browser-cdp -- --ignored --test-threads=1`

use std::time::Duration;

use base64::Engine as _;
use browser_cdp::{
    Browser, LaunchOptions, Page, ScreenshotOptions, ScreenshotTarget, SnapshotOptions,
    WaitCondition,
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

// -- actionability (card 248 item 1) ----------------------------------------------------------

const BLOCKED: &str = r#"<html><body>
<button id=buy onclick="window.n=(window.n||0)+1">Buy</button>
<button id=off disabled onclick="window.offHit=1">Off</button>
<div id=veil role=dialog aria-label="Cookie consent"
  style="position:fixed;inset:0;background:#fff;z-index:9">We use cookies</div>
<label><input id=cb type=checkbox onchange="window.c=(window.c||0)+1"> Agree</label>
<label><input id=veto type=checkbox onchange="window.v=(window.v||0)+1; this.checked=false"> Veto</label>
<label><input id=r1 type=radio name=g> One</label>
<input id=short maxlength=3 oninput="this.value=this.value.slice(0,3)" aria-label="Short">
</body></html>"#;

async fn counter(p: &Page, expr: &str) -> i64 {
    let v = p.evaluate(&format!("String({expr} || 0)")).await.unwrap();
    let t = v.as_str().unwrap();
    t.parse()
        .unwrap_or_else(|_| panic!("counter {expr}: {t:?}"))
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn a_covered_or_disabled_target_is_refused_with_the_blocker_named() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(&b, BLOCKED).await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let buy = ref_of(&snap, "button", "Buy");
    let r = p.click(&buy).await.unwrap();
    assert!(!r.ok);
    let err = r.error.unwrap();
    assert!(
        err.contains("covered by dialog \u{ab}Cookie consent\u{bb}"),
        "{err}"
    );
    assert_eq!(
        counter(&p, "window.n").await,
        0,
        "nothing may be clicked through the overlay"
    );

    // A disabled button is refused too, and its handler never runs.
    p.evaluate("document.getElementById('veil').remove()")
        .await
        .unwrap();
    let off = ref_of(&snap, "button", "Off");
    let r = p.click(&off).await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("disabled"));
    assert_eq!(counter(&p, "window.offHit").await, 0);

    // With the overlay gone the same ref is clickable.
    assert!(p.click(&buy).await.unwrap().ok);
    assert_eq!(counter(&p, "window.n").await, 1);
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn an_overlay_that_goes_away_is_waited_for_not_slept_for() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(&b, BLOCKED).await;
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let buy = ref_of(&snap, "button", "Buy");
    p.evaluate("setTimeout(() => document.getElementById('veil').remove(), 400)")
        .await
        .unwrap();
    let r = p.click(&buy).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(counter(&p, "window.n").await, 1);
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn set_checked_is_idempotent_and_never_clicks_twice_after_a_veto() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(&b, BLOCKED).await;
    p.evaluate("document.getElementById('veil').remove()")
        .await
        .unwrap();
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let cb = ref_of(&snap, "checkbox", "Agree");
    assert!(p.set_checked(&cb, true).await.unwrap().ok);
    assert!(
        p.set_checked(&cb, true).await.unwrap().ok,
        "already checked: no-op"
    );
    assert_eq!(
        counter(&p, "window.c").await,
        1,
        "the second call must not click"
    );
    assert!(p.set_checked(&cb, false).await.unwrap().ok);
    assert_eq!(counter(&p, "window.c").await, 2);

    let veto = ref_of(&snap, "checkbox", "Veto");
    let r = p.set_checked(&veto, true).await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("not clicking again"));
    assert_eq!(counter(&p, "window.v").await, 1, "no blind second click");

    let r1 = ref_of(&snap, "radio", "One");
    assert!(p.set_checked(&r1, true).await.unwrap().ok);
    let r = p.set_checked(&r1, false).await.unwrap();
    assert!(!r.ok && r.error.unwrap().contains("radio"));
    let r = p
        .set_checked(&ref_of(&snap, "button", "Buy"), true)
        .await
        .unwrap();
    assert!(!r.ok && r.error.unwrap().contains("not a checkbox"));
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn fill_reads_the_value_back_without_echoing_it() {
    let b = Browser::launch(LaunchOptions::default()).await.unwrap();
    let p = open(&b, BLOCKED).await;
    p.evaluate("document.getElementById('veil').remove()")
        .await
        .unwrap();
    let snap = p.snapshot(&SnapshotOptions::default()).await.unwrap();
    let short = ref_of(&snap, "textbox", "Short");
    assert!(p.fill(&short, "abc").await.unwrap().ok);
    let r = p.fill(&short, "tooLongSecret").await.unwrap();
    assert!(!r.ok);
    let err = r.error.unwrap();
    // The value is set through the field itself; a page that rewrites it is reported
    // without echoing what was typed.
    assert!(err.contains("did not accept the value"), "{err}");
    assert!(
        !err.contains("tooLongSecret") && !err.contains("too"),
        "{err}"
    );
}
