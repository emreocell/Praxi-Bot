//! `browser_handover`: the bot asks the OWNER for a step it must not do itself.
//!
//! Two kinds, both for the managed browser:
//!
//! * **CAPTCHA** (`kind: "captcha"`): the page shows a "prove you are human"
//!   check. The bot may try a simple one itself, at most [`CAPTCHA_OWN_ATTEMPTS`]
//!   actions per challenge (see [`HandoverHub::captcha_action_at`]); after that, or for a
//!   behavioural one, it hands over: the owner takes control in the live view,
//!   solves it and presses "Done". The daemon then checks that the check is gone
//!   before the run resumes. No solver service, no evasion plugin. Only the check
//!   itself and the submit that needs its token are gated: filling the ordinary
//!   fields of a form that merely embeds a CAPTCHA is never counted or refused.
//! * **One-time code** (`kind: "code"`): the page wants a 2FA code. The owner types
//!   it into a card the app draws; the daemon types it into the page like a secret.
//!   The code never reaches the model, the journal, the audit log or any log, and
//!   is never stored: it lives in one local variable for the time of the typing.
//!
//! The tool itself only validates (the page really shows such a prompt, read by
//! the daemon, not claimed by the model) and returns a card description the agent
//! loop turns into a parked request, exactly like `secret_request`. Detection
//! works on the rendered snapshot text ([`detect`]); a page can fake a prompt, so
//! the card names the site the BROWSER reports and warns when the run has read
//! untrusted content.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde_json::Value;

use crate::browser::BrowserDriver;

/// Name of the model-facing tool.
pub const BROWSER_HANDOVER: &str = "browser_handover";

/// `kind` of the JSON part the tool returns; the loop parks the run on it.
pub const HANDOVER_CARD_KIND: &str = "handover_card";

/// How many actions the bot may attempt on the same CAPTCHA.
pub const CAPTCHA_OWN_ATTEMPTS: u32 = 2;

/// What a handover asks the owner to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoverKind {
    /// Solve a "prove you are human" check in the live view.
    Captcha,
    /// Type a one-time code.
    Code,
}

impl HandoverKind {
    /// The wire name (`captcha` | `code`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Captcha => "captcha",
            Self::Code => "code",
        }
    }
}

/// A validated request, as the loop reads it back from the card part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverCard {
    /// What the owner is asked to do.
    pub kind: HandoverKind,
    /// Origin the browser reports for the page (never page text).
    pub site: String,
}

impl HandoverCard {
    /// Read the card part [`browser_handover_card`] produced. `None` for any other JSON.
    #[must_use]
    pub fn from_part(data: &Value) -> Option<Self> {
        Some(Self {
            kind: match data.get("handover")?.as_str()? {
                "captcha" => HandoverKind::Captcha,
                "code" => HandoverKind::Code,
                _ => return None,
            },
            site: data.get("site")?.as_str()?.to_owned(),
        })
    }
}

/// The card part for `kind` on `site`.
#[must_use]
pub fn browser_handover_card(kind: HandoverKind, site: &str) -> Value {
    serde_json::json!({
        "kind": HANDOVER_CARD_KIND,
        "handover": kind.as_str(),
        "site": site,
    })
}

// ── Detection ────────────────────────────────────────────────────────────────

/// What a page asks for that the bot must not (or cannot) do alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    /// A CAPTCHA. `hard`: a behavioural or picture-tile one the bot should not try.
    Captcha {
        /// Press-and-hold, tile or slider puzzles: hand over without trying.
        hard: bool,
    },
    /// A one-time code prompt; `field` is the ref of the field that takes it.
    Code {
        /// Ref (`e12`) of the code field.
        field: Option<String>,
    },
}

/// Prompts that name a CAPTCHA wherever they appear (a challenge the user is asked to pass).
const CAPTCHA_STRONG: &[&str] = &[
    "not a robot",
    "verify you are human",
    "verify you're human",
    "verify that you are human",
    "prove you are human",
    "security challenge",
    "robot değilim",
    "insan olduğunuzu",
    "ich bin kein roboter",
    "sind sie ein mensch",
    "bestätigen sie, dass sie ein mensch",
];

/// Names of CAPTCHA widgets. On their own they are not a challenge: reCAPTCHA v3 puts a
/// passive badge (an iframe titled "reCAPTCHA") and a "protected by reCAPTCHA" footer on
/// a very large number of ordinary forms.
const CAPTCHA_WIDGETS: &[&str] = &["recaptcha", "hcaptcha", "turnstile", "captcha"];

