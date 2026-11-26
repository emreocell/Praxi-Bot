//! The browser tools end to end on a real browser and a local fixture site: pointer mode for
//! what has no ref (a canvas radio, a box in a frame of another origin), a step that fails and
//! is recovered in place, and attaching to the owner's own browser.
//!
//! Needs Chrome/Edge (`PRAXI_BROWSER_PATH` or an installed one). Run with
//! `cargo test -p builtin-tools --test browser_pointer -- --ignored --test-threads=1`.

use std::sync::Arc;
use std::time::Duration;

use builtin_tools::BrowserDriver as _;
use builtin_tools::browser::{BrowserSettings, CdpDriver, tools};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tool_registry::{ContentPart, Tool, ToolContext, ToolError};

/// A canvas-drawn radio group (no element to ref), a same-origin iframe form whose first send
/// fails, and a box drawn with a plain div in a frame of ANOTHER origin (`localhost`, while the
/// page is on `127.0.0.1`). `window.__marker` is wiped by any reload.
const WIDGETS: &str = "<title>Widgets</title><style>body{margin:0;font:16px sans-serif}</style>\
<h1>Pick one</h1><canvas id=cv width=300 height=60 style='border:1px solid #888'></canvas>\
<p id=picked>none</p>\
<iframe id=form src='/form' width=420 height=150></iframe>\
<iframe id=human src='http://localhost:{PORT}/human' width=300 height=90 title='Human check'></iframe>\
<script>window.__marker=1;\
const cv=document.getElementById('cv'),c=cv.getContext('2d');\
function draw(sel){c.clearRect(0,0,300,60);[60,180].forEach((x,i)=>{c.beginPath();c.arc(x,30,18,0,7);c.stroke();if(sel===i){c.beginPath();c.arc(x,30,10,0,7);c.fill();}});}\
draw(-1);\
cv.addEventListener('click',e=>{const r=cv.getBoundingClientRect();const i=(e.clientX-r.left)<120?0:1;draw(i);document.getElementById('picked').textContent='radio-'+i;});\
addEventListener('message',e=>{if(e.data==='human')document.getElementById('picked').textContent+='|human';});</script>";

const FORM: &str = "<title>form</title><label>Email <input id=email></label>\
<button id=send onclick=\"window.n=(window.n||0)+1;document.getElementById('msg').textContent=window.n<2?'Server busy, try again':'Sent '+document.getElementById('email').value\">Send</button>\
<p id=msg>ready</p>";

const HUMAN: &str = "<title>human</title><style>body{margin:0}</style>\
<div id=box style='width:30px;height:30px;border:2px solid #000;margin:20px;float:left' \
onclick=\"this.textContent='x';parent.postMessage('human','*')\"></div><span style='line-height:70px'>I am human</span>";

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
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let body = match path.as_str() {
                    "/widgets" => WIDGETS,
                    "/form" => FORM,
                    "/human" => HUMAN,
                    _ => "<title>other</title><p>other</p>",
                }
                .replace("{PORT}", &port.to_string());
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

