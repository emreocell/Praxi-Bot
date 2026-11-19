//! `CdpDriver` batch path against a real Chrome on local `data:` pages (no network).
//! Run with `cargo test -p builtin-tools --test browser_act_chrome -- --ignored --test-threads=1`
//! and `PRAXI_BROWSER_PATH` pointing at Chrome (the CI browser job does this).

use std::fmt::Write as _;

use builtin_tools::{ActStep, BrowserDriver, BrowserSettings, CdpDriver};

const PAGE: &str = r#"<html><body>
<label>Name <input id=name type=text></label>
<label>Password <input id=pw type=password></label>
<button id=go onclick="window.n=(window.n||0)+1">Go</button>
<button id=swap onclick="document.getElementById('victim').textContent='Delete account'">Swap</button>
<button id=victim onclick="window.v=1">Harmless</button>
<button id=next onclick="location.href='about:blank?next'">Next</button>
</body></html>"#;

fn data_url(html: &str) -> String {
    let mut out = String::from("data:text/html;charset=utf-8,");
    for b in html.bytes() {
        if b.is_ascii_alphanumeric() {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `[e12] button "Go"` -> `e12`.
fn ref_in(snapshot: &str, role: &str, name: &str) -> String {
    let want = format!("{role} \"{name}\"");
    snapshot
        .lines()
        .find(|l| l.contains(&want))
        .and_then(|l| l.split(['[', ']']).nth(1))
        .map_or_else(|| panic!("no {want} in:\n{snapshot}"), str::to_string)
}

#[tokio::test]
#[ignore = "needs Chrome/Edge"]
async fn the_real_driver_verifies_each_step_and_stops_on_a_changed_target() {
    let dir = tempfile::tempdir().unwrap();
    let d = CdpDriver::new(BrowserSettings::isolated(dir.path()));
    let page_view = d.open(&data_url(PAGE), None).await.unwrap();
    let name = ref_in(&page_view, "textbox", "Name");
    let pw = ref_in(&page_view, "textbox", "Password");
    let go = ref_in(&page_view, "button", "Go");
    let swapper = ref_in(&page_view, "button", "Swap");
    let victim = ref_in(&page_view, "button", "Harmless");
    let next = ref_in(&page_view, "button", "Next");

    // Facts: only the password field is sensitive.
    let facts = d.ref_facts(&[name.clone(), pw.clone()]).await;
    assert_eq!(facts[0].as_ref().unwrap().name, "Name");
    assert!(!facts[0].as_ref().unwrap().sensitive);
    assert!(facts[1].as_ref().unwrap().sensitive);

    let ids = d
        .act_begin(&[
            name.clone(),
            go.clone(),
            swapper.clone(),
            victim.clone(),
            next.clone(),
        ])
        .await
        .unwrap();
    assert!(ids.iter().all(Option::is_some), "{ids:?}");

    // Fill and click run against the identity recorded at the start.
    let fill = ActStep::Fill {
        r: name.clone(),
        text: "Ada".into(),
    };
    d.act_step(&fill, ids[0].as_deref()).await.unwrap();
    let click = ActStep::Click {
        r: go.clone(),
        checked: None,
    };
    let out = d.act_step(&click, ids[1].as_deref()).await.unwrap();
    assert!(!out.url_changed);
    let n = d.eval("window.n").await.unwrap();
    assert_eq!(n.as_i64(), Some(1), "the click ran once");

    // Step 3 makes the page rename the button of step 4 into something else: step 4 is
    // refused against the fingerprint taken at the start and nothing reaches that element.
    let swap_step = ActStep::Click {
        r: swapper.clone(),
        checked: None,
    };
    d.act_step(&swap_step, ids[2].as_deref()).await.unwrap();
    let victim_step = ActStep::Click {
        r: victim.clone(),
        checked: None,
    };
    let err = d
        .act_step(&victim_step, ids[3].as_deref())
        .await
        .expect_err("a renamed target must stop the batch");
    assert!(err.contains(&victim), "{err}");
    let v = d.eval("window.v === undefined").await.unwrap();
    assert_eq!(
        v.as_bool(),
        Some(true),
        "nothing was clicked on the changed target"
    );

    // A step that navigates reports it, so the batch can stop there.
    let nav = ActStep::Click {
        r: next.clone(),
        checked: None,
    };
    let out = d.act_step(&nav, ids[4].as_deref()).await.unwrap();
    assert!(out.url_changed);
    assert_ne!(d.act_end().await.unwrap(), "");
}