/// Text of a passive notice (badge, footer) that a CAPTCHA service is merely present.
const CAPTCHA_PASSIVE: &[&str] = &[
    "protected by recaptcha",
    "protected by hcaptcha",
    "protected by turnstile",
    "recaptcha privacy",
    "hcaptcha privacy",
    "this site is protected",
];

/// Markers of a puzzle that needs pointer behaviour or picture tiles.
const CAPTCHA_HARD: &[&str] = &[
    "press & hold",
    "press and hold",
    "select all images",
    "select all squares",
    "click each image",
    "click on all",
    "slide to",
    "drag the slider",
    "rotate the",
    "complete the puzzle",
    "basılı tutun",
    "tüm görselleri seçin",
    "gedrückt halten",
    "alle bilder auswählen",
];

/// Words that name a one-time-code field.
const CODE_FIELD_WORDS: &[&str] = &[
    "verification code",
    "security code",
    "authentication code",
    "authenticator code",
    "confirmation code",
    "one-time",
    "one time",
    "otp",
    "2fa",
    "two-factor",
    "6-digit",
    "6 digit",
    "sms code",
    "passcode",
    "doğrulama kodu",
    "güvenlik kodu",
    "onay kodu",
    "tek kullanımlık",
    "bestätigungscode",
    "sicherheitscode",
    "verifizierungscode",
    "einmalcode",
    "einmalpasswort",
];

/// Fields that merely contain the word "code" for another reason.
const CODE_FIELD_NOT: &[&str] = &[
    "coupon",
    "promo",
    "discount",
    "voucher",
    "gift",
    "zip",
    "postal",
    "post code",
    "postcode",
    "country",
    "area code",
    "kupon",
    "indirim",
    "posta kodu",
    "gutschein",
    "rabatt",
    "postleitzahl",
    "password",
    "parola",
    "şifre",
    "passwort",
];

/// Prompt sentences that say a code was sent / is needed.
const CODE_PROMPTS: &[&str] = &[
    "enter the code",
    "enter the 6",
    "enter your code",
    "code we sent",
    "code we've sent",
    "we sent a code",
    "we've sent a code",
    "sent you a code",
    "sent a verification code",
    "verification code",
    "two-step verification",
    "2-step verification",
    "two-factor",
    "authenticator app",
    "kodu girin",
    "gönderdiğimiz kod",
    "doğrulama kodu",
    "iki adımlı doğrulama",
    "iki aşamalı",
    "code eingeben",
    "geben sie den code",
    "bestätigungscode",
    "zwei-faktor",
    "zwei-schritt",
];

/// Roles that take typed text.
fn takes_text(role: &str) -> bool {
    matches!(role, "textbox" | "spinbutton" | "searchbox")
}

/// Role and quoted name of a snapshot line like `  [e5] textbox "Code" value="x"`.
fn parts(line: &str) -> (Option<String>, &str, String) {
    let mut rest = line.trim_start();
    let mut r = None;
    if let Some(stripped) = rest.strip_prefix('[')
        && let Some((id, tail)) = stripped.split_once(']')
    {
        r = Some(id.to_owned());
        rest = tail.trim_start();
    }
    let role_end = rest.find([' ', '"']).unwrap_or(rest.len());
    let role = &rest[..role_end];
    let name = rest[role_end..]
        .trim_start()
        .strip_prefix('"')
        .and_then(|n| n.split('"').next())
        .unwrap_or("")
        .to_owned();
    (r, role, name)
}

