//! Owner notices: what a bridge pushes to the owner on its own (a finished, failed or blocked
//! mission or background run, a milestone), and how the owner answers the questions in one.
//!
//! The daemon builds a [`OwnerNotice`] from the ledger and the run record (never from model
//! prose it has not been given by the bot itself), serves it over `GET /v1/owner-notices/stream`
//! and the bridges render it here, in the chat's language, with the same wording on every
//! channel. Approvals never travel through a notice: [`OwnerNotice::app_only`] only says that
//! something waits in the app (a purchase, a one-time code), with no button and no way to decide it
//! from the chat.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Most blocked items a notice message lists (and numbers) in full.
pub const MAX_LISTED: usize = 12;
/// Longest text of one listed line, in characters.
const LINE_CHARS: usize = 220;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A mission or background run is done.
    Finished,
    /// It ended in an error.
    Failed,
    /// It cannot go on without the owner.
    Blocked,
    /// A milestone of a mission that is still running.
    Progress,
}

/// Where the work ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NoticeSource {
    /// A mission (ledger of items).
    Mission,
    /// A scheduled or other background run.
    Run,
}

/// One line of a notice: an item or question, in the bot's own words.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NoticeItem {
    /// Item key (a mission item) or tool call id (a question of a run): what an answer names.
    pub key: String,
    /// Short title.
    #[serde(default)]
    pub title: String,
    /// The question, the result or the reason.
    #[serde(default)]
    pub text: Option<String>,
}

/// Headline numbers of a mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NoticeProgress {
    /// All items.
    pub total: u32,
    /// Finished.
    pub done: u32,
    /// Waiting for the owner.
    pub blocked: u32,
    /// Given up.
    pub failed: u32,
    /// Skipped.
    pub skipped: u32,
}

/// An owner notice as the daemon serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OwnerNotice {
    /// Position in the daemon's push log (strictly increasing); a bridge that reconnects asks
    /// for everything `after` the last one it saw.
    #[serde(default)]
    pub seq: u64,
    /// Stable id (what a retry or an answer names).
    pub id: String,
    /// When it happened, Unix ms.
    pub at_ms: i64,
    /// What happened.
    pub kind: NoticeKind,
    /// Where the work ran.
    pub source: NoticeSource,
    /// Mission title, or what the scheduled run was.
    pub title: String,
    /// The bot's name.
    #[serde(default)]
    pub bot_name: String,
    /// The bot's id.
    #[serde(default)]
    pub bot_id: Option<String>,
    /// The mission (for a mission notice).
    #[serde(default)]
    pub mission_id: Option<String>,
    /// The run (for a run notice).
    #[serde(default)]
    pub run_id: Option<String>,
    /// The chat the work ran in.
    #[serde(default)]
    pub chat_id: Option<String>,
    /// Why it ended or blocked.
    #[serde(default)]
    pub reason: Option<String>,
    /// Mission numbers.
    #[serde(default)]
    pub progress: Option<NoticeProgress>,
    /// What the run said at its end (a background run), or a few finished items.
    #[serde(default)]
    pub done: Vec<NoticeItem>,
    /// Items given up, with the last error.
    #[serde(default)]
    pub failed: Vec<NoticeItem>,
    /// Questions the owner can answer from the chat.
    #[serde(default)]
    pub questions: Vec<NoticeItem>,
    /// Things that wait for the owner in the app only (an approval, a one-time code).
    #[serde(default)]
    pub app_only: Vec<String>,
    /// The daemon can retry or resume this (`POST /v1/owner-notices/{id}/retry`).
    #[serde(default)]
    pub can_retry: bool,
    /// Held back by quiet hours and delivered now.
    #[serde(default)]
    pub late: bool,
}

/// Language of a rendered notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLang {
    /// English (the default and the fallback).
    En,
    /// Turkish.
    Tr,
}

