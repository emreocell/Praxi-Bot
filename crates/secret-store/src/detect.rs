//! Precision-first detection of a pasted credential in a chat message.
//!
//! See `docs/design/secret-detection.md` for the rules and the reasoning.
//! The detector is pure and local: no I/O, no network, no logging. A
//! [`SecretCandidate`] holds a kind, a confidence and a byte span, never the
//! matched text, so printing one (`Debug`, `Display`) cannot leak the value.
//!
//! Two sources of candidates:
//! * **Known shapes** ([`Confidence::High`]): fixed provider prefixes and
//!   lengths. The shapes the [`crate::Redactor`] already masks, plus a few
//!   more documented ones listed in [`EXTRA_SHAPES`].
//! * **Labelled context** ([`Confidence::Medium`]): a random-looking value
//!   right after a label such as `api key`, `token`, `password`, `şifre`.
//! * **Card numbers** ([`Confidence::Medium`]): a Luhn-valid number with a
//!   real card-network prefix, either written in the usual groups or
//!   contiguous right after a word such as `card` / `kart`.
//!
//! Entropy and character mix only ever *support* a labelled match. Text with
//! no known shape and no label is never reported.

use std::fmt;
use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

use crate::redact::SHAPES;
use crate::secret::validate_name;

/// What a candidate most likely is. The text is for logs and counters; the UI
/// maps each kind to its own localised wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SecretKind {
    /// `sk-...` (`OpenAI`, Anthropic, `OpenRouter` style).
    OpenAiStyleKey,
    /// `AIza...` Google API key.
    GoogleApiKey,
    /// `ghp_...`, `github_pat_...`.
    GithubToken,
    /// `xoxb-...` Slack token.
    SlackToken,
    /// `nvapi-...`.
    NvidiaKey,
    /// Three-part JSON Web Token.
    Jwt,
    /// `AKIA...` / `ASIA...` AWS access key id.
    AwsAccessKeyId,
    /// `sk_live_...` / `rk_live_...` Stripe live key.
    StripeKey,
    /// `123456789:AA...` Telegram bot token.
    TelegramBotToken,
    /// `npm_...`.
    NpmToken,
    /// `glpat-...` GitLab personal access token.
    GitlabToken,
    /// `hf_...` Hugging Face token.
    HuggingFaceToken,
    /// `gsk_...` Groq key.
    GroqKey,
    /// `dop_v1_...` `DigitalOcean` token.
    DigitalOceanToken,
    /// A PEM private key block.
    PrivateKey,
    /// A payment card number (Luhn-valid, known network prefix).
    PaymentCard,
    /// Random-looking value labelled "api key" / "anahtar".
    LabelledApiKey,
    /// Random-looking value labelled "token" / "bearer" / "jeton".
    LabelledToken,
    /// Random-looking value labelled "secret" / "private key".
    LabelledSecret,
    /// Password-looking value labelled "password" / "şifre" / "parola".
    LabelledPassword,
}

impl SecretKind {
    /// Stable `snake_case` id, safe for counters and logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiStyleKey => "openai_style_key",
            Self::GoogleApiKey => "google_api_key",
            Self::GithubToken => "github_token",
            Self::SlackToken => "slack_token",
            Self::NvidiaKey => "nvidia_key",
            Self::Jwt => "jwt",
            Self::AwsAccessKeyId => "aws_access_key_id",
            Self::StripeKey => "stripe_key",
            Self::TelegramBotToken => "telegram_bot_token",
            Self::NpmToken => "npm_token",
            Self::GitlabToken => "gitlab_token",
            Self::HuggingFaceToken => "huggingface_token",
            Self::GroqKey => "groq_key",
            Self::DigitalOceanToken => "digitalocean_token",
            Self::PrivateKey => "private_key",
            Self::PaymentCard => "payment_card",
            Self::LabelledApiKey => "labelled_api_key",
            Self::LabelledToken => "labelled_token",
            Self::LabelledSecret => "labelled_secret",
            Self::LabelledPassword => "labelled_password",
        }
    }
}

