//! A match from spam never completes a verification wait on its own: the sender must be on the
//! site the wait is bound to AND the receiving server must have vouched for that domain. A forged
//! `From` in a spam folder is reported as a junk candidate (no code, no link), not handed out.

use std::sync::Arc;
use std::time::Duration;

use crate::EmailConnector;
use crate::fake_imap::{FakeImap, date_ago};
use crate::message::parse_detail;
use crate::wait::{
    WaitBinding, WaitEnd, WaitFilter, WaitOwner, WaitPlan, WaitProgress, Waiters, found_json,
    link_basis, timed_out_json, wait_for_mail,
};

const BOT: &str = "11111111-1111-4111-8111-111111111111";
const SITE: &str = "https://site.test:443";

async fn rig() -> (FakeImap, Arc<EmailConnector>) {
    let imap = FakeImap::start(&[
        ("INBOX", None),
        ("Spam", Some("\\Junk")),
        ("Drafts", Some("\\Drafts")),
    ])
    .await;
    let conn = imap.connector(imap.account("own", Some(BOT), 1, ""));
    (imap, conn)
}

fn plan() -> WaitPlan {
    WaitPlan {
        timeout: Duration::from_secs(2),
        max_age: Duration::from_secs(900),
        since: None,
        first_pause: Duration::from_millis(40),
        max_pause: Duration::from_millis(80),
    }
}

/// A verification mail with `extra` header lines (each ending in CRLF).
fn mail(from: &str, extra: &str, id: &str, code: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: bot@example.com\r\nSubject: Verify your address\r\nMessage-ID: <{id}>\r\n{extra}Date: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Your code is {code}</p><a href=\"https://www.site.test/verify?t=tok-{code}\">Verify</a>\r\n",
        date_ago(20)
    )
    .into_bytes()
}

const PASS: &str = "Authentication-Results: mx.example.com; dkim=pass header.d=site.test; spf=pass smtp.mailfrom=bounce.site.test\r\n";

fn owner() -> WaitOwner<'static> {
    WaitOwner {
        bot: Some(BOT),
        run: "run-1",
        account: "own",
    }
}

/// Run one wait bound to `origin` to its end.
async fn run_wait(conn: &Arc<EmailConnector>, origin: Option<&str>) -> WaitProgress {
    let waiters = Waiters::new();
    // No sender filter: the site test below is the wait's own, not the model's.
    let filter = WaitFilter::default();
    let id = waiters
        .start(
            conn,
            owner(),
            &filter,
            &plan(),
            WaitBinding { origin, site: None },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match waiters
                .progress(
                    &id,
                    owner(),
                    Duration::from_millis(200),
                    std::future::pending(),
                )
                .await
            {
                WaitProgress::Pending { .. } => {}
                done => return done,
            }
        }
    })
    .await
    .expect("the wait ends")
}

fn candidate_of(p: WaitProgress) -> crate::JunkCandidate {
    match p {
        WaitProgress::Finished(WaitEnd::TimedOut {
            junk_candidate: Some(c),
            ..
        }) => c,
        other => panic!("expected a timed-out wait with a junk candidate, got {other:?}"),
    }
}

#[tokio::test]
async fn a_forged_sender_in_spam_is_a_candidate_and_never_hands_out_its_code_or_link() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "Spam",
        mail("Site <no-reply@site.test>", "", "s1@x", "111111"),
    );
    let c = candidate_of(run_wait(&conn, Some(SITE)).await);
    assert_eq!(c.folder, "Spam");
    assert!(c.reason.contains("SPF or DKIM"), "{}", c.reason);
    let said = timed_out_json(3, Duration::from_secs(2), Some(&c)).to_string();
    assert!(said.contains("junk_candidate"), "{said}");
    assert!(said.contains("Spam"), "{said}");
    assert!(!said.contains("111111"), "no code: {said}");
    assert!(!said.contains("tok-"), "no link: {said}");
    assert!(!said.contains("\"found\":true"), "{said}");
}

