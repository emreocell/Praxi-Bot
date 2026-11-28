//! Outreach safety on top of the send log: matching what comes back to what we sent, acting
//! only on a confident match, and a hard daily budget per mailbox.
//!
//! A reply or a delivery report is matched to a log entry by the `Message-ID` we wrote into
//! the mail (found in `In-Reply-To` / `References`, or quoted in a bounce's returned headers) or
//! by the provider's thread id, and always within the same mailbox. Even then the sender must be
//! one of the addresses the matched mail went to: a stranger who knows the id cannot suppress
//! somebody else. What the reply says is untrusted text; it only ever decides between a few
//! fixed actions (suppress the very address that wrote, count a soft bounce, name a follow-up
//! date) and never carries an instruction.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{MailGuard, SendLogEntry, WARMUP_START, key_of, lock, now_secs};
use crate::dsn::DeliveryStatus;
use crate::reply_classification::{ReplyClass, classify_reply, stated_return_date};

/// When an out-of-office reply states no (believable) return date: follow up this much later.
pub const DEFAULT_FOLLOW_UP_SECS: u64 = 3 * 86_400;
/// Most message ids read from one reply's `In-Reply-To` and `References`.
pub(crate) const MAX_REFS: usize = 50;
/// Most recipient blocks of one delivery report acted on.
const MAX_DSN_RECIPIENTS: usize = 20;
/// Inbound message ids remembered per address, so one report is never counted twice.
const SEEN_KEEP: usize = 10;
/// Most addresses with a soft-bounce count.
const MAX_SOFT_BOUNCE_ADDRESSES: usize = 20_000;

/// Soft bounces of one address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoftBounce {
    /// Temporary delivery failures reported so far.
    pub count: u32,
    /// Unix seconds of the last one.
    pub last_at: u64,
    /// The `Message-ID` of the mail we sent that the last one was about.
    pub last_message_id: String,
    /// Ids of the latest reports counted (a report seen twice counts once).
    pub seen: Vec<String>,
}

/// What is saved in `outreach-state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutreachState {
    /// Soft bounces by address (the tag-free key).
    pub(super) soft_bounces: BTreeMap<String, SoftBounce>,
    /// Unix seconds of the first mail each mailbox sent: the start of its warm-up.
    pub(super) warmup_start: BTreeMap<String, u64>,
}

/// A reply or delivery report as the matcher sees it. All text is untrusted.
#[derive(Debug, Clone)]
pub struct InboundMail<'a> {
    /// The mailbox it arrived in.
    pub account: &'a str,
    /// Its own id (the `Message-ID` header or the provider's id): the same mail is counted once.
    pub id: &'a str,
    /// Bare sender address.
    pub from_addr: &'a str,
    /// Subject.
    pub subject: &'a str,
    /// Plain-text body.
    pub body: &'a str,
    /// The `Auto-Submitted` header, when the provider shows it.
    pub auto_submitted: Option<&'a str>,
    /// `In-Reply-To` and `References` ids, without angle brackets.
    pub references: &'a [String],
    /// The provider's thread id, when it has one.
    pub thread_id: Option<&'a str>,
    /// The delivery report, when the mail is one.
    pub dsn: Option<&'a DeliveryStatus>,
    /// Whether the receiving server vouched for the sender (`Some(false)`: SPF, DKIM and DMARC
    /// all failed or none passed). `None` when the provider reports nothing.
    pub sender_authenticated: Option<bool>,
    /// Whether the mail sat in the junk folder or carries the provider's spam label.
    pub from_junk: Option<bool>,
}

/// What was done about one inbound mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InboundAction {
    /// Nothing: the reason (`no_matching_send`, `not_a_signal`, ...).
    None {
        /// Short code.
        why: &'static str,
    },
    /// The address went on the suppression list.
    Suppressed {
        /// The address.
        address: String,
        /// `hard_bounce` or `stop_reply`.
        reason: &'static str,
    },
    /// A temporary delivery failure was counted.
    SoftBounce {
        /// The address.
        address: String,
        /// How many so far.
        count: u32,
    },
    /// The person is away: follow up not before this.
    FollowUp {
        /// Unix seconds.
        until_secs: u64,
        /// The reply stated the date (`false`: the default delay applies).
        stated: bool,
    },
}

/// The result of [`MailGuard::apply_inbound`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboundOutcome {
    /// What the text looked like (a hint).
    pub class: ReplyClass,
    /// The `Message-ID` of the mail we sent that it answers (only for a confident match).
    pub sent_message_id: Option<String>,
    /// The mission that mail came from, when it was one.
    pub mission_id: Option<String>,
    /// What was done.
    pub actions: Vec<InboundAction>,
}

impl InboundOutcome {
    fn none(class: ReplyClass, why: &'static str) -> Self {
        Self {
            class,
            sent_message_id: None,
            mission_id: None,
            actions: vec![InboundAction::None { why }],
        }
    }