impl fmt::Display for SecretKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How sure the detector is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// A label and a random-looking value next to it.
    Medium,
    /// A documented provider shape (fixed prefix and length).
    High,
}

/// One suspected secret: kind, confidence and where it is. Never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretCandidate {
    /// What it looks like.
    pub kind: SecretKind,
    /// How sure the detector is.
    pub confidence: Confidence,
    /// Byte offset of the first byte of the value (inclusive).
    pub start: usize,
    /// Byte offset one past the last byte of the value (exclusive).
    pub end: usize,
}

impl SecretCandidate {
    /// The value's byte range in the text that was scanned.
    #[must_use]
    pub fn span(&self) -> Range<usize> {
        self.start..self.end
    }

    /// The matched text, borrowed from `text` (which must be the string that
    /// was scanned). The caller owns what it does with it: do not log it.
    #[must_use]
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.span())
    }
}

impl fmt::Display for SecretCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({:?})", self.kind, self.confidence)
    }
}

/// The placeholder a message carries instead of the value once the owner
/// stored it: `[secret: login/openai]`. `None` when `name` is not a valid
/// secret name (see [`crate::validate_name`]).
#[must_use]
pub fn placeholder(name: &str) -> Option<String> {
    validate_name(name).ok()?;
    Some(format!("[secret: {name}]"))
}

/// Extra documented shapes that only the detector knows, `(id, kind, regex)`.
/// Candidates to be promoted to the redactor later (that changes masking of
/// tool output, so it is a separate decision).
const EXTRA_SHAPES: &[(&str, SecretKind, &str)] = &[
    (
        "aws",
        SecretKind::AwsAccessKeyId,
        r"(?:AKIA|ASIA)[0-9A-Z]{16}",
    ),
    (
        "stripe",
        SecretKind::StripeKey,
        r"[sr]k_live_[0-9A-Za-z]{20,}",
    ),
    (
        "telegram",
        SecretKind::TelegramBotToken,
        r"[0-9]{8,10}:[A-Za-z0-9_-]{35}",
    ),
    ("npm", SecretKind::NpmToken, r"npm_[A-Za-z0-9]{36}"),
    (
        "gitlab",
        SecretKind::GitlabToken,
        r"glpat-[A-Za-z0-9_-]{20,}",
    ),
    ("hf", SecretKind::HuggingFaceToken, r"hf_[A-Za-z]{34}"),
    ("groq", SecretKind::GroqKey, r"gsk_[A-Za-z0-9]{40,}"),
    ("do", SecretKind::DigitalOceanToken, r"dop_v1_[a-f0-9]{64}"),
];

fn kind_for_shape(id: &str) -> SecretKind {
    match id {
        "openai_style" => SecretKind::OpenAiStyleKey,
        "google_api" => SecretKind::GoogleApiKey,
        "github" => SecretKind::GithubToken,
        "slack" => SecretKind::SlackToken,
        "nvidia" => SecretKind::NvidiaKey,
        _ => SecretKind::Jwt,
    }
}

struct Shape {
    kind: Option<SecretKind>,
    id: &'static str,
    re: Regex,
}

fn shapes() -> &'static [Shape] {
    static CELL: OnceLock<Vec<Shape>> = OnceLock::new();
    CELL.get_or_init(|| {
        let shared = SHAPES.iter().map(|(id, p)| Shape {
            kind: None,
            id,
            re: Regex::new(p).expect("built-in pattern is valid"),
        });
        let extra = EXTRA_SHAPES.iter().map(|(id, kind, p)| Shape {
            kind: Some(*kind),
            id,
            re: Regex::new(p).expect("built-in pattern is valid"),
        });
        shared.chain(extra).collect()
    })
}