fn clip(text: &str, max: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut out: String = one_line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn pick<'a>(lang: NoticeLang, en: &'a str, tr: &'a str) -> &'a str {
    match lang {
        NoticeLang::En => en,
        NoticeLang::Tr => tr,
    }
}

/// The notice as a short plain message for a chat. Free text (titles, reasons, answers the bot
/// wrote) is clipped and passed through; everything else is the channel's own wording.
#[must_use]
pub fn render(n: &OwnerNotice, lang: NoticeLang) -> String {
    let head = match n.kind {
        NoticeKind::Finished => pick(lang, "✅ Finished", "✅ Tamamlandı"),
        NoticeKind::Failed => pick(lang, "⚠️ Failed", "⚠️ Başarısız oldu"),
        NoticeKind::Blocked => pick(lang, "✋ Needs you", "✋ Senden bir şey gerekiyor"),
        NoticeKind::Progress => pick(lang, "⏳ Progress", "⏳ İlerleme"),
    };
    let mut s = format!("{head}: {}", clip(&n.title, 90));
    if !n.bot_name.is_empty() {
        let _ = write!(s, " ({})", clip(&n.bot_name, 40));
    }
    s.push('\n');
    if n.late {
        s.push_str(pick(
            lang,
            "(held back during your quiet hours)\n",
            "(sessiz saatlerin için bekletildi)\n",
        ));
    }
    if let Some(p) = n.progress {
        let _ = match lang {
            NoticeLang::En => writeln!(
                s,
                "{} of {} done, {} waiting for you, {} given up.",
                p.done, p.total, p.blocked, p.failed
            ),
            NoticeLang::Tr => writeln!(
                s,
                "{} / {} tamam, {} seni bekliyor, {} vazgeçildi.",
                p.done, p.total, p.blocked, p.failed
            ),
        };
    }
    if let Some(r) = n.reason.as_deref().filter(|r| !r.trim().is_empty()) {
        let _ = writeln!(s, "{} {}", pick(lang, "Why:", "Neden:"), clip(r, 300));
    }
    for d in n.done.iter().take(3) {
        let text = d.text.as_deref().unwrap_or(&d.title);
        let _ = writeln!(s, "• {}", clip(text, LINE_CHARS));
    }
    if !n.failed.is_empty() {
        s.push_str(pick(lang, "\nGiven up:\n", "\nVazgeçilenler:\n"));
        for f in n.failed.iter().take(5) {
            let _ = writeln!(
                s,
                "- {}: {}",
                clip(&f.key, 60),
                clip(f.text.as_deref().unwrap_or("?"), LINE_CHARS)
            );
        }
    }
    if !n.questions.is_empty() {
        s.push_str(pick(lang, "\nQuestions for you:\n", "\nSana sorular:\n"));
        for (i, q) in n.questions.iter().take(MAX_LISTED).enumerate() {
            let text = q.text.as_deref().unwrap_or(&q.title);
            let _ = writeln!(
                s,
                "{}. {} — {}",
                i + 1,
                clip(&q.title, 60),
                clip(text, LINE_CHARS)
            );
        }
        if n.questions.len() > MAX_LISTED {
            let _ = writeln!(
                s,
                "{}",
                pick(lang, "…more in the app.", "…devamı uygulamada.")
            );
        }
        s.push_str(pick(
            lang,
            "\nAnswer with /answer <text>. For several questions write one per line: `1: yes` and `2: skip`.\n",
            "\n/answer <metin> ile yanıtla. Birden çok soruda her satıra bir tane yaz: `1: evet` ve `2: atla`.\n",
        ));
    }
    if !n.app_only.is_empty() {
        s.push_str(pick(
            lang,
            "\nWaiting in the app (not from here):\n",
            "\nUygulamada bekleyenler (buradan yapılamaz):\n",
        ));
        for a in n.app_only.iter().take(5) {
            let _ = writeln!(s, "- {}", clip(a, LINE_CHARS));
        }
    }
    s.trim_end().to_owned()
}

/// What the owner decided for one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The question's key.
    pub key: String,
    /// The answer, or `None` to skip it.
    pub answer: Option<String>,
}