    /// The date to follow up at, when the mail asked for one.
    #[must_use]
    pub fn follow_up_secs(&self) -> Option<u64> {
        self.actions.iter().find_map(|a| match a {
            InboundAction::FollowUp { until_secs, .. } => Some(*until_secs),
            _ => None,
        })
    }
}

/// Why a send is over the daily budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetBlock {
    /// Outreach mails this mailbox sent in the last 24 hours.
    pub sent: u32,
    /// What it may send today (the ramp-up value while it is warming up).
    pub allowed: u32,
    /// The full budget the ramp is heading for.
    pub budget: u32,
    /// Day of the warm-up (0 = the first day); `None` once it is over.
    pub warmup_day: Option<u32>,
}

impl std::fmt::Display for BudgetBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the outreach budget of this mailbox is used up ({} of {} mails in 24 hours",
            self.sent, self.allowed
        )?;
        if self.warmup_day.is_some() {
            write!(
                f,
                "; a new mailbox warms up slowly, the full budget is {}",
                self.budget
            )?;
        }
        f.write_str(")")
    }
}

/// What a mailbox may send in the day `day` of its warm-up (0 = first day).
fn ramp(budget: u32, warmup_days: u32, day: u32) -> u32 {
    if budget == 0 || warmup_days == 0 || day >= warmup_days {
        return budget;
    }
    let start = WARMUP_START.min(budget);
    let extra = u64::from(budget - start) * u64::from(day) / u64::from(warmup_days);
    start + u32::try_from(extra).unwrap_or(budget)
}

fn same_id(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim().trim_matches(['<', '>']).to_owned();
    let (a, b) = (trim(a), trim(b));
    !a.is_empty() && a.eq_ignore_ascii_case(&b)
}

/// The log entries (newest first) of `account` that `refs` or `thread` point to.
fn candidates<'a>(
    log: &'a [SendLogEntry],
    account: &str,
    refs: &[String],
    thread: Option<&str>,
) -> Vec<&'a SendLogEntry> {
    let thread = thread.map(str::trim).filter(|t| !t.is_empty());
    log.iter()
        .rev()
        .filter(|e| e.account == account)
        .filter(|e| {
            refs.iter()
                .take(MAX_REFS)
                .any(|r| same_id(r, &e.message_id))
                || thread.is_some_and(|t| e.thread_id.as_deref() == Some(t))
        })
        .collect()
}

/// `address` is one of the addresses the logged mail went to.
fn went_to(entry: &SendLogEntry, address: &str) -> bool {
    let k = key_of(address);
    entry.recipients.iter().any(|r| {
        let rk = key_of(r);
        rk.full == k.full || rk.base == k.base
    })
}

fn local_part(address: &str) -> &str {
    address.rsplit_once('@').map_or(address, |(l, _)| l)
}

/// The lower-case domain of a bare or `Name <addr>` address; empty when there is none.
fn domain_of(address: &str) -> String {
    address
        .rsplit_once('@')
        .map_or("", |(_, d)| d)
        .trim_matches(|c: char| c == '>' || c == '.' || c.is_whitespace())
        .to_lowercase()
}

/// The two domains are the same or one is under the other. A bare top-level name never counts.
fn domains_aligned(a: &str, b: &str) -> bool {
    a.contains('.')
        && b.contains('.')
        && (a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}")))
}

/// An explicit failure to vouch for the sender, or a spam placement, makes a mail unfit to
/// change anyone's state: anybody can forge a sender or drop mail in the junk folder.
fn sender_untrusted(m: &InboundMail<'_>) -> bool {
    m.from_junk == Some(true) || m.sender_authenticated == Some(false)
}