/// Find suspected secrets in `text`. Results are sorted by start and do not
/// overlap; a known shape wins over a labelled match on the same bytes.
///
/// Pure and local. Linear in the length of `text`.
#[must_use]
pub fn detect_secrets(text: &str) -> Vec<SecretCandidate> {
    let mut out: Vec<SecretCandidate> = Vec::new();
    detect_known_shapes(text, &mut out);
    detect_private_keys(text, &mut out);
    detect_cards(text, &mut out);
    let known = out.clone();
    for c in detect_labelled(text) {
        if !known.iter().any(|k| c.start < k.end && k.start < c.end) {
            out.push(c);
        }
    }
    out.sort_by_key(|c| (c.start, std::cmp::Reverse(c.end)));
    // Drop overlaps (keep the earliest / longest).
    let mut last_end = 0usize;
    out.retain(|c| {
        let keep = c.start >= last_end;
        if keep {
            last_end = c.end;
        }
        keep
    });
    out
}

/// `text` with every candidate's span replaced by [`crate::REDACTED`], for
/// callers that must not pass the value on (a message from a channel that
/// cannot show the offer). `candidates` must come from [`detect_secrets`] on
/// the same `text`.
#[must_use]
pub fn redact_candidates(text: &str, candidates: &[SecretCandidate]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for c in candidates {
        if c.start < at || c.end > text.len() {
            continue;
        }
        out.push_str(&text[at..c.start]);
        out.push_str(crate::REDACTED);
        at = c.end;
    }
    out.push_str(&text[at..]);
    out
}

/// An inbound message with its suspected secrets cut out; see [`screen_inbound`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenedMessage {
    /// The message with each secret replaced by [`crate::REDACTED`].
    pub text: String,
    /// What was found, each kind once, in order of appearance. Never the values.
    pub kinds: Vec<SecretKind>,
    /// Nothing but secrets was in the message: there is nothing left to pass on.
    pub only_secrets: bool,
}

/// The daemon-side second check for a channel that cannot show the owner the
/// "keep it out of the chat" card (Telegram, Discord): `None` when `text` has
/// no suspected secret, otherwise the message with the values cut out. The
/// caller passes on `text` (or nothing, when `only_secrets`), tells the owner
/// what happened and never logs the original.
#[must_use]
pub fn screen_inbound(text: &str) -> Option<ScreenedMessage> {
    let found = detect_secrets(text);
    if found.is_empty() {
        return None;
    }
    let mut kinds: Vec<SecretKind> = Vec::new();
    for c in &found {
        if !kinds.contains(&c.kind) {
            kinds.push(c.kind);
        }
    }
    let redacted = redact_candidates(text, &found);
    let only_secrets = !redacted
        .replace(crate::REDACTED, "")
        .chars()
        .any(char::is_alphanumeric);
    Some(ScreenedMessage {
        text: redacted,
        kinds,
        only_secrets,
    })
}

fn prev_is_alnum(text: &str, start: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
}

fn next_is_alnum(text: &str, end: usize) -> bool {
    text[end..]
        .chars()
        .next()
        .is_some_and(char::is_alphanumeric)
}

fn detect_known_shapes(text: &str, out: &mut Vec<SecretCandidate>) {
    for shape in shapes() {
        for m in shape.re.find_iter(text) {
            // A shape must start and end at a word edge: `task-...` is not
            // `sk-...`, and a longer alphanumeric run is not a key id.
            if prev_is_alnum(text, m.start()) || next_is_alnum(text, m.end()) {
                continue;
            }
            let value = m.as_str();
            if shape.id == "openai_style" && !value.bytes().any(|b| b.is_ascii_digit()) {
                // `sk-learn-is-a-fine-library` has no digit; real keys do.
                continue;
            }
            if shape.id == "telegram" && !value.contains(|c: char| c.is_ascii_uppercase()) {
                continue;
            }
            let kind = shape.kind.unwrap_or_else(|| kind_for_shape(shape.id));
            out.push(SecretCandidate {
                kind,
                confidence: Confidence::High,
                start: m.start(),
                end: m.end(),
            });
        }
    }
}

fn detect_private_keys(text: &str, out: &mut Vec<SecretCandidate>) {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("built-in pattern is valid")
    });
    for m in header.find_iter(text) {
        let end = text[m.end()..]
            .find("PRIVATE KEY-----")
            .map_or(text.len(), |i| m.end() + i + "PRIVATE KEY-----".len());
        out.push(SecretCandidate {
            kind: SecretKind::PrivateKey,
            confidence: Confidence::High,
            start: m.start(),
            end,
        });
    }
}