/// Look for a CAPTCHA or a one-time-code prompt in the snapshot text of a page.
///
/// A CAPTCHA wins over a code prompt (the check comes first). The result only
/// ever feeds a daemon-written notice and the `browser_handover` check: page text
/// cannot make the bot do anything by itself.
#[must_use]
pub fn detect(snapshot: &str) -> Option<Challenge> {
    let mut captcha = false;
    let mut widget_seen = false;
    let mut hard = false;
    let mut prompt = false;
    let mut field: Option<String> = None;
    let mut first_text: Option<String> = None;
    for line in snapshot.lines() {
        let low = line.to_lowercase();
        let (r, role, name) = parts(line);
        let name_low = name.to_lowercase();
        let widget = CAPTCHA_WIDGETS.iter().any(|m| low.contains(m));
        // A bare iframe titled like the service, or the footer sentence, is a passive badge.
        let passive = widget
            && (CAPTCHA_PASSIVE.iter().any(|m| low.contains(m))
                || (role.eq_ignore_ascii_case("iframe")
                    && CAPTCHA_WIDGETS.iter().any(|m| name_low.trim() == *m)));
        if CAPTCHA_STRONG.iter().any(|m| low.contains(m))
            || (widget
                && !passive
                && (low.contains("challenge")
                    || low.contains("widget")
                    || matches!(
                        role,
                        "iframe" | "checkbox" | "img" | "image" | "textbox" | "button" | "group"
                    )))
        {
            captcha = true;
        }
        if widget && !passive {
            widget_seen = true;
        }
        if CAPTCHA_HARD.iter().any(|m| low.contains(m)) {
            hard = true;
        }
        if CODE_PROMPTS.iter().any(|m| low.contains(m)) {
            prompt = true;
        }
        if let (Some(r), true) = (&r, takes_text(role)) {
            let blocked = CODE_FIELD_NOT.iter().any(|m| name_low.contains(m));
            if !blocked && first_text.is_none() {
                first_text = Some(r.clone());
            }
            if !blocked && field.is_none() && CODE_FIELD_WORDS.iter().any(|m| name_low.contains(m))
            {
                field = Some(r.clone());
            }
        }
    }
    // A picture or pointer puzzle next to a named (non-passive) widget is a CAPTCHA too.
    if captcha || (hard && widget_seen) {
        return Some(Challenge::Captcha {
            hard: hard || snapshot.to_lowercase().contains("hcaptcha"),
        });
    }
    if field.is_some() {
        return Some(Challenge::Code { field });
    }
    if prompt && first_text.is_some() {
        return Some(Challenge::Code { field: first_text });
    }
    None
}

