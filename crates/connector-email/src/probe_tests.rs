//! "Test connection": log in over TLS, never send mail, never leak.

use crate::SmtpSecurity;
use crate::probe::{ProbeStatus, probe};
use crate::testutil::{PASSWORD, Protocol, account, server};

#[tokio::test]
async fn probe_logs_in_to_both_servers_without_sending_or_reading_mail() {
    for (proto, security) in [
        (Protocol::Smtp, SmtpSecurity::Implicit),
        (Protocol::StartTls, SmtpSecurity::StartTls),
    ] {
        let imap = server(Protocol::Imap).await;
        let smtp = server(proto).await;
        let acct = account(&imap, &smtp, security);
        let report = probe(&acct, PASSWORD).await;
        assert_eq!(report.imap, ProbeStatus::Ok);
        assert_eq!(report.smtp, ProbeStatus::Ok);
        let smtp_cmds = smtp.commands.lock().unwrap().join("");
        assert!(smtp_cmds.contains("AUTH PLAIN"), "{smtp_cmds}");
        for forbidden in ["MAIL FROM", "RCPT TO", "DATA"] {
            assert!(
                !smtp_cmds.contains(forbidden),
                "{forbidden} sent: {smtp_cmds}"
            );
        }
        assert!(smtp.messages.lock().unwrap().is_empty());
        let imap_cmds = imap.commands.lock().unwrap().join("");
        assert!(imap_cmds.contains("LOGIN"), "{imap_cmds}");
        for forbidden in ["SELECT", "EXAMINE", "FETCH", "APPEND", "SEARCH"] {
            assert!(!imap_cmds.contains(forbidden), "{forbidden}: {imap_cmds}");
        }
        assert!(imap.messages.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn probe_reports_coarse_causes_and_nothing_else() {
    let smtp = server(Protocol::Smtp).await;

    // Rejected login.
    let imap = server(Protocol::RejectLogin).await;
    let acct = account(&imap, &smtp, SmtpSecurity::Implicit);
    let report = probe(&acct, PASSWORD).await;
    assert_eq!(report.imap, ProbeStatus::AuthFailed);
    assert_eq!(report.smtp, ProbeStatus::Ok);

    // Nothing listening.
    let imap = server(Protocol::Imap).await;
    let mut acct = account(&imap, &smtp, SmtpSecurity::Implicit);
    let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    acct.imap.port = closed.local_addr().unwrap().port();
    drop(closed);
    acct.imap.host = "127.0.0.1".into(); // `localhost` would also try ::1, slowly, on Windows
    assert_eq!(probe(&acct, PASSWORD).await.imap, ProbeStatus::Unreachable);

    // Certificate not trusted: no private CA configured.
    let mut acct = account(&imap, &smtp, SmtpSecurity::Implicit);
    acct.extra_root_pem = None;
    let report = probe(&acct, PASSWORD).await;
    assert_eq!(report.imap, ProbeStatus::Tls);
    assert_eq!(report.smtp, ProbeStatus::Tls);

    // A server that never answers.
    let stall = server(Protocol::Stall).await;
    let mut acct = account(&stall, &smtp, SmtpSecurity::Implicit);
    acct.timeout_secs = 1;
    assert_eq!(probe(&acct, PASSWORD).await.imap, ProbeStatus::Timeout);

    // STARTTLS required but not offered.
    let no_tls = server(Protocol::NoStartTls).await;
    let imap = server(Protocol::Imap).await;
    let mut acct = account(&imap, &no_tls, SmtpSecurity::StartTls);
    acct.smtp.host = "127.0.0.1".into(); // see above: `localhost` is slow here
    assert_eq!(probe(&acct, PASSWORD).await.smtp, ProbeStatus::Protocol);

    // The report carries a status per server and nothing that came off the wire
    // or out of the request.
    let json = serde_json::to_string(&report).unwrap();
    assert!(
        !json.contains(PASSWORD) && !json.contains("localhost"),
        "{json}"
    );
    assert_eq!(json, r#"{"imap":"tls","smtp":"tls"}"#);
}
