//! Assertions over what the run left behind: nobody was asked, the vault and the ledger agree
//! with what the sites saw, the owner heard exactly once, and nothing crossed a line it must not.

use std::path::Path;
use std::time::{Duration, Instant};

use channel_api::notice::{Flavor, NoticeKind, NoticeLang, NoticeSource, OwnerNotice, render};
use missions::Item;
use serde_json::Value;

use crate::sites::{CAPTCHA, PLAIN, TWOFA, VERIFY};
use crate::world::World;

/// Event types that are a card, a question or a hand-over to the owner (or a failure).
const NOT_EXPECTED: [&str; 5] = [
    "approval_needed",
    "question_asked",
    "secret_requested",
    "tool_call_denied",
    "error",
];

fn files_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

impl World {
    /// Every account the bot made is its own, holds the password the site was given and is bound
    /// to the bot's mailbox; only the site that asked for two-factor holds a key.
    pub fn assert_vault_holds_the_accounts(&self) {
        let bot = self.bot.to_string();
        let accounts = self.fill.accounts(Some(&bot)).unwrap();
        assert!(
            accounts
                .iter()
                .all(|a| a.created_by == "bot" && a.has_value && a.kind == "password"),
            "{accounts:#?}"
        );
        eprintln!(
            "PROBE ACCOUNTS {:?}",
            accounts
                .iter()
                .map(|a| (a.site.clone(), a.unused, a.has_totp))
                .collect::<Vec<_>>()
        );
        let book = self.sites.book.lock();
        for host in [PLAIN, VERIFY, TWOFA] {
            let mine: Vec<_> = accounts.iter().filter(|a| a.site.contains(host)).collect();
            assert_eq!(mine.len(), 1, "one account for {host}: {accounts:#?}");
            let a = mine[0];
            assert_eq!(
                a.login_id,
                self.mail.address(),
                "{host}: the bot's own address"
            );
            assert_eq!(a.has_totp, host == TWOFA, "{host}: two-factor key");
            assert!(!a.unused, "{host}: the password was typed");
            let stored = self.fill.resolve_value_for(&a.name, Some(&bot)).unwrap();
            let sent = book.signups.iter().find(|s| s.0 == host).unwrap();
            assert_eq!(
                stored.expose(),
                sent.2,
                "{host}: the site got the vault's password"
            );
        }
        // The CAPTCHA site never got a sign-up, so there is nothing to keep two-factor for.
        assert!(
            accounts
                .iter()
                .filter(|a| a.site.contains(CAPTCHA))
                .all(|a| !a.has_totp)
        );
        assert!(accounts.len() <= 4, "{accounts:#?}");
    }

    /// No card, question, secret request, hand-over, denial or error anywhere, and no tool call
    /// that failed. A Full bot's unattended mission has nobody to ask.
    pub fn assert_nobody_was_asked(&self) {
        let events: Vec<Value> = self.events.lock().clone();
        for e in &events {
            let ev = &e["event"];
            let kind = ev["type"].as_str().unwrap_or_default();
            assert!(
                !NOT_EXPECTED.contains(&kind),
                "unexpected {kind}: {ev}\n{}",
                self.diagnose()
            );
        }
        let called: Vec<String> = events
            .iter()
            .filter(|e| e["event"]["type"] == "tool_call_requested")
            .filter_map(|e| e["event"]["tool_name"].as_str().map(str::to_owned))
            .collect();
        for tool in [
            "mission_create",
            "mission_plan",
            "mission_item",
            "mission_wait",
            "account_register",
            "account_totp_save",
            "browser_open",
            "browser_fill",
            "browser_click",
            "email_wait_for",
        ] {
            assert!(
                called.iter().any(|c| c == tool),
                "{tool} never ran: {called:?}"
            );
        }
        let audit = self.storage.list_audit(self.user).unwrap();
        let parked: Vec<_> = audit
            .iter()
            .filter(|a| a.action == "handover.parked")
            .collect();
        assert_eq!(parked.len(), 1, "{parked:#?}");
        assert_eq!(parked[0].detail["item"], CAPTCHA);
        assert_eq!(parked[0].detail["kind"], "captcha");
        for a in &audit {
            let decided = a.action.starts_with("approval.") || a.action.starts_with("secret.");
            assert!(
                !decided || a.action == "approval.auto_allowed",
                "an approval was decided: {} {}",
                a.action,
                a.detail
            );
        }
    }