/// Why a reply could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyError {
    /// Nothing was written.
    Empty,
    /// Several questions, but the text does not number its answers.
    NeedsNumbers,
    /// A number names no listed question.
    UnknownNumber(usize),
}

fn is_skip(word: &str) -> bool {
    matches!(
        word.trim().to_lowercase().as_str(),
        "skip" | "atla" | "geç" | "gec"
    )
}

/// Split `1: text`, `1) text`, `1. text`, `1 - text` (and `skip 2`) into `(number, text)`.
fn numbered(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("skip ").or_else(|| t.strip_prefix("atla "))
        && let Ok(n) = rest.trim().parse::<usize>()
    {
        return Some((n, "skip".to_owned()));
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let rest = t[digits.len()..].trim_start();
    let rest = rest
        .strip_prefix([':', ')', '.', '-', '='])
        .map(str::trim_start)?;
    Some((digits.parse().ok()?, rest.trim_end().to_owned()))
}

/// Read the owner's text as answers to the questions of `n` (those listed in its message).
///
/// One question: the whole text is the answer (`skip` skips it). Several: one numbered line per
/// answer, a line without a number continues the previous answer.
///
/// # Errors
/// See [`ReplyError`].
pub fn parse_reply(text: &str, n: &OwnerNotice) -> Result<Vec<Reply>, ReplyError> {
    let listed = &n.questions[..n.questions.len().min(MAX_LISTED)];
    let text = text.trim();
    if text.is_empty() || listed.is_empty() {
        return Err(ReplyError::Empty);
    }
    let first_numbered = numbered(text.lines().next().unwrap_or_default());
    if listed.len() == 1 && first_numbered.is_none() {
        return Ok(vec![Reply {
            key: listed[0].key.clone(),
            answer: (!is_skip(text)).then(|| text.to_owned()),
        }]);
    }
    let mut parts: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        if let Some((num, rest)) = numbered(line) {
            parts.push((num, rest));
        } else if let Some(last) = parts.last_mut() {
            last.1.push('\n');
            last.1.push_str(line.trim_end());
        } else {
            return Err(ReplyError::NeedsNumbers);
        }
    }
    let mut out: Vec<Reply> = Vec::new();
    for (num, body) in parts {
        let Some(q) = num.checked_sub(1).and_then(|i| listed.get(i)) else {
            return Err(ReplyError::UnknownNumber(num));
        };
        let body = body.trim().to_owned();
        if body.is_empty() {
            return Err(ReplyError::Empty);
        }
        // A later line for the same question replaces the earlier one.
        out.retain(|r| r.key != q.key);
        out.push(Reply {
            key: q.key.clone(),
            answer: (!is_skip(&body)).then_some(body),
        });
    }
    Ok(out)
}

/// What the bridge tells the owner after they answer or retry, in the chat's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    /// `/answer` with no open question.
    NothingOpen,
    /// `/answer` with no text.
    Empty,
    /// Several questions but no numbers.
    NeedsNumbers,
    /// A number past the list.
    UnknownNumber(usize),
    /// The daemon took the answers.
    Answered {
        /// Answers that went back to the bot.
        answered: u32,
        /// Questions skipped.
        skipped: u32,
    },
    /// The daemon refused or could not take them.
    AnswerFailed,
    /// "Try again" was accepted.
    Retried,
    /// "Try again" was refused or failed.
    RetryFailed,
    /// Label of the retry button.
    TryAgainLabel,
}

