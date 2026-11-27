//! What an owner-started Full ("Tam") run may do WITHOUT a card after it read a page, and the
//! checks that keep a run holding private data from leaking it.
//!
//! See `Shared::full_mode_waives`:
//!
//! 1. A run with no private data of the owner in it (files, mail, memory, screens, other bots)
//!    browses any public site, searches and reads the public web and fills any form without
//!    asking. Reading web pages alone never makes a run private.
//! 2. A run that holds private data AND read untrusted content trusts only the sites the OWNER
//!    named or approved (never a site the model navigated to). Anywhere else a call passes only
//!    when none of the private results' words ([`SeenText`]: salted hashes, in memory for the
//!    run, never the text) are in it, it has no data SHAPE ([`blob_like`]: keys, hashes,
//!    encoded strings, card numbers), it is small, and the run's running total sent to such
//!    places stays under a budget. Memory recall, knowledge-base hits and the replies of earlier
//!    turns are private data that CAN be hashed, so they only make a copied word or phrase ask;
//!    only private data whose text cannot be hashed (a signed-in browser, a window, an attached
//!    file, another bot, an MCP tool) makes every new site ask ([`SeenText::unhashed`]).
//!
//! Honest limit: an injected model that PARAPHRASES or re-encodes private data into a fresh,
//! differently shaped string, or private data that never passed through a tool result the run
//! saw (the model "knows" it), cannot be recognised here.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::sync::OnceLock;

use serde_json::Value;

/// Longest decoded path + query + fragment text of a URL to an untrusted site.
const URL_TOTAL_LIMIT: usize = 120;
/// Longest single path segment, query key or value (decoded) of such a URL.
const URL_PART_LIMIT: usize = 40;
/// What one run may send to ONE site the owner did not name, in decoded characters of URLs
/// (host included), search queries and form text together. Above it every further call to that
/// site asks, so a secret cannot leave in many small pieces. The scope is the site (the page's
/// host for text typed into it, the address's host for a URL, "web search" for queries) because
/// a task that signs up on many sites sends each of them a few hundred characters of its own
/// details: one run-wide total ran out after the third to fifth site of a research task and then
/// asked for every call after it. It bounds the VOLUME that a paraphrase could carry out to one
/// place, the words themselves are caught by [`SeenText`].
pub(crate) const SITE_SEND_BUDGET: usize = 1500;
/// What one run may send to all such sites together: four sites' worth. A run of the default
/// 40 steps cannot reach it by browsing one address per step, so it only bounds a long run (or a
/// raised step limit) that sprays small pieces over many sites the model chose.
pub(crate) const RUN_SEND_BUDGET: usize = 4 * SITE_SEND_BUDGET;
/// What ALL sessions of one mission may send to places the owner did not name together: three
/// runs' worth. It is kept with the mission (`missions::MissionStore::take_send_budget`), not in
/// a session, because every session starts with an empty [`SeenText`]: without it a mission of
/// many sessions would get a fresh run budget each time, and text the model paraphrased into the
/// ledger (which loses its hashes) could leave in pieces at the pace of one session after another.
/// Above it every further call that sends to such a place asks.
pub(crate) const MISSION_SEND_BUDGET: usize = 3 * RUN_SEND_BUDGET;
/// Text typed into a page that is not the owner's must be shorter than this: a field value or a
/// short profile description, not a paragraph.
pub(crate) const FOREIGN_TEXT_LIMIT: usize = 240;
/// Most distinct hashes kept for a run; more means "cannot compare", which asks.
const MAX_SEEN: usize = 400_000;
/// Most characters of one result read into the set.
const MAX_OBSERVED_CHARS: usize = 1_000_000;

/// More query pairs than this is a data channel, not a page address.
const MAX_QUERY_PAIRS: usize = 12;
/// More percent escapes than this in one URL is an encoded payload.
const MAX_PERCENT_ESCAPES: usize = 24;