impl MailGuard {
    /// Match a reply or a delivery report to what we sent and act on a confident match:
    ///
    /// * a hard bounce suppresses the failed address (reason `hard_bounce`, with the message id);
    /// * a temporary failure is counted per address;
    /// * an explicit STOP suppresses the address that wrote it (reason `stop_reply`);
    /// * an out-of-office reply names the date to follow up at (its stated return date, else in
    ///   three days; the caller reschedules).
    ///
    /// No match, or a sender that is not one of the addresses the matched mail went to, changes
    /// nothing. Idempotent: the same mail twice does the same once.
    pub fn apply_inbound(&self, m: &InboundMail<'_>) -> InboundOutcome {
        let sender = m.from_addr.trim().to_lowercase();
        if let Some(dsn) = m.dsn {
            if sender_untrusted(m) {
                return InboundOutcome::none(ReplyClass::Other, "sender_not_trusted");
            }
            return self.apply_dsn(m, dsn, &sender);
        }
        let class = classify_reply(m.subject, m.body, m.auto_submitted);
        if !matches!(class, ReplyClass::Stop | ReplyClass::OutOfOffice) {
            return InboundOutcome::none(class, "not_a_signal");
        }
        // A forged or spam-folder "STOP" or "out of office" changes nothing.
        if sender_untrusted(m) {
            return InboundOutcome::none(class, "sender_not_trusted");
        }
        if sender.is_empty() {
            return InboundOutcome::none(class, "no_sender");
        }
        let entry = {
            let st = lock(&self.state);
            candidates(&st.log, m.account, m.references, m.thread_id)
                .into_iter()
                .find(|e| went_to(e, &sender))
                .cloned()
        };
        let Some(entry) = entry else {
            return InboundOutcome::none(class, "no_matching_send");
        };
        let action = if class == ReplyClass::Stop {
            match self.add_suppression(
                &sender,
                "asked to stop (reply)",
                "stop_reply",
                &entry.message_id,
            ) {
                Ok(_) => InboundAction::Suppressed {
                    address: sender,
                    reason: "stop_reply",
                },
                Err(_) => InboundAction::None { why: "not_saved" },
            }
        } else {
            let now = now_secs();
            let stated = stated_return_date(m.body, now);
            InboundAction::FollowUp {
                until_secs: stated.unwrap_or(now + DEFAULT_FOLLOW_UP_SECS),
                stated: stated.is_some(),
            }
        };
        InboundOutcome {
            class,
            sent_message_id: Some(entry.message_id),
            mission_id: entry.mission_id,
            actions: vec![action],
        }
    }

    fn apply_dsn(&self, m: &InboundMail<'_>, dsn: &DeliveryStatus, sender: &str) -> InboundOutcome {
        // The id the report itself quotes comes first: a long `References` chain must never
        // push it out of the bounded list the matcher reads.
        let mut refs: Vec<String> = dsn.original_message_id.iter().cloned().collect();
        refs.extend(m.references.iter().take(MAX_REFS - 1).cloned());
        let from_mta = matches!(local_part(sender), "mailer-daemon" | "postmaster");
        let mut out = InboundOutcome {
            class: ReplyClass::Other,
            sent_message_id: None,
            mission_id: None,
            actions: Vec::new(),
        };
        for r in dsn.recipients.iter().take(MAX_DSN_RECIPIENTS) {
            let (hard, soft) = (r.is_permanent_failure(), r.is_temporary());
            let Some(address) = r.address().filter(|_| hard || soft) else {
                continue;
            };
            if hard {
                out.class = ReplyClass::HardBounce;
            } else if out.class != ReplyClass::HardBounce {
                out.class = ReplyClass::SoftBounce;
            }
            // Only a mail that looks like it came from a mail system counts, structured or
            // not: any sender can attach a `message/delivery-status` part.
            if !from_mta {
                out.actions.push(InboundAction::None {
                    why: "report_not_from_a_mail_system",
                });
                continue;
            }
            let entry = {
                let st = lock(&self.state);
                candidates(&st.log, m.account, &refs, m.thread_id)
                    .into_iter()
                    // The failed address must be one the mail went to. A recipient who
                    // wrote this "report" can only speak for their own address.
                    .find(|e| {
                        went_to(e, address)
                            && (!went_to(e, sender) || key_of(sender).base == key_of(address).base)
                    })
                    .cloned()
            };
            let Some(entry) = entry else {
                out.actions.push(InboundAction::None {
                    why: "no_matching_send",
                });
                continue;
            };
            // The mail system must belong to the mail it reports on: our own sending domain
            // or the domain of the failed address. `mailer-daemon@anywhere` is not enough.
            let sender_domain = domain_of(sender);
            if !domains_aligned(&sender_domain, &domain_of(&entry.from))
                && !domains_aligned(&sender_domain, &domain_of(address))
            {
                out.actions.push(InboundAction::None {
                    why: "report_sender_not_aligned",
                });
                continue;
            }
            // A report about one of several recipients speaks for somebody else's address, so
            // the receiving server must also have vouched for the mail system.
            if entry.recipients.len() > 1 && m.sender_authenticated != Some(true) {
                out.actions.push(InboundAction::None {
                    why: "report_not_authenticated",
                });
                continue;
            }
            out.sent_message_id = Some(entry.message_id.clone());
            out.mission_id.clone_from(&entry.mission_id);
            let action = if hard {
                match self.add_suppression(
                    address,
                    "bounced: permanent delivery failure",
                    "hard_bounce",
                    &entry.message_id,
                ) {
                    Ok(_) => InboundAction::Suppressed {
                        address: address.to_owned(),
                        reason: "hard_bounce",
                    },
                    Err(_) => InboundAction::None { why: "not_saved" },
                }
            } else {
                match self.note_soft_bounce(address, m.id, &entry.message_id) {
                    Ok(count) => InboundAction::SoftBounce {
                        address: address.to_owned(),
                        count,
                    },
                    Err(_) => InboundAction::None { why: "not_saved" },
                }
            };
            out.actions.push(action);
        }
        if out.actions.is_empty() {
            return InboundOutcome::none(out.class, "not_a_signal");
        }
        out
    }

