//! The owner digest: what the owner reads when a mission ends or blocks.
//!
//! Built from the ledger by code (numbers and the bot's own recorded reasons),
//! never by asking the model to summarise, so it cannot claim more than the
//! ledger shows.

use std::fmt::Write as _;

use crate::error::Result;
use crate::model::{Digest, DigestItem, Item, ItemStatus, Mission, clip};
use crate::store::MissionStore;

/// How many finished items the digest samples.
const DONE_SAMPLE: usize = 5;
/// How many blocked / failed items it lists in full.
const LIST_LIMIT: usize = 50;

fn digest_item(it: &Item, text: Option<&String>) -> DigestItem {
    DigestItem {
        key: it.key.clone(),
        title: it.title.clone(),
        text: text.map(|t| clip(t, 300)),
    }
}

/// Build the digest for `mission` as it stands (`outcome` is `done`, `blocked`,
/// `failed` or `cancelled`).
///
/// # Errors
/// SQLite failure.
pub fn build(store: &MissionStore, mission: &Mission, outcome: &str, now: i64) -> Result<Digest> {
    let items = store.items(&mission.id, None)?;
    let of = |s: ItemStatus| items.iter().filter(move |i| i.status == s);
    let decisions = store
        .notes(&mission.id, false)?
        .into_iter()
        .filter(|n| n.kind == "decision")
        .map(|n| n.text)
        .collect();
    Ok(Digest {
        at_ms: now,
        outcome: outcome.to_owned(),
        reason: mission.state_reason.clone(),
        progress: store.progress(&mission.id)?,
        blocked: of(ItemStatus::Blocked)
            .take(LIST_LIMIT)
            .map(|i| digest_item(i, i.blocked_reason.as_ref()))
            .collect(),
        failed: of(ItemStatus::Failed)
            .take(LIST_LIMIT)
            .map(|i| digest_item(i, i.last_error.as_ref()))
            .collect(),
        done_sample: of(ItemStatus::Done)
            .take(DONE_SAMPLE)
            .map(|i| digest_item(i, i.result.as_ref()))
            .collect(),
        decisions,
        sessions: mission.sessions,
        model_steps: mission.model_steps,
        cost_usd: mission.cost_usd,
    })
}

/// The digest as a short plain-text message for the mission's chat. The
/// desktop app shows the structured digest in the user's language; this text
/// is for chats and channels, so its first line carries both English and
/// Turkish.
#[must_use]
pub fn render_text(mission: &Mission, d: &Digest) -> String {
    let headline = match d.outcome.as_str() {
        "done" => "Mission finished / Görev tamamlandı",
        "blocked" => "Mission needs you / Görev senden bir şey bekliyor",
        "failed" => "Mission failed / Görev başarısız oldu",
        "cancelled" => "Mission stopped / Görev durduruldu",
        _ => "Mission update / Görev durumu",
    };
    let p = d.progress;
    let mut s = format!("{headline}: {}\n", clip(&mission.title, 80));
    let _ = writeln!(
        s,
        "{} done, {} blocked, {} to do, {} given up, {} skipped (of {}). {} sessions, {} model calls.",
        p.done,
        p.blocked,
        p.open(),
        p.failed,
        p.skipped,
        p.total,
        d.sessions,
        d.model_steps
    );
    if let Some(r) = &d.reason {
        let _ = writeln!(s, "Why: {r}");
    }
    if !d.blocked.is_empty() {
        s.push_str("\nNeeds you:\n");
        for b in &d.blocked {
            let _ = writeln!(s, "- {}: {}", b.key, b.text.as_deref().unwrap_or("?"));
        }
    }
    if !d.failed.is_empty() {
        s.push_str("\nGiven up:\n");
        for f in d.failed.iter().take(10) {
            let _ = writeln!(
                s,
                "- {}: {}",
                f.key,
                f.text.as_deref().unwrap_or("no result")
            );
        }
    }
    if !d.decisions.is_empty() {
        s.push_str("\nDecisions I made:\n");
        for t in d.decisions.iter().take(10) {
            let _ = writeln!(s, "- {t}");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Budgets;
    use crate::store::{ItemUpdate, NewItem, NewMission};

    #[test]
    fn digest_lists_blocked_failed_and_decisions_from_the_ledger() {
        let s = MissionStore::open_in_memory().unwrap();
        let items = ["a.com", "b.com", "c.com"].map(|k| NewItem {
            key: k.into(),
            kind: "site".into(),
            title: format!("Sign up {k}"),
            payload: None,
        });
        let m = s
            .create(
                &NewMission {
                    user_id: "u".into(),
                    bot_id: "b".into(),
                    chat_id: "c".into(),
                    title: "Backlinks".into(),
                    goal_text: "g".into(),
                    target_done: None,
                    budgets: Budgets::default(),
                    items: items.into(),
                },
                1,
            )
            .unwrap();
        let upd = |st, r: &str| ItemUpdate {
            status: Some(st),
            result: Some(r.into()),
            error: (st == ItemStatus::Failed).then(|| r.to_owned()),
            blocked_reason: (st == ItemStatus::Blocked).then(|| r.to_owned()),
        };
        s.update_item(
            &m.id,
            "a.com",
            &upd(ItemStatus::Done, "account created"),
            false,
            2,
        )
        .unwrap();
        s.update_item(
            &m.id,
            "b.com",
            &upd(ItemStatus::Blocked, "CAPTCHA"),
            false,
            2,
        )
        .unwrap();
        s.update_item(
            &m.id,
            "c.com",
            &upd(ItemStatus::Failed, "needs payment"),
            false,
            2,
        )
        .unwrap();
        s.add_note(&m.id, "decision", "used the bot mailbox", true, 3)
            .unwrap();
        let m = s.get(&m.id).unwrap().unwrap();
        let d = build(&s, &m, "blocked", 4).unwrap();
        assert_eq!(
            (d.progress.done, d.progress.blocked, d.progress.failed),
            (1, 1, 1)
        );
        assert_eq!(d.blocked[0].text.as_deref(), Some("CAPTCHA"));
        assert_eq!(d.failed[0].text.as_deref(), Some("needs payment"));
        assert_eq!(d.done_sample[0].text.as_deref(), Some("account created"));
        assert_eq!(d.decisions, ["used the bot mailbox"]);
        let text = render_text(&m, &d);
        assert!(text.contains("Needs you:") && text.contains("b.com: CAPTCHA"));
        assert!(text.contains("Görev"));
    }
}