/// A token of letters and digits that looks like a key, hash, id or encoded blob rather than a
/// word or a slug (`referanslarimiz-hakkimizda`, `2024-yilin-en-iyi-10-urunu`).
pub(crate) fn blob_like(token: &str) -> bool {
    let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let len = token.chars().count();
    if len < 16 {
        return false;
    }
    let digits = token.chars().filter(char::is_ascii_digit).count();
    let letters = token.chars().filter(char::is_ascii_alphabetic).count();
    let upper = token.chars().any(|c| c.is_ascii_uppercase());
    let lower = token.chars().any(|c| c.is_ascii_lowercase());
    // Hex digests and long numbers.
    if token.chars().all(|c| c.is_ascii_hexdigit()) {
        return digits > 0 || len >= 32;
    }
    let separators = token
        .chars()
        .filter(|c| matches!(c, '-' | '_' | '.' | '~'))
        .count();
    // Words and slugs are cut by separators often; a key or an encoded blob is not.
    let sparse = separators * 10 < len;
    if token.ends_with('=') && sparse {
        return true;
    }
    (len >= 20 && digits > 0 && letters > 0 && sparse)
        || (len >= 24 && upper && lower && sparse)
        || (len >= 40 && sparse)
}

/// Well-known secret prefixes (an API key, a bearer token, a private key block).
fn secret_prefixed(token: &str) -> bool {
    let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
    let lower = t.to_ascii_lowercase();
    (t.len() >= 12
        && [
            "sk-", "ghp_", "gho_", "xoxb-", "xoxp-", "akia", "eyj", "bearer",
        ]
        .iter()
        .any(|p| lower.starts_with(p)))
        || token.contains("-----BEGIN")
}

/// Digits with spaces or dashes between them that add up to a card or account number.
fn long_number(text: &str, joiners: &[char], min: usize) -> bool {
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= min {
                return true;
            }
        } else if !joiners.contains(&c) {
            run = 0;
        }
    }
    false
}

/// Typed or sent text that looks like a key, a token, an encoded blob or a card number.
pub(crate) fn text_carries_data(text: &str) -> bool {
    long_number(text, &[' ', '-'], 13)
        || text
            .split(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        ',' | ';' | ':' | '|' | '"' | '\'' | '/' | '?' | '&' | '='
                    )
            })
            .any(|w| blob_like(w) || secret_prefixed(w))
}

/// One decoded URL part (path segment, query key or value, fragment).
fn part_carries_data(part: &str) -> bool {
    part.chars().count() > URL_PART_LIMIT
        || part
            .split(|c: char| c.is_whitespace() || matches!(c, '/' | ',' | ';' | ':' | '|' | '+'))
            .any(|w| blob_like(w) || secret_prefixed(w))
}