#[tokio::test]
async fn the_real_mail_in_the_inbox_beats_a_forged_one_in_spam() {
    let (imap, conn) = rig().await;
    // The forgery came first and carries a code of its own.
    imap.deliver(
        "Spam",
        mail("Site <no-reply@site.test>", "", "s1@x", "111111"),
    );
    imap.deliver(
        "INBOX",
        mail("Site <no-reply@site.test>", "", "i1@x", "222222"),
    );
    let WaitProgress::Finished(WaitEnd::Found(d)) = run_wait(&conn, Some(SITE)).await else {
        panic!("the inbox mail is found")
    };
    assert_eq!(d.from_junk, Some(false));
    let v = found_json(&d, "own", &link_basis(None, Some(SITE)), &[]);
    assert_eq!(v["codes"][0], "222222", "{v}");
}

#[tokio::test]
async fn spam_from_the_site_that_its_server_vouched_for_completes_the_wait() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "Spam",
        mail("Site <no-reply@mail.site.test>", PASS, "s2@x", "333333"),
    );
    let WaitProgress::Finished(WaitEnd::Found(d)) = run_wait(&conn, Some(SITE)).await else {
        panic!("a vouched-for mail from the site's own domain is found")
    };
    assert_eq!(d.from_junk, Some(true));
    assert_eq!(d.found_in.as_deref(), Some("Spam"));
    let v = found_json(&d, "own", &link_basis(None, Some(SITE)), &[]);
    assert_eq!(v["codes"][0], "333333", "{v}");
    assert_eq!(v["from_junk"], true);
    assert!(v["junk_note"].as_str().is_some());
}

#[tokio::test]
async fn spam_stays_a_candidate_unless_site_and_authentication_both_hold() {
    let fail = "Authentication-Results: mx.example.com; dkim=fail header.d=site.test; spf=softfail smtp.mailfrom=site.test\r\n";
    let other_domain = "Authentication-Results: mx.example.com; dkim=pass header.d=evil.test; spf=pass smtp.mailfrom=evil.test\r\n";
    // A header the sender wrote itself sits below the one the receiving server added.
    let planted = format!(
        "Authentication-Results: mx.example.com; dkim=fail header.d=site.test; spf=fail smtp.mailfrom=site.test\r\n{PASS}"
    );
    let cases: Vec<(&str, Vec<u8>, Option<&str>, &str)> = vec![
        // From the site, but the server says it failed.
        (
            "failed",
            mail("S <a@site.test>", fail, "c1@x", "444441"),
            Some(SITE),
            "SPF or DKIM",
        ),
        // It passed, but for the sender's own domain, not the site's.
        (
            "other site",
            mail("S <a@evil.test>", other_domain, "c2@x", "444442"),
            Some(SITE),
            "domain",
        ),
        // Vouched for, but the From is not on the site.
        (
            "off-site sender",
            mail("S <a@evil.test>", PASS, "c3@x", "444443"),
            Some(SITE),
            "domain",
        ),
        // A From that only ends like the site.
        (
            "look-alike",
            mail("S <a@notsite.test>", PASS, "c4@x", "444444"),
            Some(SITE),
            "domain",
        ),
        // A pass for a domain that is not the From's.
        (
            "misaligned pass",
            mail("S <a@site.test>", other_domain, "c5@x", "444445"),
            Some(SITE),
            "SPF or DKIM",
        ),
        // Only the topmost header counts.
        (
            "planted below",
            mail("S <a@site.test>", &planted, "c6@x", "444446"),
            Some(SITE),
            "SPF or DKIM",
        ),
        // No page was open: there is no site to match.
        (
            "no site",
            mail("S <a@site.test>", PASS, "c7@x", "444447"),
            None,
            "no verified sign-up site",
        ),
    ];
    for (name, raw, origin, reason) in cases {
        let (imap, conn) = rig().await;
        imap.deliver("Spam", raw);
        let c = candidate_of(run_wait(&conn, origin).await);
        assert!(c.reason.contains(reason), "{name}: {}", c.reason);
        assert_eq!(c.folder, "Spam", "{name}");
    }
}