    /// Count one temporary delivery failure of `address` (saved at once). `inbound_id` is the
    /// report's own id: the same report counts once. Returns the count so far.
    fn note_soft_bounce(
        &self,
        address: &str,
        inbound_id: &str,
        sent_message_id: &str,
    ) -> Result<u32, String> {
        let key = key_of(address).base;
        let mut st = lock(&self.state);
        let mut next = st.outreach.clone();
        if !next.soft_bounces.contains_key(&key)
            && next.soft_bounces.len() >= MAX_SOFT_BOUNCE_ADDRESSES
        {
            return Err("the soft-bounce list is full".into());
        }
        let e = next.soft_bounces.entry(key).or_default();
        if !inbound_id.is_empty() && e.seen.iter().any(|s| s == inbound_id) {
            return Ok(e.count);
        }
        e.count = e.count.saturating_add(1);
        e.last_at = now_secs();
        e.last_message_id = sent_message_id.chars().take(300).collect();
        if !inbound_id.is_empty() {
            e.seen.push(inbound_id.chars().take(300).collect());
            if e.seen.len() > SEEN_KEEP {
                e.seen.remove(0);
            }
        }
        let count = e.count;
        let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        self.write_json("outreach-state.json", &text)?;
        st.outreach = next;
        Ok(count)
    }

    /// Temporary delivery failures counted for `address` so far.
    #[must_use]
    pub fn soft_bounces(&self, address: &str) -> u32 {
        lock(&self.state)
            .outreach
            .soft_bounces
            .get(&key_of(address).base)
            .map_or(0, |b| b.count)
    }

    /// The mail in the send log that carries `message_id` (or thread `thread_id`) in `account`.
    #[must_use]
    pub fn find_sent(
        &self,
        account: &str,
        message_id: &str,
        thread_id: Option<&str>,
    ) -> Option<SendLogEntry> {
        let st = lock(&self.state);
        candidates(&st.log, account, &[message_id.to_owned()], thread_id)
            .first()
            .map(|e| (*e).clone())
    }

    /// Whether `account` may send `planned` more mails as outreach right now: today's count
    /// (every mail the mailbox sent in the last 24 hours, outreach or not) against its budget,
    /// ramped up over the warm-up. Read this and [`Self::record`] the send under one hold of the
    /// send lock and two bots cannot pass the same remaining budget; the count comes from the
    /// saved send log, so a restart does not reset it.
    ///
    /// # Errors
    /// [`BudgetBlock`] when the send would go over.
    pub fn check_outreach_budget(&self, account: &str, planned: usize) -> Result<(), BudgetBlock> {
        let st = lock(&self.state);
        let budget = st.settings.outreach_daily_budget;
        if budget == 0 {
            return Ok(());
        }
        let now = now_secs();
        let day_ago = now.saturating_sub(86_400);
        let sent: usize = st
            .log
            .iter()
            .filter(|e| e.at >= day_ago && e.account == account)
            .map(|e| e.recipients.len().max(1))
            .sum();
        let start = st
            .outreach
            .warmup_start
            .get(account)
            .copied()
            .or_else(|| {
                st.log
                    .iter()
                    .filter(|e| e.account == account)
                    .map(|e| e.at)
                    .min()
            })
            .unwrap_or(now);
        let day = u32::try_from(now.saturating_sub(start) / 86_400).unwrap_or(u32::MAX);
        let warmup_days = st.settings.outreach_warmup_days;
        let allowed = ramp(budget, warmup_days, day);
        if sent + planned > allowed as usize {
            return Err(BudgetBlock {
                sent: u32::try_from(sent).unwrap_or(u32::MAX),
                allowed,
                budget,
                warmup_day: (warmup_days > 0 && day < warmup_days).then_some(day),
            });
        }
        Ok(())
    }

    /// Remember the start of `account`'s warm-up (its first mail) in the saved state, once.
    /// Called with the entry about to be logged.
    pub(super) fn note_first_send(&self, st: &mut super::State, account: &str, at: u64) {
        if st.outreach.warmup_start.contains_key(account) {
            return;
        }
        // An established mailbox (older entries in the log) does not start over.
        let start = st
            .log
            .iter()
            .filter(|e| e.account == account)
            .map(|e| e.at)
            .min()
            .map_or(at, |first| first.min(at));
        let mut next = st.outreach.clone();
        next.warmup_start.insert(account.to_owned(), start);
        if let Ok(text) = serde_json::to_string_pretty(&next)
            && self.write_json("outreach-state.json", &text).is_ok()
        {
            st.outreach = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::GuardSettings;

    fn start_for(guard: &MailGuard, account: &str) -> Option<u64> {
        lock(&guard.state)
            .outreach
            .warmup_start
            .get(account)
            .copied()
    }

    fn sent(guard: &MailGuard, id: &str, to: &str, mission: Option<&str>) {
        guard.record(SendLogEntry {
            at: now_secs(),
            at_utc: String::new(),
            bot: Some("bot".into()),
            run: "run".into(),
            account: "work".into(),
            from: "me@example.com".into(),
            recipients: vec![to.into()],
            subject: "Hello".into(),
            message_id: id.into(),
            thread_id: None,
            mission_id: mission.map(str::to_owned),
            opt_out_line: true,
            overridden: Vec::new(),
        });
    }

    fn mail<'a>(from: &'a str, body: &'a str, refs: &'a [String]) -> InboundMail<'a> {
        InboundMail {
            account: "work",
            id: "in-1",
            from_addr: from,
            subject: "Re: Hello",
            body,
            auto_submitted: None,
            references: refs,
            thread_id: None,
            dsn: None,
            sender_authenticated: None,
            from_junk: None,
        }
    }