/// What a browser action reaches, as far as the CAPTCHA budget is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach<'a> {
    /// A named element (`e12`) that is clicked, filled or selected.
    Element(&'a str),
    /// A named element that is only pointed at (hover, select text in it): judged like a click,
    /// because pointing at the check or the submit is probing it.
    Look(&'a str),
    /// Acts where the focus is or by coordinates, so it could land on the check
    /// (Enter, Space, a pointer click, a script): counted like the check itself.
    Anywhere,
    /// Loads this address in the browser. Going to another site, or to another page of this
    /// one, touches nothing; a same-site address with a query string could send the form
    /// (a GET form), so that counts like the submit (see [`navigation_submits`]).
    Navigate(&'a str),
    /// Touches no element (leaving the page, switching a tab, scrolling).
    Nothing,
}

/// What a snapshot element is, for the CAPTCHA budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Touch {
    /// The check itself, or anything that could act on it: a tick box, a radio, a slider, a
    /// canvas, a picture, anything inside a frame, and every role the budget does not know.
    Check,
    /// A plainly typed field of the form (text, search, number, select list).
    Field,
    /// A link: it leaves the page, it cannot solve the check or send the form.
    Link,
    /// A button or menu entry: it may send the form.
    Submit,
    /// The ref is not in the snapshot.
    Unknown,
}

/// A line that names the check or one of its prompts.
fn names_check(low: &str) -> bool {
    CAPTCHA_WIDGETS.iter().any(|m| low.contains(m))
        || CAPTCHA_STRONG.iter().any(|m| low.contains(m))
        || CAPTCHA_HARD.iter().any(|m| low.contains(m))
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Roles that hold another document or tree: whatever is inside one could be the challenge
/// (the title of a frame says nothing a page cannot choose freely).
fn holds_frame(role: &str) -> bool {
    matches!(
        role.to_ascii_lowercase().as_str(),
        "iframe" | "frame" | "frameset" | "embed" | "object" | "shadow-host" | "shadow-root"
    )
}

/// Classify the element `target` of `snapshot`, while a CAPTCHA is on the page.
///
/// A label proves nothing, so the ROLE decides: only typed fields and links are ordinary, and
/// everything that can be ticked, dragged or pointed at (checkbox, radio, switch, slider,
/// canvas, image, a role this code does not know) acts on a check. Anything inside a frame
/// counts as the check whatever the frame is called, and an element that itself names the
/// check does too: those two rules only ever add to the budget, never take from it.
fn touch(snapshot: &str, target: &str) -> Touch {
    let lines: Vec<&str> = snapshot.lines().collect();
    let Some(at) = lines
        .iter()
        .position(|l| parts(l).0.as_deref() == Some(target))
    else {
        return Touch::Unknown;
    };
    if names_check(&lines[at].to_lowercase()) {
        return Touch::Check;
    }
    let mut depth = indent(lines[at]);
    // Line 0 is the page title (its address may say "captcha"): never a frame.
    for line in lines.iter().take(at).skip(1).rev() {
        let d = indent(line);
        if d < depth {
            depth = d;
            if holds_frame(parts(line).1) {
                return Touch::Check;
            }
        }
    }
    match parts(lines[at]).1.to_ascii_lowercase().as_str() {
        "textbox" | "searchbox" | "spinbutton" | "combobox" | "listbox" => Touch::Field,
        "link" => Touch::Link,
        "button" | "menuitem" | "tab" | "popupbutton" | "disclosuretriangle" => Touch::Submit,
        _ => Touch::Check,
    }
}

/// Whether loading `target` from the page at `current` could send the page's form: it stays on
/// the same site and carries a query string (what a GET form sends, a guessed answer included).
/// Another site, or another page of this one without a query, only leaves.
#[must_use]
pub fn navigation_submits(current: Option<&str>, target: &str) -> bool {
    let target = target.trim();
    let (Some(current), Ok(target_url)) = (current, url::Url::parse(target)) else {
        return false;
    };
    let same_site = match (
        crate::browser::origin_of(current),
        crate::browser::origin_of(target),
    ) {
        (Some(a), Some(b)) => site_key(Some(&a)) == site_key(Some(&b)),
        _ => false,
    };
    same_site && target_url.query().is_some_and(|q| !q.is_empty())
}

/// The notice appended to a snapshot that shows `challenge`; `seen` counts the
/// looks at a CAPTCHA so far (this one included). Written by the daemon.
#[must_use]
pub fn notice(challenge: &Challenge, seen: u32) -> String {
    match challenge {
        Challenge::Captcha { hard } if !*hard && seen <= CAPTCHA_OWN_ATTEMPTS => format!(
            "[Praxi notice] This page shows a CAPTCHA (a check that wants proof of a human). If you can see images and it is a simple one (an \"I'm not a robot\" checkbox or an easy picture or text puzzle) you may try it yourself, as an ordinary user would: attempt {seen} of {CAPTCHA_OWN_ATTEMPTS}. Typing into the text fields of the form and following links is free; ticking a box, pressing Tab or Enter, pointing at or pressing the check or the submit button each count as an attempt. Never use a solving service or tricks. If it does not work, or you cannot see images, call browser_handover with kind \"captcha\"."
        ),
        Challenge::Captcha { .. } => "[Praxi notice] This page shows a CAPTCHA you must not try to solve yourself (it is a behavioural or picture puzzle, or your own attempts are used up). You may still fill the ordinary fields of the form, but it cannot be submitted until the check is solved. Call browser_handover with kind \"captcha\": the owner takes control, solves it, and the run continues.".to_owned(),
        Challenge::Code { field } => format!(
            "[Praxi notice] This page asks for a one-time code (2FA){}. Do NOT ask the owner to type the code in chat and do not guess one: call browser_handover with kind \"code\". The owner enters it in a private card and Praxi types it into the page; you never see it.",
            field
                .as_deref()
                .map_or_else(String::new, |f| format!(", the code field is [{f}]"))
        ),
    }
}

// ── Hub ──────────────────────────────────────────────────────────────────────

/// Empty documents and iframe placeholders are not evidence of a completed page.
pub(crate) fn rendered_page(snapshot: &str) -> bool {
    !snapshot.lines().any(|line| parts(line).1 == "iframe")
        && snapshot.lines().skip(1).any(|line| {
            matches!(
                parts(line).1,
                "heading" | "text" | "textbox" | "button" | "link" | "paragraph"
            )
        })
}

/// Why the daemon could not finish an owner's answer. The owner's card shows
/// these in plain words and stays open so they can try again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoverError {
    /// Nothing is armed for this run (it ended, or the tool was never called).
    NotArmed,
    /// The code is not 4 to 12 letters or digits.
    BadCode,
    /// The page is not on the site the card named.
    WrongPage,
    /// The page no longer shows a code prompt.
    NoPrompt,
    /// The check is still on the page.
    StillThere,
    /// The browser could not take the code (the message never holds it).
    Typing(String),
}

impl std::fmt::Display for HandoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotArmed => "this request is not waiting any more",
            Self::BadCode => "a code has 4 to 12 letters or digits",
            Self::WrongPage => "the browser is not on the site the card named any more",
            Self::NoPrompt => "the page does not ask for a code any more",
            Self::StillThere => {
                "the page still shows the check and looks unchanged; if it is solved, press Done again to continue anyway"
            }
            Self::Typing(m) => m,
        })
    }
}