/// `%XX` escapes and `+` decoded, so the judged text is what the site would read.
pub(crate) fn decoded(raw: &str) -> String {
    // The receiving server may decode more than once (`%2568` -> `%68` -> `h`).
    let mut current = decode(raw);
    for _ in 0..3 {
        let next = decode(&current);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// A `%XX` escape is left in `text`.
fn has_escape(text: &str) -> bool {
    let b = text.as_bytes();
    b.windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

/// `%XX` escapes and `+` decoded.
fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        let escaped = (bytes[i] == b'%')
            .then(|| {
                (
                    bytes.get(i + 1).and_then(|b| hex(*b)),
                    bytes.get(i + 2).and_then(|b| hex(*b)),
                )
            })
            .and_then(|pair| match pair {
                (Some(h), Some(l)) => u8::try_from(h * 16 + l).ok(),
                _ => None,
            });
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decoded characters of a URL that go to a server: host, path, query and fragment.
pub(crate) fn url_sent_chars(raw: &str) -> usize {
    url::Url::parse(raw.trim()).map_or(raw.chars().count(), |u| {
        u.host_str().map_or(0, |h| h.chars().count())
            + decode(u.path()).chars().count()
            + u.query().map_or(0, |q| decode(q).chars().count())
            + u.fragment().map_or(0, |f| decode(f).chars().count())
    })
}

/// A public `http(s)` address to a site the owner did not name whose host, path, query or
/// fragment look like they carry data. Fails closed: anything that does not parse carries.
pub(crate) fn url_carries_data(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw.trim()) else {
        return true;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return true;
    }
    let Some(host) = url.host_str() else {
        return true;
    };
    // A name made of data (`<text>.example.com`) is a channel too.
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() > 6 || labels.iter().any(|l| l.len() > 30 || blob_like(l)) {
        return true;
    }
    if raw.matches('%').count() > MAX_PERCENT_ESCAPES {
        return true;
    }
    // Escapes that survive decoding are a layered payload; a card number split by dashes,
    // dots or spaces is a number.
    let all = decoded(raw);
    if has_escape(&all) || long_number(&all, &[' ', '-', '.', '_'], 15) {
        return true;
    }
    let mut total = 0usize;
    if let Some(segments) = url.path_segments() {
        for segment in segments {
            let part = decode(segment);
            total += part.chars().count();
            if part_carries_data(&part) {
                return true;
            }
        }
    }
    let mut pairs = 0usize;
    for (k, v) in url.query_pairs() {
        pairs += 1;
        total += k.chars().count() + v.chars().count();
        if pairs > MAX_QUERY_PAIRS || part_carries_data(&k) || part_carries_data(&v) {
            return true;
        }
    }
    // A fragment never reaches the server but a page script reads it: not on an unknown site.
    if url.fragment().is_some_and(|f| !f.is_empty()) {
        return true;
    }
    total > URL_TOTAL_LIMIT
}

/// `text` with each of `addresses` (and its `%40` spelling in a URL) cut out, any letter case.
///
/// For the bot's OWN mail addresses: typing the address its sign-ups use into a form is the
/// bot's own business, not private text leaving, and a long address with digits in it looks like
/// a key to [`text_carries_data`]. Only the exact address is removed, never a longer token, so
/// nothing else can ride along inside it; what is left is judged as usual.
pub(crate) fn without_addresses(text: &str, addresses: &[String]) -> String {
    let mut out = text.to_owned();
    for address in addresses {
        let plain = address.trim().to_ascii_lowercase();
        if plain.is_empty() {
            continue;
        }
        for needle in [plain.replace('@', "%40"), plain] {
            while let Some(at) = out.to_ascii_lowercase().find(&needle) {
                out.replace_range(at..at + needle.len(), " ");
            }
        }
    }
    out
}

/// The URL-like strings (keys `url`, `uri`, `href`) and the free text of a browser call. Keys
/// of the call are the tool's own schema and are not judged.
pub(crate) fn split_input(input: &Value) -> (Vec<String>, Vec<String>) {
    fn walk(value: &Value, key: Option<&str>, urls: &mut Vec<String>, texts: &mut Vec<String>) {
        match value {
            Value::String(s) => {
                if key.is_some_and(|k| {
                    matches!(k.to_ascii_lowercase().as_str(), "url" | "uri" | "href")
                }) {
                    urls.push(s.clone());
                } else {
                    texts.push(s.clone());
                }
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, key, urls, texts)),
            Value::Object(map) => map
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "ref" | "dialog" | "action" | "op"))
                .for_each(|(k, v)| walk(v, Some(k), urls, texts)),
            _ => {}
        }
    }
    let (mut urls, mut texts) = (Vec::new(), Vec::new());
    walk(input, None, &mut urls, &mut texts);
    (urls, texts)
}