// ----------------------------------------------------------------- cards ---

/// Words (folded) that make a contiguous digit run a card number.
const CARD_LABELS: &[&str] = &[
    "card",
    "cc",
    "ccn",
    "visa",
    "mastercard",
    "amex",
    "kart",
    "kredi",
    "banka",
];

/// Digit group sizes people write a card number in.
const CARD_LAYOUTS: &[&[usize]] = &[&[4, 4, 4, 4], &[4, 4, 4, 4, 3], &[4, 6, 5], &[4, 6, 4]];

fn luhn_ok(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// A real card network prefix with the matching length (Visa, Mastercard,
/// Amex, Discover, Diners, JCB, `UnionPay`, Troy).
fn card_network_ok(d: &[u8]) -> bool {
    let n = d.len();
    let p = |k: usize| d[..k].iter().fold(0u32, |a, &x| a * 10 + u32::from(x));
    match d[0] {
        4 => matches!(n, 13 | 16 | 19),
        5 => n == 16 && (51..=55).contains(&p(2)),
        2 => n == 16 && (2221..=2720).contains(&p(4)),
        3 => match p(2) {
            34 | 37 => n == 15,
            36 | 38 | 39 => (14..=16).contains(&n),
            30 => n == 14 && (300..=305).contains(&p(3)),
            35 => (16..=19).contains(&n) && (3528..=3589).contains(&p(4)),
            _ => false,
        },
        6 => {
            (16..=19).contains(&n)
                && (p(4) == 6011 || p(2) == 65 || (644..=649).contains(&p(3)) || p(2) == 62)
        }
        9 => n == 16 && p(4) == 9792,
        _ => false,
    }
}

/// True when one of the last few words before `start` is a card label.
fn card_label_before(text: &str, start: usize) -> bool {
    let tail: String = text[..start]
        .chars()
        .rev()
        .take(32)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(fold)
        .collect();
    tail.split(|c: char| !c.is_ascii_alphanumeric())
        .rev()
        .filter(|w| !w.is_empty())
        .take(4)
        .any(|w| CARD_LABELS.contains(&w) || w.starts_with("kart") || w.starts_with("kredi"))
}

fn detect_cards(text: &str, out: &mut Vec<SecretCandidate>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // A maximal chain of digit groups joined by one space or dash.
        let start = i;
        let mut groups: Vec<usize> = Vec::new();
        let mut sep: Option<u8> = None;
        let mut mixed = false;
        let end = loop {
            let g0 = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            groups.push(i - g0);
            if i + 1 < bytes.len()
                && matches!(bytes[i], b' ' | b'-')
                && bytes[i + 1].is_ascii_digit()
            {
                if sep.is_some_and(|s| s != bytes[i]) {
                    mixed = true;
                }
                sep = Some(bytes[i]);
                i += 1;
            } else {
                break i;
            }
        };
        if mixed || prev_blocks_card(text, start) || next_blocks_card(text, end) {
            continue;
        }
        let digits: Vec<u8> = bytes[start..end]
            .iter()
            .filter(|b| b.is_ascii_digit())
            .map(|b| b - b'0')
            .collect();
        if !(13..=19).contains(&digits.len()) {
            continue;
        }
        let layout_ok = if groups.len() == 1 {
            card_label_before(text, start)
        } else {
            CARD_LAYOUTS.contains(&groups.as_slice())
        };
        let same = digits.iter().all(|&d| d == digits[0]);
        if layout_ok && !same && card_network_ok(&digits) && luhn_ok(&digits) {
            out.push(SecretCandidate {
                kind: SecretKind::PaymentCard,
                confidence: Confidence::Medium,
                start,
                end,
            });
        }
    }
}