    fn report(rcpt: &str, action: &str, status: &str, id: &str) -> DeliveryStatus {
        DeliveryStatus::parse(
            &format!(
                "Final-Recipient: rfc822; {rcpt}\nAction: {action}\nStatus: {status}\n\nMessage-ID: <{id}>\n"
            ),
            true,
        )
        .unwrap()
    }

    #[test]
    fn the_ramp_climbs_from_the_start_value_to_the_budget() {
        assert_eq!(ramp(20, 14, 0), 5);
        assert_eq!(ramp(20, 14, 7), 12);
        assert_eq!(ramp(20, 14, 14), 20);
        assert_eq!(ramp(20, 0, 0), 20);
        assert_eq!(ramp(3, 14, 0), 3);
        assert_eq!(ramp(0, 14, 0), 0);
    }

    #[test]
    fn a_stop_reply_suppresses_only_the_sender_of_a_matched_mail() {
        let g = MailGuard::in_memory();
        sent(&g, "id1@example.com", "ann@example.org", Some("m1"));
        let refs = vec!["<id1@example.com>".to_owned()];
        let o = g.apply_inbound(&mail("ann@example.org", "STOP\n\n> hi", &refs));
        assert_eq!(o.class, ReplyClass::Stop);
        assert_eq!(o.sent_message_id.as_deref(), Some("id1@example.com"));
        assert_eq!(o.mission_id.as_deref(), Some("m1"));
        assert!(matches!(
            &o.actions[..],
            [InboundAction::Suppressed {
                reason: "stop_reply",
                ..
            }]
        ));
        let list = g.opt_outs();
        assert_eq!(list[0].value, "ann@example.org");
        assert_eq!(list[0].reason, "stop_reply");
        assert_eq!(list[0].message_id, "id1@example.com");
        // Twice is the same as once.
        g.apply_inbound(&mail("ann@example.org", "STOP", &refs));
        assert_eq!(g.opt_outs().len(), 1);
    }

    #[test]
    fn a_stop_without_a_confident_match_changes_nothing() {
        let g = MailGuard::in_memory();
        sent(&g, "id1@example.com", "ann@example.org", None);
        let refs = vec!["id1@example.com".to_owned()];
        // Someone else knows the id: not one of the addresses the mail went to.
        let o = g.apply_inbound(&mail("eve@example.net", "STOP", &refs));
        assert_eq!(
            o.actions,
            vec![InboundAction::None {
                why: "no_matching_send"
            }]
        );
        // The right person, but no reference to anything we sent.
        let o = g.apply_inbound(&mail("ann@example.org", "STOP", &["other@x".to_owned()]));
        assert_eq!(
            o.actions,
            vec![InboundAction::None {
                why: "no_matching_send"
            }]
        );
        // The right id in another mailbox.
        let mut m = mail("ann@example.org", "STOP", &refs);
        m.account = "other";
        assert_eq!(
            g.apply_inbound(&m).actions,
            vec![InboundAction::None {
                why: "no_matching_send"
            }]
        );
        // Not asking to stop.
        let o = g.apply_inbound(&mail("ann@example.org", "Please stop by tomorrow", &refs));
        assert_eq!(
            o.actions,
            vec![InboundAction::None {
                why: "not_a_signal"
            }]
        );
        assert_eq!(g.opt_outs(), Vec::new());
    }

    #[test]
    fn a_hard_bounce_suppresses_the_failed_address_with_the_message_id() {
        let g = MailGuard::in_memory();
        sent(&g, "id2@example.com", "Bob@Example.org", None);
        let dsn = report("bob@example.org", "failed", "5.1.1", "id2@example.com");
        let mut m = mail("mailer-daemon@mx.example.org", "", &[]);
        m.dsn = Some(&dsn);
        let o = g.apply_inbound(&m);
        assert_eq!(o.class, ReplyClass::HardBounce);
        assert!(matches!(
            &o.actions[..],
            [InboundAction::Suppressed {
                reason: "hard_bounce",
                ..
            }]
        ));
        assert_eq!(g.opt_outs()[0].reason, "hard_bounce");
        assert_eq!(g.opt_outs()[0].message_id, "id2@example.com");
    }