/// Lower-case alphanumeric words of `text`.
pub(crate) fn words(text: &str) -> Vec<String> {
    fold(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Compatibility-decomposed, accents stripped, lower case: composed and decomposed spellings
/// of the same word (Turkish, German) are one word on both the reading and the comparing side.
fn fold(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    text.nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn salt() -> &'static RandomState {
    static SALT: OnceLock<RandomState> = OnceLock::new();
    SALT.get_or_init(RandomState::new)
}

/// The three-word key of a window.
fn triple(w: &[String], i: usize) -> Option<String> {
    (i + 3 <= w.len()).then(|| format!("{} {} {}", w[i], w[i + 1], w[i + 2]))
}

/// What a run read, as salted hashes only (never the text), so a later call can be checked for
/// copied content. Lives in the run's [`crate::taint::ContentTaint`] and goes away with it.
#[derive(Debug, Clone, Default)]
pub(crate) struct SeenText {
    hashes: HashSet<u64>,
    /// Too much was read to compare: every comparison then says "copied".
    overflow: bool,
    /// Private data is in the run that was NOT hashed (memory without text, an earlier turn, a
    /// script's read): nothing can be compared, so the run asks.
    pub(crate) unhashed: bool,
    /// Decoded characters this run already sent to places the owner did not name, by site.
    sent: HashMap<String, usize>,
    /// Shell commands this run started without a card after reading untrusted content.
    pub(crate) shell_waived: usize,
}

impl SeenText {
    /// Every word of 6 or more characters and every run of three consecutive words.
    pub(crate) fn observe(&mut self, text: &str) {
        let text: String = text.chars().take(MAX_OBSERVED_CHARS).collect();
        let w = words(&text);
        for (i, word) in w.iter().enumerate() {
            if word.chars().count() >= 6 {
                self.insert(word);
            }
            if let Some(t) = triple(&w, i) {
                self.insert(&t);
            }
        }
    }

    fn insert(&mut self, token: &str) {
        if self.hashes.len() >= MAX_SEEN {
            self.overflow = true;
        } else {
            self.hashes.insert(salt().hash_one(token));
        }
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        self.hashes.extend(other.hashes.iter().copied());
        self.overflow |= other.overflow || self.hashes.len() > MAX_SEEN;
        self.unhashed |= other.unhashed;
        for (site, n) in &other.sent {
            let mine = self.sent.entry(site.clone()).or_default();
            *mine = (*mine).max(*n);
        }
        self.shell_waived = self.shell_waived.max(other.shell_waived);
    }

    /// Whether sending `spend` (characters per site, one entry per site) stays inside both the
    /// per-site budget and the run-wide one.
    pub(crate) fn within_send_budget(&self, spend: &[(String, usize)]) -> bool {
        let total: usize =
            self.sent.values().sum::<usize>() + spend.iter().map(|(_, n)| n).sum::<usize>();
        total <= RUN_SEND_BUDGET
            && spend
                .iter()
                .all(|(site, n)| self.sent.get(site).copied().unwrap_or(0) + n <= SITE_SEND_BUDGET)
    }

    /// Count `spend` against its sites (after [`Self::within_send_budget`] said yes).
    pub(crate) fn record_sent(&mut self, spend: &[(String, usize)]) {
        for (site, n) in spend {
            *self.sent.entry(site.clone()).or_default() += n;
        }
    }

    /// `text` holds a word (6+ characters) or a run of three words the run read. Words the owner
    /// typed themselves (`owner_words`) are not copied content.
    pub(crate) fn copied(&self, text: &str, owner_words: &HashSet<String>) -> bool {
        if self.overflow {
            return true;
        }
        let w = words(text);
        for (i, word) in w.iter().enumerate() {
            if word.chars().count() >= 6
                && !owner_words.contains(word)
                && self.hashes.contains(&salt().hash_one(word.as_str()))
            {
                return true;
            }
            if let Some(t) = triple(&w, i)
                && !w[i..i + 3].iter().all(|x| owner_words.contains(x))
                && self.hashes.contains(&salt().hash_one(t.as_str()))
            {
                return true;
            }
            // One private word split by separators (`hanne-lore`, `hanne.lore.example.com`,
            // `4111-1111-1111-1111`): two to four adjacent pieces joined.
            let mut joined = word.clone();
            for k in 1..4 {
                let Some(next) = w.get(i + k) else { break };
                joined.push_str(next);
                if joined.chars().count() >= 6
                    && !w[i..=i + k].iter().all(|x| owner_words.contains(x))
                    && self.hashes.contains(&salt().hash_one(joined.as_str()))
                {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ordinary_addresses_to_a_new_site_do_not_carry_data_shape() {
        for url in [
            "https://other.example/products/shoes",
            "https://other.example/products?page=3",
            "https://www.google.com/search?q=web+design+prices",
            "https://cdn.example.com/a.js?v=1700000000000",
            "https://other.example/%C3%BCr%C3%BCnler/%C5%9Fapka",
        ] {
            assert!(!url_carries_data(url), "{url}");
        }
    }

    #[test]
    fn data_shaped_addresses_carry() {
        for url in [
            "https://other.example/c2VjcmV0LXRva2VuLWZyb20tdGhlLXBhZ2UtYWJjZGVmZ2hpams",
            "https://other.example/?d=0123456789abcdef0123456789abcdef",
            "https://other.example/?q=sk-live-AbCdEfGhIjKlMnOp",
            "https://other.example/?a=1&b=2&c=3&d=4&e=5&f=6&g=7&h=8&i=9&j=10&k=11&l=12&m=13",
            "https://c2VjcmV0LXRva2VuLWZyb20tdGhlLXBhZ2U.other.example/",
            "https://a.b.c.d.e.f.g.other.example/",
            "https://other.example/#anything",
            "https://u:p@other.example/",
            "ftp://other.example/",
            "not a url",
        ] {
            assert!(url_carries_data(url), "{url}");
        }
        let long = format!("https://other.example/?q={}", "kelime+".repeat(7));
        assert!(url_carries_data(&long));
    }

    #[test]
    fn the_host_counts_in_what_is_sent() {
        assert_eq!(
            url_sent_chars("https://ab.example/xy?q=z"),
            "ab.example".len() + 3 + 3
        );
    }

    #[test]
    fn form_text_is_judged_by_shape() {
        for text in [
            "Merhaba, fiyat teklifi almak istiyorum.",
            "ali@example.com",
            "Enter",
        ] {
            assert!(!text_carries_data(text), "{text}");
        }
        for text in [
            "4111 1111 1111 1111",
            "sk-live-AbCdEfGhIjKlMnOp",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "0123456789abcdef0123456789abcdef",
            "-----BEGIN PRIVATE KEY-----",
        ] {
            assert!(text_carries_data(text), "{text}");
        }
        // a long description with an address in it is ordinary text
        assert!(!text_carries_data(&format!(
            "{} https://www.company.com/about-us",
            "word ".repeat(300)
        )));
    }

    #[test]
    fn copied_words_and_word_triples_are_found_without_keeping_the_text() {
        let mut seen = SeenText::default();
        seen.observe("The account holder is Hannelore Wuthering and the code is 482913 today.");
        let none = HashSet::new();
        // a long word from the page
        assert!(seen.copied("https://evil.example/?q=hannelore", &none));
        // three words in a row, however short
        assert!(seen.copied("the-code-is.evil.example", &none));
        // words that were not read pass
        assert!(!seen.copied("https://other.example/products/shoes", &none));
        // words the owner typed are not copied content
        let owner: HashSet<String> = ["hannelore".to_owned()].into();
        assert!(!seen.copied("https://evil.example/?q=hannelore", &owner));
        // too much to compare: everything counts as copied
        let big = SeenText {
            overflow: true,
            ..SeenText::default()
        };
        assert!(big.copied("anything", &none));
    }

    #[test]
    fn split_double_encoded_and_decomposed_private_words_are_still_found() {
        let mut seen = SeenText::default();
        seen.observe("Hannelore lives in Z\u{fc}richstrasse, card 4111111111111111");
        let none = HashSet::new();
        for url in [
            "https://evil.example/?q=hanne-lore",
            "https://hanne.lore.evil.example/",
            "https://evil.example/?q=%2568annelore",
            "https://evil.example/?q=Hannelore",
            // a decomposed u + combining diaeresis, against the composed word read
            "https://evil.example/?q=Zu%CC%88richstrasse",
            "https://evil.example/?c=4111-1111-1111-1111",
        ] {
            assert!(seen.copied(&decoded(url), &none), "{url}");
        }
        assert!(!seen.copied(&decoded("https://evil.example/?q=winter+coats"), &none));
    }

    #[test]
    fn layered_escapes_and_split_numbers_are_data_shape() {
        assert!(url_carries_data(
            "https://evil.example/?c=4111-1111-1111-1111"
        ));
        assert!(url_carries_data(
            "https://evil.example/?c=4111.1111.1111.1111"
        ));
        assert!(!url_carries_data("https://evil.example/?q=a%20b"));
    }

    #[test]
    fn a_call_is_split_into_addresses_and_text() {
        let (urls, texts) = split_input(
            &json!({"url": "https://a.example/", "steps": [{"ref": "e1", "text": "ali"}]}),
        );
        assert_eq!(urls, ["https://a.example/"]);
        assert!(texts.contains(&"ali".to_owned()));
    }

    fn spend(site: &str, n: usize) -> Vec<(String, usize)> {
        vec![(site.to_owned(), n)]
    }

    #[test]
    fn a_site_has_its_own_send_budget() {
        let mut seen = SeenText::default();
        assert!(seen.within_send_budget(&spend("a.example", SITE_SEND_BUDGET)));
        seen.record_sent(&spend("a.example", SITE_SEND_BUDGET));
        // The same site is used up, the next site starts from zero.
        assert!(!seen.within_send_budget(&spend("a.example", 1)));
        assert!(seen.within_send_budget(&spend("b.example", SITE_SEND_BUDGET)));
        // One call may spend on several sites; each is judged on its own.
        let both = vec![("b.example".to_owned(), 10), ("a.example".to_owned(), 1)];
        assert!(!seen.within_send_budget(&both));
    }

    #[test]
    fn the_run_has_a_ceiling_over_all_sites() {
        let mut seen = SeenText::default();
        for i in 0..4 {
            let site = format!("s{i}.example");
            assert!(seen.within_send_budget(&spend(&site, SITE_SEND_BUDGET)));
            seen.record_sent(&spend(&site, SITE_SEND_BUDGET));
        }
        assert!(!seen.within_send_budget(&spend("fresh.example", 1)));
    }

    #[test]
    fn a_resumed_run_keeps_the_larger_count_of_each_site() {
        let mut a = SeenText::default();
        a.record_sent(&spend("a.example", 100));
        let mut b = SeenText::default();
        b.record_sent(&spend("a.example", 700));
        b.record_sent(&spend("b.example", 5));
        a.merge(&b);
        assert!(a.within_send_budget(&spend("a.example", SITE_SEND_BUDGET - 700)));
        assert!(!a.within_send_budget(&spend("a.example", SITE_SEND_BUDGET - 699)));
        assert!(a.within_send_budget(&spend("b.example", SITE_SEND_BUDGET - 5)));
    }

    #[test]
    fn the_bots_own_address_is_cut_out_and_nothing_else() {
        let own = vec!["Praxi-8f3a9c1d2e4b@agentmail.to".to_owned()];
        // As typed, any letter case, and as a URL query value.
        assert!(text_carries_data("praxi-8f3a9c1d2e4b@agentmail.to"));
        assert!(!text_carries_data(&without_addresses(
            "praxi-8f3a9c1d2e4b@agentmail.to",
            &own
        )));
        assert!(!text_carries_data(&without_addresses(
            "PRAXI-8F3A9C1D2E4B@AGENTMAIL.TO",
            &own
        )));
        let url = without_addresses(
            "https://x.example/s?email=praxi-8f3a9c1d2e4b%40agentmail.to",
            &own,
        );
        assert!(!url_carries_data(&url), "{url}");
        // A longer token that merely contains it keeps its other characters; a key beside it stays one.
        let hidden = without_addresses(
            "praxi-8f3a9c1d2e4b@agentmail.to sk-live-AbCdEfGhIjKlMnOpQr",
            &own,
        );
        assert!(text_carries_data(&hidden), "{hidden}");
        // Someone else's address of the same shape is not cut.
        assert!(text_carries_data(&without_addresses(
            "praxi-111122223333@agentmail.to",
            &own
        )));
        // No address, no change.
        assert_eq!(without_addresses("hello", &[]), "hello");
        assert_eq!(without_addresses("hello", &[String::new()]), "hello");
    }
}