#[derive(Clone)]
struct Armed {
    driver: Arc<dyn BrowserDriver>,
    kind: HandoverKind,
    origin: String,
    fingerprint: u64,
    cancelled: Arc<AtomicBool>,
    /// Fingerprint of the page at the owner's last "Done" that found the check still
    /// there: pressing Done again on the very same page is the owner's own confirmation.
    unconfirmed: Arc<Mutex<Option<u64>>>,
}

/// How a CAPTCHA handover was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolvedBy {
    /// The daemon saw the page move on (same site, a changed page, no challenge).
    Page,
    /// The page still looked the same, and the owner pressed Done a second time on it.
    OwnerConfirmed,
}

/// What the daemon remembers about CAPTCHAs one run (and its subagents) met.
///
/// The attempt budget belongs to a SITE, never to a page, a moment or a child run:
/// leaving the page, opening another site, coming back, refreshing or asking a subagent
/// to try gives no attempt back. Only the owner's "Done" ([`HandoverHub::verify_solved`])
/// clears it.
#[derive(Default)]
struct CaptchaBudget {
    /// Own actions spent, per site key (see [`site_key`]).
    attempts: HashMap<String, u32>,
}

/// The budget key of an origin: scheme and port do not matter, and `www.` is the same site.
fn site_key(origin: Option<&str>) -> String {
    let o = origin.unwrap_or_default().to_ascii_lowercase();
    let host = o.split_once("://").map_or(o.as_str(), |(_, h)| h);
    let host = host.split('/').next().unwrap_or_default();
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

fn fingerprint(snapshot: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    snapshot.hash(&mut hash);
    hash.finish()
}

/// The armed handovers of the process, keyed by run id.
#[derive(Default)]
pub struct HandoverHub {
    armed: Mutex<HashMap<String, Armed>>,
    /// Keep a bot's browser session open until the owner may finish a step on the page.
    browser_holds: Mutex<HashMap<String, std::time::Instant>>,
    /// Own action budgets per ROOT run.
    seen: Mutex<HashMap<String, CaptchaBudget>>,
    /// Subagent run -> root run of its family.
    parents: Mutex<HashMap<String, String>>,
    /// The browser each run last used (to look at its page when a desktop tool is called).
    browsers: Mutex<HashMap<String, Weak<dyn BrowserDriver>>>,
    captures: Mutex<Vec<(Weak<dyn BrowserDriver>, String)>>,
}

/// The process-wide hub the browser tool arms and the daemon's owner route answers.
#[must_use]
pub fn handover_hub() -> &'static HandoverHub {
    static HUB: OnceLock<HandoverHub> = OnceLock::new();
    HUB.get_or_init(HandoverHub::default)
}

/// Strip separators and check a typed code (`123 456`, `123-456`).
fn clean_code(raw: &str) -> Result<String, HandoverError> {
    let c: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if (4..=12).contains(&c.chars().count()) && c.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(c)
    } else {
        Err(HandoverError::BadCode)
    }
}

impl HandoverHub {
    /// The run at the top of `run_id`'s family (itself for a top-level run).
    fn root(&self, run_id: &str) -> String {
        let parents = self
            .parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut at = run_id;
        for _ in 0..8 {
            match parents.get(at) {
                Some(p) => at = p,
                None => break,
            }
        }
        at.to_owned()
    }

    /// Tell the hub `child` is a subagent run of `parent`: they share one CAPTCHA budget.
    pub fn link_child(&self, child: &str, parent: &str) {
        let root = self.root(parent);
        self.parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(child.to_owned(), root);
    }

    fn with_budget<T>(&self, run_id: &str, f: impl FnOnce(&mut CaptchaBudget) -> T) -> T {
        let root = self.root(run_id);
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(seen.entry(root).or_default())
    }

    /// Clear the budget of one site after the owner's "Done".
    fn clear_captcha(&self, run_id: &str, origin: &str) {
        let key = site_key(Some(origin));
        self.with_budget(run_id, |b| {
            b.attempts.remove(&key);
        });
    }