struct Rig {
    _dir: tempfile::TempDir,
    driver: Arc<CdpDriver>,
    tools: Vec<Arc<dyn Tool>>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(CdpDriver::new(BrowserSettings::isolated(dir.path())));
        let tools = tools(driver.clone(), dir.path(), None);
        Self {
            _dir: dir,
            driver,
            tools,
        }
    }

    async fn call(&self, name: &str, input: Value) -> Result<String, ToolError> {
        let tool = self.tools.iter().find(|t| t.spec().name == name).unwrap();
        let out = tool.call(&ToolContext::default(), input).await?;
        Ok(out
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn ok(&self, name: &str, input: Value) -> String {
        self.call(name, input)
            .await
            .unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    async fn eval(&self, js: &str) -> String {
        let text = self.ok("browser_eval", json!({"js": js})).await;
        text.lines()
            .next()
            .unwrap_or_default()
            .trim_matches('"')
            .to_owned()
    }
}

/// `x=N y=M w=W h=H` of the first listed region whose line contains `needle`.
fn region(shot: &str, needle: &str) -> (f64, f64) {
    let line = shot
        .lines()
        .find(|l| l.starts_with("- ") && l.contains(needle))
        .unwrap_or_else(|| panic!("no region {needle:?} in:\n{shot}"));
    let field = |k: &str| -> f64 {
        let at = line.find(&format!("{k}=")).unwrap() + k.len() + 1;
        line[at..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    (field("x"), field("y"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome/Edge"]
#[allow(clippy::too_many_lines)] // one story, read top to bottom
async fn pointer_mode_reaches_what_has_no_ref_and_a_failed_step_keeps_the_page() {
    let port = serve().await;
    let rig = Rig::new();
    // `browser_open` only opens public addresses, so the fixture on this computer is opened
    // through the driver; everything after that goes through the model-facing tools.
    rig.driver
        .open(&format!("http://127.0.0.1:{port}/widgets"), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await; // the cross-origin frame loads

    // Screenshot grounding: the file, the picture's size, and the frames and canvas to point at.
    let shot = rig.ok("browser_screenshot", json!({})).await;
    let first = shot.lines().next().unwrap();
    assert!(first.starts_with("screenshot: "), "{shot}");
    let path = first["screenshot: ".len()..].split(" (").next().unwrap();
    assert!(std::path::Path::new(path).exists(), "{shot}");
    assert!(
        shot.contains("browser_pointer takes x,y in these pixels"),
        "{shot}"
    );
    assert!(
        shot.contains("canvas") && shot.contains("\"Human check\""),
        "{shot}"
    );

    // A radio drawn on a canvas: only a point can choose it.
    let (cx, cy) = region(&shot, "canvas");
    let out = rig
        .ok(
            "browser_pointer",
            json!({"action":"click","x": cx + 1.0 + 180.0,"y": cy + 1.0 + 30.0}),
        )
        .await;
    assert!(out.contains("under the point: <canvas"), "{out}");
    assert!(
        out.contains("\nscreenshot: "),
        "every pointer result ends with a fresh picture: {out}"
    );
    assert_eq!(
        rig.eval("document.getElementById('picked').textContent")
            .await,
        "radio-1"
    );

    // A box in a frame of another origin: the click is routed into the frame.
    let (hx, hy) = region(&shot, "\"Human check\"");
    let out = rig
        .ok(
            "browser_pointer",
            json!({"action":"click","x": hx + 37.0,"y": hy + 37.0}),
        )
        .await;
    assert!(out.contains("a frame"), "{out}");
    let mut seen = String::new();
    for _ in 0..20 {
        seen = rig
            .eval("document.getElementById('picked').textContent")
            .await;
        if seen.contains("|human") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen.contains("|human"), "{seen}");

    // The form is in a same-origin iframe: refs reach it. The first send is rejected by the page.
    let snap = rig
        .ok("browser_snapshot", json!({"interactive_only": true}))
        .await;
    let ref_of = |role: &str, name: &str| -> String {
        let line = snap
            .lines()
            .find(|l| l.contains(role) && l.contains(&format!("\"{name}\"")))
            .unwrap_or_else(|| panic!("no {role} {name:?} in:\n{snap}"));
        line.split(['[', ']']).nth(1).unwrap().to_owned()
    };
    rig.ok(
        "browser_fill",
        json!({"ref": ref_of("textbox", "Email"), "text": "ada@example.org"}),
    )
    .await;
    rig.ok("browser_click", json!({"ref": ref_of("button", "Send")}))
        .await;
    let in_form = |js: &str| format!("document.getElementById('form').contentDocument.{js}");
    assert_eq!(
        rig.eval(&in_form("getElementById('msg').textContent"))
            .await,
        "Server busy, try again"
    );

    // A step on a ref that is gone fails; the error keeps the page and says how to recover.
    let err = rig
        .call("browser_click", json!({"ref": "e9999"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("[Praxi notice] The step failed"), "{err}");
    assert!(
        err.contains(&format!("page now: http://127.0.0.1:{port}/widgets")),
        "{err}"
    );
    assert!(err.contains("Do NOT call browser_open"), "{err}");
    assert!(
        err.contains("\"Send\""),
        "the current state lists the controls: {err}"
    );
    assert!(err.contains("screenshot: "), "and a screenshot: {err}");
    // The same failure again escalates.
    let again = rig
        .call("browser_click", json!({"ref": "e9999"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(again.contains("do not send it again unchanged"), "{again}");

    // Nothing was reloaded: what was typed is still there, and the retry in place goes through.
    assert_eq!(
        rig.eval(&in_form("getElementById('email').value")).await,
        "ada@example.org"
    );
    assert_eq!(rig.eval("String(window.__marker)").await, "1");
    let snap2 = rig
        .ok("browser_snapshot", json!({"interactive_only": true}))
        .await;
    let send = snap2
        .lines()
        .find(|l| l.contains("\"Send\""))
        .and_then(|l| l.split(['[', ']']).nth(1))
        .unwrap()
        .to_owned();
    rig.ok("browser_click", json!({"ref": send})).await;
    assert_eq!(
        rig.eval(&in_form("getElementById('msg').textContent"))
            .await,
        "Sent ada@example.org"
    );

    rig.driver.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome/Edge"]
async fn attaching_to_the_owners_browser_uses_it_names_it_and_leaves_it_running() {
    use browser_cdp::{Browser, LaunchOptions};
    let port = serve().await;
    // The owner's own browser, already open on a page, with a debugging port.
    let owner = Browser::launch(LaunchOptions::default()).await.unwrap();
    let tab = owner
        .new_page(&format!("http://127.0.0.1:{port}/form"))
        .await
        .unwrap();
    tab.bring_to_front().await.unwrap();
    let endpoint = owner.ws_url().to_owned();

    let rig = Rig::new();
    // A remote address is refused before anything connects.
    let err = rig
        .call(
            "browser_attach",
            json!({"endpoint": "http://192.168.1.9:9222"}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not on this computer"), "{err}");
    assert!(rig.driver.attached().is_none());

    let out = rig
        .ok("browser_attach", json!({"endpoint": endpoint}))
        .await;
    assert!(out.contains("attached to the owner's own browser"), "{out}");
    assert!(
        out.contains("/form"),
        "the tab the owner is looking at is the one in use: {out}"
    );
    assert!(rig.driver.attached().is_some());

    // Every result now names the browser, and the page read there is the owner's.
    let snap = rig.ok("browser_snapshot", json!({})).await;
    assert!(snap.contains("[browser: the OWNER's own browser"), "{snap}");
    assert!(snap.contains("Email"), "{snap}");
    let tool = rig
        .tools
        .iter()
        .find(|t| t.spec().name == "browser_snapshot")
        .unwrap();
    let exposure = tool
        .call(&ToolContext::default(), json!({}))
        .await
        .unwrap()
        .exposure
        .expect("the owner's browser is private, untrusted content");
    assert!(exposure.reads_private && exposure.ingests_untrusted);

    // Every attach asks; detaching does not.
    let attach = rig
        .tools
        .iter()
        .find(|t| t.spec().name == "browser_attach")
        .unwrap();
    assert!(attach.always_ask_for(&json!({})));
    assert!(!attach.always_ask_for(&json!({"detach": true})));

    // Detaching only disconnects: the owner's browser and tab are still there.
    let out = rig.ok("browser_attach", json!({"detach": true})).await;
    assert!(out.contains("detached from the owner's browser"), "{out}");
    assert!(rig.driver.attached().is_none());
    assert!(!owner.is_closed());
    assert_eq!(tab.title().await.unwrap(), "form");
    owner.close().await.unwrap();
    rig.driver.shutdown().await;
}