/// A digit run glued to a letter, `+`, `#` or a decimal point is a phone
/// number, an id or an amount, not a card.
fn prev_blocks_card(text: &str, start: usize) -> bool {
    let mut back = text[..start].chars().rev();
    match back.next() {
        Some(c) if c.is_alphanumeric() || matches!(c, '+' | '#' | '_') => true,
        Some('.' | ',') => back.next().is_some_and(|c| c.is_ascii_digit()),
        _ => false,
    }
}

fn next_blocks_card(text: &str, end: usize) -> bool {
    let mut fwd = text[end..].chars();
    match fwd.next() {
        Some(c) if c.is_alphanumeric() || c == '_' => true,
        Some('.' | ',') => fwd.next().is_some_and(|c| c.is_ascii_digit()),
        _ => false,
    }
}

// ---------------------------------------------------------------- labels ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum LabelClass {
    /// Random-looking machine value (api key, token, secret).
    Machine(SecretKind),
    /// Human-chosen password.
    Password,
}

/// Fold one char to lowercase ASCII where Turkish letters have an ASCII twin,
/// so `ŞİFRE`, `Şifre` and `sifre` are the same word.
fn fold(c: char) -> char {
    match c {
        'ş' | 'Ş' => 's',
        'ğ' | 'Ğ' => 'g',
        'ü' | 'Ü' => 'u',
        'ö' | 'Ö' => 'o',
        'ç' | 'Ç' => 'c',
        'ı' | 'İ' | 'I' => 'i',
        _ => c.to_ascii_lowercase(),
    }
}

/// Classify a folded word as a label (full-word forms plus the Turkish
/// possessive / case suffixes of the stem).
fn label_word(word: &str) -> Option<LabelClass> {
    use LabelClass::{Machine, Password};
    let turkish_stem = |stem: &str| {
        word.strip_prefix(stem)
            .is_some_and(|rest| rest.len() <= 4 && rest.bytes().all(|b| b.is_ascii_lowercase()))
    };
    match word {
        "token" | "bearer" | "jeton" => return Some(Machine(SecretKind::LabelledToken)),
        "secret" => return Some(Machine(SecretKind::LabelledSecret)),
        "apikey" => return Some(Machine(SecretKind::LabelledApiKey)),
        "password" | "passwd" | "pwd" | "passphrase" => return Some(Password),
        _ => {}
    }
    if turkish_stem("sifre") || turkish_stem("parola") {
        return Some(Password);
    }
    if turkish_stem("anahtar") {
        return Some(Machine(SecretKind::LabelledApiKey));
    }
    if turkish_stem("jeton") {
        return Some(Machine(SecretKind::LabelledToken));
    }
    None
}

/// Words that may sit between a label and its value ("password is X").
fn is_glue_word(word: &str) -> bool {
    matches!(
        word,
        "is" | "was" | "are" | "ise" | "su" | "budur" | "olarak" | "sunu" | "key" | "anahtar"
    )
}

/// Words that, before `key`/`anahtar`, make a two-word label.
fn is_label_prefix(word: &str) -> Option<LabelClass> {
    match word {
        "api" | "access" | "erisim" | "gizli" | "secret" | "private" | "client" => {
            Some(LabelClass::Machine(match word {
                "api" | "access" | "erisim" => SecretKind::LabelledApiKey,
                _ => SecretKind::LabelledSecret,
            }))
        }
        _ => None,
    }
}

struct Word {
    /// Index of the first char in the char array.
    first: usize,
    /// One past the last char index.
    last: usize,
    folded: String,
}

const MAX_GAP_CHARS: usize = 24;