/// The wording of a [`Feedback`].
#[must_use]
pub fn feedback(lang: NoticeLang, f: Feedback) -> String {
    use NoticeLang::{En, Tr};
    match (f, lang) {
        (Feedback::NothingOpen, En) => "No question is waiting for an answer here.".into(),
        (Feedback::NothingOpen, Tr) => "Burada yanıt bekleyen bir soru yok.".into(),
        (Feedback::Empty, En) => "Write your answer after /answer.".into(),
        (Feedback::Empty, Tr) => "/answer komutundan sonra yanıtını yaz.".into(),
        (Feedback::NeedsNumbers, En) => {
            "There are several questions. Write one answer per line, starting each with its number: `1: yes`.".into()
        }
        (Feedback::NeedsNumbers, Tr) => {
            "Birden çok soru var. Her satıra bir yanıt yaz ve numarayla başlat: `1: evet`.".into()
        }
        (Feedback::UnknownNumber(n), En) => format!("There is no question {n}."),
        (Feedback::UnknownNumber(n), Tr) => format!("{n} numaralı bir soru yok."),
        (Feedback::Answered { answered, skipped }, En) => {
            format!("Got it: {answered} answered, {skipped} skipped. Back to work.")
        }
        (Feedback::Answered { answered, skipped }, Tr) => {
            format!("Tamam: {answered} yanıtlandı, {skipped} atlandı. İşe devam.")
        }
        (Feedback::AnswerFailed, En) => {
            "I could not pass that on. Open the app to answer, or try again.".into()
        }
        (Feedback::AnswerFailed, Tr) => {
            "Bunu iletemedim. Uygulamadan yanıtla ya da tekrar dene.".into()
        }
        (Feedback::Retried, En) => "Trying again.".into(),
        (Feedback::Retried, Tr) => "Yeniden deniyorum.".into(),
        (Feedback::RetryFailed, En) => "I could not start it again. Open the app to see why.".into(),
        (Feedback::RetryFailed, Tr) => {
            "Yeniden başlatamadım. Nedenini görmek için uygulamayı aç.".into()
        }
        (Feedback::TryAgainLabel, En) => "Try again".into(),
        (Feedback::TryAgainLabel, Tr) => "Tekrar dene".into(),
    }
}

/// The feedback for a reply that could not be read.
#[must_use]
pub fn reply_error_feedback(e: ReplyError) -> Feedback {
    match e {
        ReplyError::Empty => Feedback::Empty,
        ReplyError::NeedsNumbers => Feedback::NeedsNumbers,
        ReplyError::UnknownNumber(n) => Feedback::UnknownNumber(n),
    }
}

/// Short callback payload for the "Try again" button (fits Telegram's 64 bytes).
#[must_use]
pub fn retry_callback(notice_id: &str) -> String {
    format!("nr:{notice_id}")
}