    #[test]
    fn a_bounce_for_an_address_the_mail_did_not_go_to_is_ignored() {
        let g = MailGuard::in_memory();
        sent(&g, "id2@example.com", "bob@example.org", None);
        let dsn = report("carol@example.org", "failed", "5.1.1", "id2@example.com");
        let mut m = mail("mailer-daemon@mx.example.org", "", &[]);
        m.dsn = Some(&dsn);
        assert_eq!(
            g.apply_inbound(&m).actions,
            vec![InboundAction::None {
                why: "no_matching_send"
            }]
        );
        // Unknown original message id.
        let dsn = report("bob@example.org", "failed", "5.1.1", "nope@example.com");
        m.dsn = Some(&dsn);
        assert_eq!(
            g.apply_inbound(&m).actions,
            vec![InboundAction::None {
                why: "no_matching_send"
            }]
        );
        assert_eq!(g.opt_outs(), Vec::new());
    }

    #[test]
    fn a_recipient_cannot_report_a_bounce_for_a_co_recipient() {
        let g = MailGuard::in_memory();
        // A co-recipient whose own address looks like a mail system's.
        g.record(SendLogEntry {
            recipients: vec!["postmaster@example.org".into(), "carol@example.org".into()],
            ..entry_for("id3@example.com")
        });
        let dsn = report("carol@example.org", "failed", "5.1.1", "id3@example.com");
        let mut m = mail("postmaster@example.org", "", &[]);
        m.dsn = Some(&dsn);
        m.sender_authenticated = Some(true);
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::None {
                why: "no_matching_send"
            }]
        ));
        // About their own address it can speak.
        let dsn = report(
            "postmaster@example.org",
            "failed",
            "5.1.1",
            "id3@example.com",
        );
        m.dsn = Some(&dsn);
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
    }

    #[test]
    fn a_forged_report_never_suppresses_anyone() {
        let g = MailGuard::in_memory();
        g.record(SendLogEntry {
            recipients: vec!["bob@example.org".into(), "carol@example.net".into()],
            ..entry_for("id7@example.com")
        });
        let dsn = report("carol@example.net", "failed", "5.1.1", "id7@example.com");
        let why = |g: &MailGuard, m: &InboundMail<'_>| match &g.apply_inbound(m).actions[..] {
            [InboundAction::None { why }] => *why,
            other => panic!("{other:?}"),
        };
        // A co-recipient writes a structured report from its own, non-mail-system address.
        let mut m = mail("bob@example.org", "", &[]);
        m.dsn = Some(&dsn);
        m.sender_authenticated = Some(true);
        assert_eq!(why(&g, &m), "report_not_from_a_mail_system");
        // A mail-system look-alike on an unrelated domain.
        m.from_addr = "mailer-daemon@attacker.test";
        assert_eq!(why(&g, &m), "report_sender_not_aligned");
        // The real mail system of the failed address, but nobody vouched for it and the mail
        // went to several people.
        m.from_addr = "mailer-daemon@mx.example.net";
        m.sender_authenticated = None;
        assert_eq!(why(&g, &m), "report_not_authenticated");
        // Failed authentication or the junk folder: nothing is read at all.
        m.sender_authenticated = Some(false);
        assert_eq!(why(&g, &m), "sender_not_trusted");
        m.sender_authenticated = Some(true);
        m.from_junk = Some(true);
        assert_eq!(why(&g, &m), "sender_not_trusted");
        assert_eq!(g.opt_outs(), Vec::new());
        // The genuine, authenticated one goes through.
        m.from_junk = Some(false);
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
    }

    #[test]
    fn a_single_recipient_report_from_an_unreporting_provider_still_counts() {
        let g = MailGuard::in_memory();
        sent(&g, "id8@example.com", "bob@example.org", None);
        let dsn = report("bob@example.org", "failed", "5.1.1", "id8@example.com");
        // Aligned with our own sending domain too (the mail system of our provider).
        let mut m = mail("mailer-daemon@mail.example.com", "", &[]);
        m.dsn = Some(&dsn);
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
    }

    #[test]
    fn a_stop_or_an_out_of_office_from_a_failed_or_junk_sender_changes_nothing() {
        let g = MailGuard::in_memory();
        sent(&g, "id9@example.com", "ann@example.org", Some("m1"));
        let refs = vec!["id9@example.com".to_owned()];
        for (auth, junk) in [(Some(false), None), (None, Some(true))] {
            let mut m = mail("ann@example.org", "STOP", &refs);
            m.sender_authenticated = auth;
            m.from_junk = junk;
            assert_eq!(
                g.apply_inbound(&m).actions,
                vec![InboundAction::None {
                    why: "sender_not_trusted"
                }]
            );
            let mut m = mail("ann@example.org", "I am away.", &refs);
            m.subject = "Automatic reply: Hello";
            m.auto_submitted = Some("auto-replied");
            m.sender_authenticated = auth;
            m.from_junk = junk;
            assert_eq!(g.apply_inbound(&m).follow_up_secs(), None);
        }
        assert_eq!(g.opt_outs(), Vec::new());
        // Vouched for, or nothing known: it counts.
        let mut m = mail("ann@example.org", "STOP", &refs);
        m.sender_authenticated = Some(true);
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
    }

    #[test]
    fn the_id_a_report_quotes_is_found_behind_a_long_references_chain() {
        let g = MailGuard::in_memory();
        sent(&g, "id10@example.com", "bob@example.org", None);
        let refs: Vec<String> = (0..200).map(|i| format!("noise{i}@x")).collect();
        let dsn = report("bob@example.org", "failed", "5.1.1", "id10@example.com");
        let mut m = mail("mailer-daemon@mx.example.org", "", &refs);
        m.dsn = Some(&dsn);
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
    }

    fn entry_for(id: &str) -> SendLogEntry {
        SendLogEntry {
            at: now_secs(),
            at_utc: String::new(),
            bot: None,
            run: "run".into(),
            account: "work".into(),
            from: "me@example.com".into(),
            recipients: vec![],
            subject: "s".into(),
            message_id: id.into(),
            thread_id: None,
            mission_id: None,
            opt_out_line: true,
            overridden: Vec::new(),
        }
    }

    #[test]
    fn a_report_written_as_plain_text_only_counts_from_a_mail_system() {
        let g = MailGuard::in_memory();
        sent(&g, "id4@example.com", "bob@example.org", None);
        let text = DeliveryStatus::parse(
            "Final-Recipient: rfc822; bob@example.org\nAction: failed\nStatus: 5.1.1\n\nMessage-ID: <id4@example.com>",
            false,
        )
        .unwrap();
        let mut m = mail("bob@example.org", "", &[]);
        m.dsn = Some(&text);
        assert_eq!(
            g.apply_inbound(&m).actions,
            vec![InboundAction::None {
                why: "report_not_from_a_mail_system"
            }]
        );
        m.from_addr = "postmaster@mx.example.org";
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
    }

    #[test]
    fn a_soft_bounce_is_counted_once_per_report_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let g = MailGuard::open(dir.path().to_path_buf());
        sent(&g, "id5@example.com", "bob@example.org", None);
        let dsn = report("bob@example.org", "delayed", "4.2.0", "id5@example.com");
        let mut m = mail("mailer-daemon@mx.example.org", "", &[]);
        m.dsn = Some(&dsn);
        let o = g.apply_inbound(&m);
        assert_eq!(o.class, ReplyClass::SoftBounce);
        assert_eq!(
            o.actions,
            vec![InboundAction::SoftBounce {
                address: "bob@example.org".into(),
                count: 1
            }]
        );
        g.apply_inbound(&m); // the same report again
        m.id = "in-2";
        g.apply_inbound(&m);
        assert_eq!(g.soft_bounces("bob@example.org"), 2);
        assert_eq!(g.opt_outs(), Vec::new(), "a soft bounce never suppresses");
        let again = MailGuard::open(dir.path().to_path_buf());
        assert_eq!(again.soft_bounces("Bob@example.org"), 2);
    }

    #[test]
    #[allow(clippy::many_single_char_names)] // a date's y/m/d next to the usual test names
    fn an_out_of_office_reply_names_the_stated_date_or_three_days() {
        let g = MailGuard::in_memory();
        sent(&g, "id6@example.com", "ann@example.org", Some("m1"));
        let refs = vec!["id6@example.com".to_owned()];
        let mut m = mail(
            "ann@example.org",
            "I am away. I will return on 2999-01-01.",
            &refs,
        );
        m.subject = "Automatic reply: Hello";
        m.auto_submitted = Some("auto-replied");
        // A date far away is not believed.
        let now = now_secs();
        let o = g.apply_inbound(&m);
        assert_eq!(o.class, ReplyClass::OutOfOffice);
        let until = o.follow_up_secs().unwrap();
        assert!(until >= now + DEFAULT_FOLLOW_UP_SECS && until <= now + DEFAULT_FOLLOW_UP_SECS + 5);
        assert!(matches!(
            &o.actions[..],
            [InboundAction::FollowUp { stated: false, .. }]
        ));
        assert_eq!(o.mission_id.as_deref(), Some("m1"));
        // A believable date: ten days from now, written the way a person would.
        let day = (now / 86_400 + 10) * 86_400;
        let (y, mo, d) = civil(day / 86_400);
        let body = format!("Out of office. Back on {y:04}-{mo:02}-{d:02}.");
        m.body = &body;
        let o = g.apply_inbound(&m);
        assert_eq!(o.follow_up_secs(), Some(day + 9 * 3600));
        assert!(matches!(
            &o.actions[..],
            [InboundAction::FollowUp { stated: true, .. }]
        ));
        assert_eq!(g.opt_outs(), Vec::new());
        // From a stranger: nothing.
        m.from_addr = "eve@example.net";
        assert_eq!(g.apply_inbound(&m).follow_up_secs(), None);
    }

    /// Civil date of a day count since 1970-01-01 (the inverse of the library's helper).
    fn civil(z: u64) -> (i64, u32, u32) {
        let z = i64::try_from(z).unwrap() + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap();
        let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap();
        let y = yoe + era * 400 + i64::from(m <= 2);
        (y, m, d)
    }

    #[test]
    fn a_reply_is_matched_by_thread_id_when_the_provider_has_no_message_id() {
        let g = MailGuard::in_memory();
        g.record(SendLogEntry {
            recipients: vec!["ann@example.org".into()],
            thread_id: Some("thr-9".into()),
            ..entry_for("api-msg-1")
        });
        let mut m = mail("ann@example.org", "unsubscribe", &[]);
        m.thread_id = Some("thr-9");
        assert!(matches!(
            &g.apply_inbound(&m).actions[..],
            [InboundAction::Suppressed { .. }]
        ));
        assert_eq!(
            g.find_sent("work", "api-msg-1", None)
                .unwrap()
                .thread_id
                .as_deref(),
            Some("thr-9")
        );
        assert!(g.find_sent("work", "nope", Some("thr-1")).is_none());
    }

    fn budget_guard(budget: u32, warmup: u32) -> MailGuard {
        let g = MailGuard::in_memory();
        g.set_settings(GuardSettings {
            outreach_daily_budget: budget,
            outreach_warmup_days: warmup,
            ..GuardSettings::default()
        })
        .unwrap();
        g
    }

    #[test]
    fn a_new_mailbox_is_held_to_the_warm_up_and_an_old_one_to_the_budget() {
        let g = budget_guard(20, 14);
        // First day: five.
        for i in 0..5 {
            assert!(g.check_outreach_budget("work", 1).is_ok(), "{i}");
            sent(&g, &format!("a{i}@x"), "ann@example.org", None);
        }
        let block = g.check_outreach_budget("work", 1).unwrap_err();
        assert_eq!((block.sent, block.allowed, block.budget), (5, 5, 20));
        assert_eq!(block.warmup_day, Some(0));
        assert!(block.to_string().contains("5 of 5"), "{block}");
        // A mailbox with history older than the warm-up is at the full budget at once.
        let old = MailGuard::in_memory();
        old.set_settings(GuardSettings::default()).unwrap();
        old.record(SendLogEntry {
            at: now_secs() - 30 * 86_400,
            ..entry_for("old@x")
        });
        for i in 0..20 {
            assert!(old.check_outreach_budget("work", 1).is_ok(), "{i}");
            sent(&old, &format!("b{i}@x"), "ann@example.org", None);
        }
        assert!(old.check_outreach_budget("work", 1).is_err());
        // Another (new) mailbox has its own budget and warm-up; a batch counts every recipient; 0 turns it off.
        assert!(old.check_outreach_budget("other", 5).is_ok());
        assert!(old.check_outreach_budget("other", 6).is_err());
        let off = budget_guard(0, 14);
        assert!(off.check_outreach_budget("work", 10_000).is_ok());
    }

    #[test]
    fn the_budget_and_the_warm_up_start_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let g = MailGuard::open(dir.path().to_path_buf());
        g.set_settings(GuardSettings {
            outreach_daily_budget: 2,
            outreach_warmup_days: 0,
            ..GuardSettings::default()
        })
        .unwrap();
        sent(&g, "a@x", "ann@example.org", None);
        sent(&g, "b@x", "bob@example.org", None);
        drop(g);
        let again = MailGuard::open(dir.path().to_path_buf());
        assert!(again.check_outreach_budget("work", 1).is_err());
        assert!(dir.path().join("outreach-state.json").exists());
        assert!(start_for(&again, "work").is_some());
    }

    #[tokio::test]
    async fn parallel_senders_cannot_pass_the_same_remaining_budget() {
        let g = std::sync::Arc::new(budget_guard(3, 0));
        let ok = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut tasks = Vec::new();
        for i in 0..10 {
            let (g, ok) = (std::sync::Arc::clone(&g), std::sync::Arc::clone(&ok));
            tasks.push(tokio::spawn(async move {
                // The same discipline `email_send` follows: check and record under the lock.
                let _hold = g.send_lock.lock().await;
                if g.check_outreach_budget("work", 1).is_ok() {
                    tokio::task::yield_now().await;
                    sent(&g, &format!("p{i}@x"), "ann@example.org", None);
                    ok.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(ok.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}