    /// Admit one own action that may land on the check ([`Reach::Anywhere`]).
    ///
    /// # Errors
    /// Refuses hard challenges and actions after the second own attempt at this site.
    pub fn captcha_action(
        &self,
        run_id: &str,
        origin: Option<&str>,
        snapshot: &str,
    ) -> Result<(), String> {
        self.captcha_action_at(run_id, origin, snapshot, Reach::Anywhere)
    }

    /// Admit one own action on the page at `origin`, never an unbounded batch or a third attempt.
    ///
    /// While a CAPTCHA is on the page only plainly typed fields (text, search, number, select
    /// list) and links are free, and so are scrolling, going back and switching a tab. A box
    /// to tick, a radio, a slider, a canvas, a picture, anything inside a frame, a button (the
    /// submit), pressing Tab, Enter or a pointer click, hovering the check or the submit, and
    /// a same-site address with a query each cost one of the own attempts; a hard check
    /// refuses them all. So a text puzzle cannot be guessed over and over by typing an answer
    /// and submitting it, and a behavioural one cannot be probed by tabbing or hovering.
    ///
    /// # Errors
    /// Refuses hard challenges and actions after the second own attempt at this site.
    pub fn captcha_action_at(
        &self,
        run_id: &str,
        origin: Option<&str>,
        snapshot: &str,
        reach: Reach<'_>,
    ) -> Result<(), String> {
        let Some(Challenge::Captcha { hard }) = detect(snapshot) else {
            return Ok(());
        };
        let kind = match reach {
            Reach::Nothing => return Ok(()),
            Reach::Anywhere | Reach::Navigate(_) => Touch::Check,
            // Pointing at the check or the submit is probing it (a puzzle can answer to a
            // hover), so a look is judged like a click: only an ordinary field or a link is free.
            Reach::Element(r) | Reach::Look(r) => touch(snapshot, r),
        };
        match kind {
            Touch::Field | Touch::Link => return Ok(()),
            Touch::Submit if hard => {
                return Err("this page's CAPTCHA is not solved, so its form cannot be sent: fill the ordinary fields if you like, but hand over with browser_handover(kind: captcha) before submitting".to_owned());
            }
            Touch::Check | Touch::Submit | Touch::Unknown => {}
        }
        let key = site_key(origin);
        self.with_budget(run_id, |b| {
            let n = b.attempts.entry(key).or_default();
            if hard || *n >= CAPTCHA_OWN_ATTEMPTS {
                return Err("CAPTCHA own attempts exhausted: hand over with browser_handover(kind: captcha) before any further browser action on the check or the submit".to_owned());
            }
            *n += 1;
            Ok(())
        })
    }

    /// Number of the next permitted own attempt at `origin` (observations do not consume it).
    pub fn next_captcha_attempt(&self, run_id: &str, origin: Option<&str>) -> u32 {
        let key = site_key(origin);
        self.with_budget(run_id, |b| b.attempts.get(&key).copied().unwrap_or(0) + 1)
    }