fn detect_labelled(text: &str) -> Vec<SecretCandidate> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let folded: Vec<char> = chars.iter().map(|&(_, c)| fold(c)).collect();

    // Words: runs of alphanumerics (underscore, dash and dot separate words,
    // so `OPENAI_API_KEY` is `openai`, `api`, `key`).
    let mut words: Vec<Word> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].1.is_alphanumeric() {
            let first = i;
            while i < chars.len() && chars[i].1.is_alphanumeric() {
                i += 1;
            }
            words.push(Word {
                first,
                last: i,
                folded: folded[first..i].iter().collect(),
            });
        } else {
            i += 1;
        }
    }

    let mut out = Vec::new();
    let mut wi = 0;
    while wi < words.len() {
        let w = &words[wi];
        let mut class = label_word(&w.folded);
        let mut label_last = w.last;
        let mut consumed = 1;
        // Two-word labels: "api key", "secret key", "gizli anahtar", ...
        if let (Some(prefix), Some(next)) = (is_label_prefix(&w.folded), words.get(wi + 1)) {
            let contiguous = chars[w.last..next.first]
                .iter()
                .all(|&(_, c)| matches!(c, ' ' | '_' | '-'));
            let key_word =
                next.folded == "key" || next.folded == "keys" || next.folded.starts_with("anahtar");
            if contiguous && key_word {
                class = Some(prefix);
                label_last = next.last;
                consumed = 2;
            } else if contiguous && label_word(&next.folded).is_some() {
                // "api token", "client secret": the second word decides.
                wi += 1;
                continue;
            }
        }
        let Some(class) = class else {
            wi += consumed;
            continue;
        };
        // A placeholder `[secret: name]` is our own output, never a candidate.
        if w.first > 0 && chars[w.first - 1].1 == '[' {
            wi += consumed;
            continue;
        }
        if let Some(c) = value_after_label(text, &chars, &words, wi + consumed, label_last, class) {
            out.push(c);
        }
        wi += consumed;
    }
    out
}

/// Parse the value following a label ending at char index `label_last`.
fn value_after_label(
    text: &str,
    chars: &[(usize, char)],
    words: &[Word],
    next_word: usize,
    label_last: usize,
    class: LabelClass,
) -> Option<SecretCandidate> {
    let is_gap = |c: char| matches!(c, ' ' | '\t' | ':' | '=' | '"' | '\'' | '`' | '>');
    let mut p = label_last;
    let mut nw = next_word;
    let gap_start = p;
    loop {
        while p < chars.len() && is_gap(chars[p].1) {
            p += 1;
        }
        // Skip one glue word ("is", "olarak", "şu"...), then more gap chars.
        if let Some(w) = words.get(nw)
            && w.first == p
            && is_glue_word(&w.folded)
        {
            p = w.last;
            nw += 1;
            continue;
        }
        break;
    }
    if p == gap_start || p - gap_start > MAX_GAP_CHARS || p >= chars.len() {
        return None;
    }
    let start_char = p;
    let value_end = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '`'
                    | ','
                    | ';'
                    | '<'
                    | '>'
                    | ')'
                    | ']'
                    | '}'
                    | '«'
                    | '»'
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
            )
    };
    while p < chars.len() && !value_end(chars[p].1) {
        p += 1;
    }
    // Trailing sentence punctuation is not part of the value.
    while p > start_char && matches!(chars[p - 1].1, '.' | ':' | '?') {
        p -= 1;
    }
    if p <= start_char {
        return None;
    }
    let start = chars[start_char].0;
    let end = chars.get(p).map_or(text.len(), |&(b, _)| b);
    let value = &text[start..end];
    let (ok, kind) = match class {
        LabelClass::Machine(kind) => (looks_like_machine_value(value), kind),
        LabelClass::Password => (looks_like_password(value), SecretKind::LabelledPassword),
    };
    ok.then_some(SecretCandidate {
        kind,
        confidence: Confidence::Medium,
        start,
        end,
    })
}

/// Shannon entropy in bits per char.
fn entropy(value: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let mut n = 0.0_f64;
    for c in value.chars() {
        *counts.entry(c).or_insert(0.0_f64) += 1.0;
        n += 1.0;
    }
    if n == 0.0 {
        return 0.0;
    }
    -counts
        .values()
        .map(|&k| {
            let p = k / n;
            p * p.log2()
        })
        .sum::<f64>()
}

fn looks_like_url_or_path(value: &str) -> bool {
    value.contains("://")
        || value.starts_with(['/', '~', '\\', '.'])
        || value.matches('/').count() >= 2
        || value.matches('\\').count() >= 1
        || value.get(1..3) == Some(":\\")
        || value.get(1..3) == Some(":/")
}