#[tokio::test]
async fn a_plain_wait_without_a_site_never_takes_a_mail_from_spam() {
    let (imap, conn) = rig().await;
    imap.deliver(
        "Spam",
        mail("Site <no-reply@site.test>", PASS, "s5@x", "555555"),
    );
    let filter = WaitFilter::new(Some("site.test"), None, None).unwrap();
    let end = wait_for_mail(
        &conn.scoped(Some(BOT)),
        Some("own"),
        &filter,
        &plan(),
        std::future::pending(),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            end,
            WaitEnd::TimedOut {
                junk_candidate: Some(_),
                ..
            }
        ),
        "{end:?}"
    );
}

/// The one definition of a junk folder is also the one that tags a found mail.
#[tokio::test]
async fn a_mail_found_in_the_inbox_is_not_junk_whatever_the_folder_is_called() {
    let imap = FakeImap::start(&[("INBOX", None), ("Archive", None)]).await;
    let conn = imap.connector(imap.account("own", Some(BOT), 1, ""));
    imap.deliver(
        "INBOX",
        mail("Site <no-reply@site.test>", "", "i2@x", "666666"),
    );
    let WaitProgress::Finished(WaitEnd::Found(d)) = run_wait(&conn, Some(SITE)).await else {
        panic!("found")
    };
    assert_eq!(d.from_junk, Some(false));
}

// ---- What the receiving server vouched for -------------------------------------------------------

fn auth_of(from: &str, header: &str) -> Option<bool> {
    let raw = mail(from, header, "p@x", "123456");
    parse_detail(1, &raw, false, None, 1000)
        .unwrap()
        .sender_authenticated
}

#[test]
fn authentication_results_are_read_from_the_topmost_header_for_an_aligned_domain() {
    // No header: the provider reports nothing.
    assert_eq!(auth_of("a@site.test", ""), None);
    // DKIM, SPF and DMARC, each for an aligned domain.
    assert_eq!(
        auth_of(
            "a@site.test",
            "Authentication-Results: mx; dkim=pass header.d=site.test\r\n"
        ),
        Some(true)
    );
    assert_eq!(
        auth_of(
            "a@site.test",
            "Authentication-Results: mx; spf=pass smtp.mailfrom=bounces@mail.site.test\r\n"
        ),
        Some(true)
    );
    assert_eq!(
        auth_of(
            "a@site.test",
            "Authentication-Results: mx; dmarc=pass header.from=site.test\r\n"
        ),
        Some(true)
    );
    // A folded header (the usual layout) reads the same.
    assert_eq!(
        auth_of(
            "a@site.test",
            "Authentication-Results: mx.example.com;\r\n  spf=none smtp.mailfrom=site.test;\r\n  dkim=pass header.d=site.test header.s=k1\r\n"
        ),
        Some(true)
    );
    // Reported, nothing passed for the From's domain.
    for header in [
        "Authentication-Results: mx; dkim=fail header.d=site.test\r\n",
        "Authentication-Results: mx; spf=pass smtp.mailfrom=evil.test\r\n",
        "Authentication-Results: mx; dkim=pass header.d=evil.test; dmarc=fail header.from=site.test\r\n",
        "Authentication-Results: mx; dkim=pass header.d=com\r\n",
        "Authentication-Results: mx; dkim=pass\r\n",
        "Authentication-Results: mx; dkim=pass header.d=notsite.test\r\n",
    ] {
        assert_eq!(auth_of("a@site.test", header), Some(false), "{header}");
    }
    // The topmost header decides; one planted below it does not help.
    assert_eq!(
        auth_of(
            "a@site.test",
            "Authentication-Results: mx; dkim=fail header.d=site.test\r\nAuthentication-Results: mx; dkim=pass header.d=site.test\r\n"
        ),
        Some(false)
    );
}