    /// Remember the browser `run_id` is using, so a desktop tool can look at its page.
    pub fn remember_browser(&self, run_id: &str, driver: &Arc<dyn BrowserDriver>) {
        self.browsers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run_id.to_owned(), Arc::downgrade(driver));
    }

    /// Keep the managed browser of `bot_id` from idle-closing until `until`.
    pub fn hold_browser(&self, bot_id: &str, until: std::time::Instant) {
        self.browser_holds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(bot_id.to_owned(), until);
    }

    /// Drop a browser hold early (the owner finished or gave up).
    pub fn clear_browser_hold(&self, bot_id: &str) {
        self.browser_holds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(bot_id);
    }

    /// Whether a mission hand-over still needs the page open for `bot_id`.
    #[must_use]
    pub fn browser_hold_active(&self, bot_id: &str) -> bool {
        let mut holds = self
            .browser_holds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(until) = holds.get(bot_id) else {
            return false;
        };
        if std::time::Instant::now() >= *until {
            holds.remove(bot_id);
            false
        } else {
            true
        }
    }

    /// Whether a CAPTCHA is on the page of this run's VISIBLE managed browser right now
    /// (read fresh from the browser, not remembered), or a CAPTCHA handover is waiting.
    /// The desktop input tools refuse then: a visible browser window could be clicked
    /// through them without the attempt cap.
    pub async fn captcha_on_screen(&self, run_id: &str) -> bool {
        if self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(run_id)
            .is_some_and(|a| a.kind == HandoverKind::Captcha)
        {
            return true;
        }
        let root = self.root(run_id);
        let driver = {
            let browsers = self
                .browsers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            browsers
                .get(run_id)
                .or_else(|| browsers.get(&root))
                .and_then(Weak::upgrade)
        };
        let Some(driver) = driver else {
            return false;
        };
        if !driver.on_desktop() {
            return false;
        }
        driver
            .snapshot(false)
            .await
            .is_ok_and(|snap| matches!(detect(&snap), Some(Challenge::Captcha { .. })))
    }

    /// Treat the page `driver` shows as holding a secret in the clear (a value captured from
    /// it): visual observations are withheld until the page navigates.
    pub(crate) async fn latch_visible_secret(&self, driver: &Arc<dyn BrowserDriver>) {
        let epoch = driver.capture_epoch().await.unwrap_or_default();
        self.captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((Arc::downgrade(driver), epoch));
    }

    /// Whether any browser holds a freshly typed code on its page right now.
    pub async fn typed_secret_visible(&self) -> bool {
        let drivers: Vec<Arc<dyn BrowserDriver>> = {
            let mut captures = self
                .captures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            captures.retain(|(d, _)| d.strong_count() > 0);
            captures.iter().filter_map(|(d, _)| d.upgrade()).collect()
        };
        for d in &drivers {
            if self.capture_latched(d).await {
                return true;
            }
        }
        false
    }

    /// Suppress visual observations of a driver holding a freshly typed code.
    /// The latch survives the approval/run ending. One entry exists per (page session,
    /// navigation); it is dropped only when a caller on the SAME page session has moved
    /// on to a newer navigation, never because another tab or lane asked.
    pub async fn capture_latched(&self, driver: &Arc<dyn BrowserDriver>) -> bool {
        let now = driver.capture_epoch().await.unwrap_or_default();
        let (session, nav) = epoch_parts(&now);
        let mut captures = self
            .captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        captures.retain(|(d, at)| {
            d.upgrade().is_some_and(|d| {
                let (s, n) = epoch_parts(at);
                !Arc::ptr_eq(&d, driver) || s != session || n == nav
            })
        });
        captures
            .iter()
            .any(|(d, _)| d.upgrade().is_some_and(|d| Arc::ptr_eq(&d, driver)))
    }

    /// Remember what a handover of `run_id` needs to finish: the driver, the kind
    /// and the origin the owner was told about.
    pub fn arm(
        &self,
        run_id: &str,
        driver: Arc<dyn BrowserDriver>,
        kind: HandoverKind,
        origin: &str,
        fingerprint: &str,
    ) {
        self.armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                run_id.to_owned(),
                Armed {
                    driver,
                    kind,
                    origin: origin.to_owned(),
                    fingerprint: self::fingerprint(fingerprint),
                    cancelled: Arc::new(AtomicBool::new(false)),
                    unconfirmed: Arc::new(Mutex::new(None)),
                },
            );
    }

    /// Invalidate the per-call token without restoring CAPTCHA attempts.
    pub fn disarm(&self, run_id: &str) {
        if let Some(a) = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(run_id)
        {
            a.cancelled.store(true, Ordering::SeqCst);
        }
    }

    /// Drop run state after Stop or run completion. A subagent ending leaves the family's
    /// CAPTCHA budget alone; only the root run does.
    pub fn forget(&self, run_id: &str) {
        self.disarm(run_id);
        self.browsers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(run_id);
        let is_child = self
            .parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(run_id)
            .is_some();
        if !is_child {
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(run_id);
        }
    }

    fn get(&self, run_id: &str, kind: HandoverKind) -> Result<Armed, HandoverError> {
        let armed = self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let a = armed.get(run_id).ok_or(HandoverError::NotArmed)?;
        if a.kind != kind {
            return Err(HandoverError::NotArmed);
        }
        Ok(a.clone())
    }

    /// Type the owner's one-time `code` into the page of `run_id`.
    ///
    /// The code is checked, typed through the driver's secret path (which refuses
    /// unless the receiving frame is on the origin the card named, re-read from
    /// the browser right before the text is inserted) and dropped. Nothing
    /// returned or logged contains it.
    ///
    /// # Errors
    /// See [`HandoverError`]; the message never contains the code.
    pub async fn type_code(&self, run_id: &str, code: &str) -> Result<(), HandoverError> {
        let code = clean_code(code)?;
        let Armed {
            driver,
            origin,
            cancelled,
            ..
        } = self.get(run_id, HandoverKind::Code)?;
        let here = driver
            .current_url()
            .await
            .and_then(|u| crate::browser::origin_of(&u));
        if here.as_deref() != Some(origin.as_str()) {
            return Err(HandoverError::WrongPage);
        }
        // A fresh read: the field is found again, so a page that moved on or a
        // stale ref never receives the code.
        let snap = driver
            .snapshot(false)
            .await
            .map_err(|e| HandoverError::Typing(scrub(&e, &code)))?;
        let Some(Challenge::Code { field: Some(field) }) = detect(&snap) else {
            return Err(HandoverError::NoPrompt);
        };
        let want = origin.clone();
        let accept = |o: Option<String>| {
            let armed = self
                .armed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            armed
                .get(run_id)
                .is_some_and(|a| Arc::ptr_eq(&a.cancelled, &cancelled))
                && !cancelled.load(Ordering::SeqCst)
                && o.as_deref().and_then(crate::browser::origin_of).as_deref()
                    == Some(want.as_str())
        };
        if !accept(driver.current_url().await) {
            return Err(HandoverError::NotArmed);
        }
        let url = driver.capture_epoch().await.unwrap_or_default();
        if !accept(driver.current_url().await) {
            return Err(HandoverError::NotArmed);
        }
        self.captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((Arc::downgrade(&driver), url));
        match driver.fill_secret(&field, &code, &accept).await {
            Ok(_) => Ok(()),
            Err(e) => Err(HandoverError::Typing(scrub(&e, &code))),
        }
    }

    /// Check that the CAPTCHA of `run_id` is gone (the owner pressed "Done").
    ///
    /// The page must be on the same site, rendered, changed since the handover began and
    /// without the challenge. When it still looks the same, the first Done is refused and
    /// remembered; a second Done on that very same page counts as the owner's own
    /// confirmation (the page can keep a passive widget after it was solved).
    ///
    /// # Errors
    /// [`HandoverError::StillThere`] while the page still shows it.
    pub async fn verify_solved(&self, run_id: &str) -> Result<SolvedBy, HandoverError> {
        let Armed {
            driver,
            origin,
            fingerprint: at_arm,
            cancelled,
            unconfirmed,
            ..
        } = self.get(run_id, HandoverKind::Captcha)?;
        if driver
            .current_url()
            .await
            .and_then(|u| crate::browser::origin_of(&u))
            .as_deref()
            != Some(origin.as_str())
        {
            return Err(HandoverError::WrongPage);
        }
        let snap = driver
            .snapshot(false)
            .await
            .map_err(HandoverError::Typing)?;
        // Empty documents and iframe placeholders are not evidence of completion.
        if cancelled.load(Ordering::SeqCst) {
            return Err(HandoverError::NotArmed);
        }
        if driver
            .current_url()
            .await
            .and_then(|u| crate::browser::origin_of(&u))
            .as_deref()
            != Some(origin.as_str())
        {
            return Err(HandoverError::WrongPage);
        }
        let now = fingerprint(&snap);
        if rendered_page(&snap)
            && now != at_arm
            && !matches!(detect(&snap), Some(Challenge::Captcha { .. }))
        {
            self.clear_captcha(run_id, &origin);
            return Ok(SolvedBy::Page);
        }
        let mut last = unconfirmed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *last == Some(now) && snap.lines().count() >= 2 {
            drop(last);
            self.clear_captcha(run_id, &origin);
            return Ok(SolvedBy::OwnerConfirmed);
        }
        *last = Some(now);
        Err(HandoverError::StillThere)
    }
}

/// `(page session, navigation)` of a capture epoch (`session:nav`; a driver that only knows
/// URLs uses the URL, which then splits at its last colon: still consistent per page).
fn epoch_parts(epoch: &str) -> (&str, &str) {
    epoch.rsplit_once(':').unwrap_or((epoch, ""))
}

/// Remove every copy of `code` from `text`.
fn scrub(text: &str, code: &str) -> String {
    if code.is_empty() {
        text.to_owned()
    } else {
        text.replace(code, "[code]")
    }
}

#[cfg(test)]
mod tests;