/// Number of letter/digit class changes along the alphanumerics of `value`.
fn class_changes(value: &str) -> usize {
    let mut last: Option<bool> = None;
    let mut n = 0;
    for c in value.chars().filter(char::is_ascii_alphanumeric) {
        let digit = c.is_ascii_digit();
        if last.is_some_and(|l| l != digit) {
            n += 1;
        }
        last = Some(digit);
    }
    n
}

fn looks_like_machine_value(value: &str) -> bool {
    let len = value.chars().count();
    if !(16..=512).contains(&len) {
        return false;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=' | '~'))
    {
        return false;
    }
    if looks_like_url_or_path(value) {
        return false;
    }
    let has_digit = value.bytes().any(|b| b.is_ascii_digit());
    let has_lower = value.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = value.bytes().any(|b| b.is_ascii_uppercase());
    if !has_digit || !(has_lower || has_upper) {
        return false;
    }
    // A version string ("1.2.3-beta4") is dots and digits around a few words.
    if value.split(['.', '-']).all(|s| s.len() <= 8) && value.matches('.').count() >= 2 {
        return false;
    }
    let distinct = value
        .chars()
        .collect::<std::collections::HashSet<_>>()
        .len();
    if distinct < 8 || entropy(value) < 3.0 {
        return false;
    }
    // Words with a number glued on ("my-long-token-name-2024") are text,
    // not keys: a key mixes letters and digits, or mixes upper and lower case.
    (has_lower && has_upper) || class_changes(value) >= 3
}

fn looks_like_password(value: &str) -> bool {
    let len = value.chars().count();
    if !(8..=128).contains(&len) {
        return false;
    }
    if value.starts_with(['<', '[', '{', '$', '%', '*', '(']) || value.contains("://") {
        return false;
    }
    if looks_like_url_or_path(value) && !value.contains(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let has_lower = value.chars().any(char::is_lowercase);
    let has_upper = value.chars().any(char::is_uppercase);
    let has_digit = value.chars().any(|c| c.is_ascii_digit());
    let has_symbol = value.chars().any(|c| !c.is_alphanumeric());
    // `.`, `_` and `-` alone do not make a password (`getpass.getpass`).
    let has_strong_symbol = value
        .chars()
        .any(|c| !c.is_alphanumeric() && !matches!(c, '.' | '_' | '-'));
    let letters = has_lower || has_upper;
    let classes = [has_lower, has_upper, has_digit, has_symbol]
        .iter()
        .filter(|&&b| b)
        .count();
    let distinct = value
        .chars()
        .collect::<std::collections::HashSet<_>>()
        .len();
    letters
        && (has_digit || has_strong_symbol)
        && !value.contains('(')
        && classes >= 2
        && distinct >= 5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_contain_the_value() {
        let text = "key sk-abcdefghijklmnop1234567890 end";
        let found = detect_secrets(text);
        assert_eq!(found.len(), 1);
        let c = found[0];
        assert_eq!(c.slice(text), Some("sk-abcdefghijklmnop1234567890"));
        for rendered in [format!("{c:?}"), format!("{c}")] {
            assert!(!rendered.contains("abcdefghijklmnop"), "{rendered}");
        }
    }

    #[test]
    fn placeholder_uses_the_secret_name_rules() {
        assert_eq!(
            placeholder("login/openai").as_deref(),
            Some("[secret: login/openai]")
        );
        assert_eq!(placeholder("bad name]"), None);
        assert_eq!(placeholder(""), None);
    }

    #[test]
    fn own_placeholder_is_not_a_candidate() {
        let found = detect_secrets("use [secret: login/openai2024x] here");
        assert_eq!(found, Vec::<SecretCandidate>::new());
    }

    #[test]
    fn known_shape_wins_over_label_on_same_bytes() {
        let text = "api key: sk-abcdefghijklmnop1234567890";
        let found = detect_secrets(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].confidence, Confidence::High);
    }
}