/// The notice id in a `nr:` callback payload.
#[must_use]
pub fn parse_retry_callback(data: &str) -> Option<&str> {
    let id = data.strip_prefix("nr:")?;
    (!id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: NoticeKind) -> OwnerNotice {
        OwnerNotice {
            seq: 1,
            id: "00000000-0000-0000-0000-000000000001".into(),
            at_ms: 1,
            kind,
            source: NoticeSource::Mission,
            title: "Backlinks".into(),
            bot_name: "Scout".into(),
            bot_id: None,
            mission_id: Some("m1".into()),
            run_id: None,
            chat_id: None,
            reason: None,
            progress: Some(NoticeProgress {
                total: 10,
                done: 6,
                blocked: 2,
                failed: 1,
                skipped: 1,
            }),
            done: vec![],
            failed: vec![],
            questions: vec![],
            app_only: vec![],
            can_retry: false,
            late: false,
        }
    }

    fn q(key: &str) -> NoticeItem {
        NoticeItem {
            key: key.into(),
            title: format!("site {key}"),
            text: Some(format!("What name for {key}?")),
        }
    }

    #[test]
    fn renders_a_blocked_notice_in_both_languages_with_all_questions_in_one_message() {
        let mut n = notice(NoticeKind::Blocked);
        n.questions = vec![q("a.com"), q("b.com")];
        n.app_only = vec!["Pay $9 for a premium listing".into()];
        let en = render(&n, NoticeLang::En);
        assert!(en.starts_with("✋ Needs you: Backlinks (Scout)"));
        assert!(en.contains("6 of 10 done, 2 waiting for you, 1 given up."));
        assert!(en.contains("1. site a.com") && en.contains("2. site b.com"));
        assert!(en.contains("Waiting in the app (not from here)"));
        assert!(en.contains("Pay $9"));
        let tr = render(&n, NoticeLang::Tr);
        assert!(tr.contains("Senden bir şey gerekiyor") && tr.contains("/answer"));
        assert!(tr.contains("Uygulamada bekleyenler"));
    }

    #[test]
    fn a_finished_notice_lists_what_was_done_and_never_offers_an_answer() {
        let mut n = notice(NoticeKind::Finished);
        n.done = vec![NoticeItem {
            key: "a".into(),
            title: "a".into(),
            text: Some("account created on a.com".into()),
        }];
        let s = render(&n, NoticeLang::En);
        assert!(s.contains("✅ Finished") && s.contains("• account created on a.com"));
        assert!(!s.contains("/answer"));
    }

    #[test]
    fn a_single_question_takes_the_whole_text_and_skip_skips() {
        let mut n = notice(NoticeKind::Blocked);
        n.questions = vec![q("a.com")];
        let r = parse_reply("Use the shop name\nplease", &n).unwrap();
        assert_eq!(r[0].answer.as_deref(), Some("Use the shop name\nplease"));
        assert_eq!(parse_reply("skip", &n).unwrap()[0].answer, None);
        assert_eq!(parse_reply("Atla", &n).unwrap()[0].answer, None);
        assert_eq!(parse_reply("   ", &n), Err(ReplyError::Empty));
    }

    #[test]
    fn several_questions_need_numbered_lines_and_continuations_join() {
        let mut n = notice(NoticeKind::Blocked);
        n.questions = vec![q("a"), q("b"), q("c")];
        assert_eq!(parse_reply("yes", &n), Err(ReplyError::NeedsNumbers));
        let r = parse_reply("1: yes\n2) no thanks\nreally\nskip 3", &n).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].answer.as_deref(), Some("yes"));
        assert_eq!(r[1].answer.as_deref(), Some("no thanks\nreally"));
        assert_eq!(r[2].answer, None);
        assert_eq!(
            parse_reply("4: x", &n),
            Err(ReplyError::UnknownNumber(4)),
            "a number past the list is refused"
        );
        assert_eq!(parse_reply("0: x", &n), Err(ReplyError::UnknownNumber(0)));
    }

    #[test]
    fn only_listed_questions_are_answerable() {
        let mut n = notice(NoticeKind::Blocked);
        n.questions = (0..MAX_LISTED + 3).map(|i| q(&format!("k{i}"))).collect();
        let over = MAX_LISTED + 1;
        assert_eq!(
            parse_reply(&format!("{over}: x"), &n),
            Err(ReplyError::UnknownNumber(over))
        );
    }

    #[test]
    fn feedback_speaks_both_languages_for_every_case() {
        let all = [
            Feedback::NothingOpen,
            Feedback::Empty,
            Feedback::NeedsNumbers,
            Feedback::UnknownNumber(3),
            Feedback::Answered {
                answered: 2,
                skipped: 1,
            },
            Feedback::AnswerFailed,
            Feedback::Retried,
            Feedback::RetryFailed,
            Feedback::TryAgainLabel,
        ];
        for f in all {
            let (en, tr) = (feedback(NoticeLang::En, f), feedback(NoticeLang::Tr, f));
            assert!(!en.is_empty() && !tr.is_empty());
            assert_ne!(en, tr, "{f:?} is translated");
        }
        assert!(feedback(NoticeLang::En, Feedback::UnknownNumber(3)).contains('3'));
    }

    #[test]
    fn retry_callbacks_round_trip_and_reject_junk() {
        let id = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
        let data = retry_callback(id);
        assert!(data.len() <= 64);
        assert_eq!(parse_retry_callback(&data), Some(id));
        assert_eq!(parse_retry_callback("nr:"), None);
        assert_eq!(parse_retry_callback("nr:../x"), None);
        assert_eq!(parse_retry_callback("ap:abc"), None);
    }
}