    /// The owner is told once: one terminal notice of the mission (and the same one on the push
    /// stream the bridges read), with the counts and the reason the CAPTCHA site was given up.
    pub async fn assert_one_report(&self, id: &str, items: &[Item]) -> OwnerNotice {
        // The notice is published right after the mission ends.
        let t0 = Instant::now();
        let notices = loop {
            let (_, n) = self.api("GET", "/v1/owner-notices", None).await;
            let all: Vec<OwnerNotice> = serde_json::from_value(n).unwrap();
            let terminal = all
                .iter()
                .filter(|n| n.kind != NoticeKind::Progress)
                .count();
            if terminal > 0 || t0.elapsed() > Duration::from_secs(10) {
                break all;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        tokio::time::sleep(Duration::from_millis(300)).await; // a second one would be late
        let (_, n) = self.api("GET", "/v1/owner-notices", None).await;
        let notices: Vec<OwnerNotice> = serde_json::from_value(n).unwrap_or(notices);
        let report = notices
            .iter()
            .find(|n| n.kind == NoticeKind::Finished)
            .unwrap_or_else(|| panic!("mission finished notice: {notices:#?}"));
        assert_eq!(report.source, NoticeSource::Mission);
        assert_eq!(report.mission_id.as_deref(), Some(id));
        assert_eq!(report.bot_name, "Ada");
        let p = report.progress.expect("the report carries the counts");
        assert_eq!(
            (p.total, p.done, p.failed, p.blocked, p.skipped),
            (4, 3, 0, 1, 0),
            "{report:#?}"
        );
        assert!(
            !report.failed.is_empty() || !report.app_only.is_empty(),
            "the CAPTCHA appears in the report: {report:#?}"
        );
        assert!(report.questions.is_empty(), "{report:#?}");
        assert_eq!(
            items.iter().filter(|i| i.status.as_str() == "done").count(),
            3
        );

        // Telegram and Discord render the very same notice, in either language.
        for lang in [NoticeLang::En, NoticeLang::Tr] {
            for flavor in [Flavor::Plain, Flavor::Discord] {
                let text = render(report, lang, flavor);
                assert!(text.contains('3') && text.contains("CAPTCHA"), "{text}");
            }
        }

        // The final report is in the mission's own chat, once, and says the same.
        let (_, detail) = self.api("GET", &format!("/v1/missions/{id}"), None).await;
        let chat = protocol::ChatId(
            detail["mission"]["chat_id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
        );
        let reports: Vec<String> = self
            .storage
            .list_messages(chat)
            .unwrap()
            .into_iter()
            .filter(|m| m.role == "assistant" && m.content.starts_with("Mission finished"))
            .map(|m| m.content)
            .collect();
        assert_eq!(reports.len(), 1, "{reports:#?}");
        for part in ["3 done", "of 4", "CAPTCHA", CAPTCHA] {
            assert!(
                reports[0].contains(part),
                "the report lacks {part:?}: {}",
                reports[0]
            );
        }
        let digest = &detail["digest"];
        assert_eq!(digest["outcome"], "done", "{digest}");
        assert_eq!(
            digest["done_sample"].as_array().map(Vec::len),
            Some(3),
            "{digest}"
        );

        let pushed = self.pushed_notices().await;
        assert!(
            pushed.iter().any(|n| n.id == report.id),
            "the finished report was pushed: {pushed:#?}"
        );
        report.clone()
    }

    /// Everything the owner's channels carry: the notices as the bridges render them, and what
    /// the bot said to the owner in the owner's chat and in the mission's chat.
    pub async fn owner_channel_text(&self, mission_chat: &str) -> String {
        let mut out = String::new();
        let (_, n) = self.api("GET", "/v1/owner-notices", None).await;
        let notices: Vec<OwnerNotice> = serde_json::from_value(n).unwrap();
        for n in &notices {
            out += &serde_json::to_string(n).unwrap();
            for lang in [NoticeLang::En, NoticeLang::Tr] {
                for flavor in [Flavor::Plain, Flavor::Discord] {
                    out += &render(n, lang, flavor);
                }
            }
        }
        for chat in [self.chat.to_string(), mission_chat.to_owned()] {
            let chat = protocol::ChatId(chat.parse().unwrap());
            for m in self.storage.list_messages(chat).unwrap() {
                if m.role == "assistant" {
                    out += &m.content;
                }
            }
        }
        out
    }

    /// Text from a site (a hostile note, the evil host) must not reach the owner's channels.
    pub async fn assert_no_leaks(&self, mission_chat: &str, markers: &[(&str, &str)]) {
        let text = self.owner_channel_text(mission_chat).await;
        assert!(
            text.len() > 200,
            "the owner's channels carry something: {text}"
        );
        for (what, marker) in markers {
            assert!(
                !text.contains(marker),
                "{what} ({marker}) reached the owner's channels:\n{text}"
            );
        }
    }

    /// A secret (a password, the two-factor key, the mail provider's key) appears nowhere the
    /// bot, the owner or an operator can read: not in a model request, an event, a stored
    /// message, an audit row, a notice, the ledger, or any file the daemon wrote.
    pub async fn assert_secrets_stay_secret(&self, id: &str, secrets: &[(String, String)]) {
        let mut surfaces: Vec<(String, String)> = Vec::new();
        surfaces.push((
            "a model request".into(),
            self.model
                .requests()
                .iter()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .collect::<String>(),
        ));
        surfaces.push((
            "an event".into(),
            self.events.lock().iter().map(Value::to_string).collect(),
        ));
        surfaces.push((
            "the audit log".into(),
            self.storage
                .list_audit(self.user)
                .unwrap()
                .iter()
                .map(|a| format!("{} {}", a.action, a.detail))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        let (_, detail) = self.api("GET", &format!("/v1/missions/{id}"), None).await;
        surfaces.push(("the mission".into(), detail.to_string()));
        let (_, notes) = self.api("GET", "/v1/owner-notices", None).await;
        surfaces.push(("a notice".into(), notes.to_string()));
        let mission_chat = detail["mission"]["chat_id"].as_str().unwrap().to_owned();
        for chat in [self.chat.to_string(), mission_chat] {
            let chat = protocol::ChatId(chat.parse().unwrap());
            surfaces.push((
                "a stored message".into(),
                self.storage
                    .list_messages(chat)
                    .unwrap()
                    .iter()
                    .map(|m| m.content.clone())
                    .collect(),
            ));
        }
        let mut files = Vec::new();
        files_under(self.dir.path(), &mut files);
        assert!(!files.is_empty(), "the run left no files to scan");
        for f in files {
            surfaces.push((
                format!("the file {}", f.display()),
                String::from_utf8_lossy(&std::fs::read(&f).unwrap_or_default()).into_owned(),
            ));
        }
        for (what, secret) in secrets {
            assert!(secret.len() >= 12, "a real secret: {what}");
            for (surface, text) in &surfaces {
                assert!(
                    !text.contains(secret.as_str()),
                    "{what} leaked into {surface}"
                );
            }
        }
    }
}
