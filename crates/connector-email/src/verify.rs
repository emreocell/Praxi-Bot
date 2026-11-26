//! Verification mail helpers: one-time codes, links and the rule that decides
//! which link a bot may follow.
//!
//! Everything here is a pure function over untrusted text. The rules are fixed
//! in code: a link is "followable" only when it is `https`, carries no user
//! info, is not an IP literal, and its host is on the same registrable domain
//! as the site the sign-up was started on (a domain the daemon verified against
//! the live browser, see the `email_wait_for` tool) or on an explicit list the
//! owner set. Everything else is reported by host name only, as "external
//! link, not followed", so the model never receives a URL it could paste into
//! a browser by reflex.

use std::collections::BTreeSet;
use std::net::IpAddr;

/// Most links read from one message.
pub const MAX_LINKS_SCANNED: usize = 60;
/// Longest URL kept.
pub const MAX_URL_CHARS: usize = 2048;
/// Most codes returned for one message.
pub const MAX_CODES: usize = 5;
/// Most followable links returned for one message.
pub const MAX_FOLLOWABLE: usize = 5;
/// Most external hosts listed for one message.
pub const MAX_EXTERNAL_HOSTS: usize = 8;

/// Public suffixes with more than one label (and hosting platforms where every
/// customer has a sub-domain of their own, so "same domain" would mean
/// nothing). Deliberately short and conservative: a host that is not
/// recognised here is judged by its last two labels, which can only make the
/// "same site" test stricter for the platforms listed and never looser for the
/// ones not listed than the plain two-label rule.
const MULTI_LABEL_SUFFIXES: &[&str] = &[
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "me.uk",
    "ltd.uk",
    "plc.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "gov.au",
    "co.nz",
    "org.nz",
    "net.nz",
    "co.jp",
    "ne.jp",
    "or.jp",
    "ac.jp",
    "co.kr",
    "or.kr",
    "com.br",
    "net.br",
    "org.br",
    "com.tr",
    "net.tr",
    "org.tr",
    "gen.tr",
    "com.cn",
    "net.cn",
    "org.cn",
    "com.hk",
    "com.tw",
    "com.sg",
    "com.mx",
    "com.ar",
    "com.co",
    "co.in",
    "net.in",
    "org.in",
    "co.za",
    "org.za",
    "com.ua",
    "com.pl",
    "co.il",
    "co.id",
    "com.my",
    "com.ph",
    "com.vn",
    "com.sa",
    "com.eg",
    "com.pk",
    "com.ng",
    "github.io",
    "gitlab.io",
    "pages.dev",
    "workers.dev",
    "netlify.app",
    "vercel.app",
    "herokuapp.com",
    "blogspot.com",
    "wordpress.com",
    "azurewebsites.net",
    "web.app",
    "firebaseapp.com",
    "appspot.com",
    "cloudfront.net",
    "s3.amazonaws.com",
    "myshopify.com",
    "wixsite.com",
    "fly.dev",
    "onrender.com",
    "ngrok.io",
    "ngrok-free.app",
    "trycloudflare.com",
    "repl.co",
    "glitch.me",
    "surge.sh",
    "bubbleapps.io",
];

/// The registrable domain of `host` (`accounts.example.co.uk` gives
/// `example.co.uk`), lower case. `None` for an IP literal, a single label, or a
/// host that is itself a public suffix.
#[must_use]
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    let suffix_labels = MULTI_LABEL_SUFFIXES
        .iter()
        .filter(|s| host == **s || host.ends_with(&format!(".{s}")))
        .map(|s| s.split('.').count())
        .max()
        .unwrap_or(1);
    if labels.len() <= suffix_labels {
        return None;
    }
    Some(labels[labels.len() - suffix_labels - 1..].join("."))
}

/// The registrable domain of the host of an http(s) `origin` or URL.
#[must_use]
pub fn site_domain(origin_or_url: &str) -> Option<String> {
    let url = url::Url::parse(origin_or_url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    registrable_domain(url.host_str()?)
}

/// An owner allow-list entry, normalised: a host name or domain with at least
/// two labels, lower case, no scheme, path or wildcard.
///
/// # Errors
/// A short sentence when the entry cannot be a domain.
pub fn normalize_allowed_domain(entry: &str) -> Result<String, String> {
    let e = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    let bad = || format!("{entry:?} is not a domain name (write it like example.com)");
    if e.is_empty() || e.len() > 253 {
        return Err(bad());
    }
    let labels: Vec<&str> = e.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|l| {
            l.is_empty()
                || l.len() > 63
                || l.starts_with('-')
                || l.ends_with('-')
                || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || e.parse::<IpAddr>().is_ok()
    {
        return Err(bad());
    }
    Ok(e)
}

/// What one link is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkVerdict {
    /// https, same site (or owner-listed): the bot may open exactly this URL.
    Followable(String),
    /// A different site: reported by host only, never followed.
    External(String),
    /// Refused outright (plain http, user info, IP literal): host and reason.
    Refused(String, &'static str),
    /// Not a web link at all (`mailto:`, `javascript:`, unparseable, too long).
    Ignored,
}

fn host_listed(host: &str, allow: &[String]) -> bool {
    allow
        .iter()
        .any(|a| host == a || host.ends_with(&format!(".{a}")))
}

/// Judge one raw link. `site` is the verified registrable domain of the page
/// the sign-up started on (`None`: not verified, so no link is followable
/// unless the owner listed it); `allow` the owner's normalised allow-list.
#[must_use]
pub fn classify_link(raw: &str, site: Option<&str>, allow: &[String]) -> LinkVerdict {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_CHARS {
        return LinkVerdict::Ignored;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return LinkVerdict::Ignored;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return LinkVerdict::Ignored;
    }
    let Some(host) = url
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
    else {
        return LinkVerdict::Ignored;
    };
    if url.scheme() != "https" {
        return LinkVerdict::Refused(host, "plain http is never followed");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return LinkVerdict::Refused(host, "a link with a user name or password in it");
    }
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return LinkVerdict::Refused(host, "a link to a bare IP address");
    }
    let same_site = registrable_domain(&host)
        .zip(site)
        .is_some_and(|(link, site)| link == site);
    if (same_site || host_listed(&host, allow)) && url.port_or_known_default() == Some(443) {
        return LinkVerdict::Followable(url.to_string());
    }
    LinkVerdict::External(host)
}

/// Click-tracking hosts of e-mail service providers (suffixes). A sign-up mail
/// sent through one wraps the real verification link in a redirect on such a
/// host, so the link's own host says nothing about the site.
const TRACKING_SUFFIXES: &[&str] = &[
    "sendgrid.net",
    "mandrillapp.com",
    "list-manage.com",
    "mailchimp.com",
    "mcsv.net",
    "mailgun.org",
    "mailgun.net",
    "sparkpostmail.com",
    "sparkpostmail1.com",
    "customeriomail.com",
    "mlsend.com",
    "sendibt2.com",
    "sendibt3.com",
    "brevo.com",
    "awstrack.me",
    "hubspotlinks.com",
    "klaviyomail.com",
    "convertkit-mail.com",
    "convertkit-mail2.com",
    "pstmrk.it",
    "iterable.com",
    "sailthru.com",
    "rs6.net",
];

/// Most tracking links looked through for one message.
pub const MAX_TRACKING_LOOKUPS: usize = 3;

/// Whether `host` is a known click-tracking host.
#[must_use]
pub fn is_tracking_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    TRACKING_SUFFIXES
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// The links of a message that are click-tracking redirects of a known provider
/// (https only, no user info) and would otherwise be reported as external.
#[must_use]
pub fn tracking_links(links: &[String], site: Option<&str>, allow: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in links.iter().take(MAX_LINKS_SCANNED) {
        let LinkVerdict::External(host) = classify_link(raw, site, allow) else {
            continue;
        };
        if is_tracking_host(&host) && !out.contains(raw) {
            out.push(raw.trim().to_owned());
        }
        if out.len() >= MAX_TRACKING_LOOKUPS {
            break;
        }
    }
    out
}

/// What a message offers, judged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Verification {
    /// One-time codes found in the subject and body.
    pub codes: Vec<String>,
    /// Links the bot may open (https, same site or owner-listed).
    pub followable: Vec<String>,
    /// Hosts of links that are not followed (different site).
    pub external_hosts: Vec<String>,
    /// Hosts of links refused outright, with the reason.
    pub refused: Vec<(String, &'static str)>,
}

/// Judge the links and find the codes of one message.
#[must_use]
pub fn analyse(
    subject: &str,
    body: &str,
    links: &[String],
    site: Option<&str>,
    allow: &[String],
) -> Verification {
    let mut out = Verification {
        codes: extract_codes(&format!("{subject}\n{body}")),
        ..Verification::default()
    };
    let mut seen = BTreeSet::new();
    let mut ext = BTreeSet::new();
    let mut refused = BTreeSet::new();
    for raw in links.iter().take(MAX_LINKS_SCANNED) {
        match classify_link(raw, site, allow) {
            LinkVerdict::Followable(u) => {
                if seen.insert(u.clone()) {
                    out.followable.push(u);
                }
            }
            LinkVerdict::External(h) => {
                ext.insert(h);
            }
            LinkVerdict::Refused(h, why) => {
                refused.insert((h, why));
            }
            LinkVerdict::Ignored => {}
        }
    }
    // The link that looks like the verification step first.
    out.followable
        .sort_by_key(|u| std::cmp::Reverse(looks_like_verification(u)));
    out.followable.truncate(MAX_FOLLOWABLE);
    out.external_hosts = ext.into_iter().take(MAX_EXTERNAL_HOSTS).collect();
    out.refused = refused.into_iter().take(MAX_EXTERNAL_HOSTS).collect();
    out
}

fn looks_like_verification(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    [
        "verify",
        "confirm",
        "activate",
        "validation",
        "token",
        "dogrula",
    ]
    .iter()
    .any(|k| u.contains(k))
}

/// Words that introduce a code, in the languages the app ships in.
const CODE_WORDS: &[&str] = &[
    "code",
    "otp",
    "passcode",
    "pin",
    "token",
    "kod",
    "kodu",
    "doğrulama",
    "dogrulama",
    "bestätigung",
    "bestaetigung",
    "código",
    "codigo",
    "verification",
    "verify",
];

fn is_code_token(t: &str) -> bool {
    let len = t.chars().count();
    (4..=10).contains(&len)
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !t.starts_with('-')
        && !t.ends_with('-')
}

/// One-time codes in `text`: a 4-10 character token that has a digit and
/// follows a code word ("your verification code is 482913", "kod: AB12CD"), or
/// a line that is nothing but 4-8 digits. At most [`MAX_CODES`], in order of
/// appearance. The result is untrusted text like the mail it came from.
#[must_use]
pub fn extract_codes(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut add = |c: &str| {
        let c = c
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
            .to_owned();
        if is_code_token(&c) && !found.contains(&c) && found.len() < MAX_CODES {
            found.push(c);
        }
    };
    for line in text.lines().take(400) {
        let line = line.trim();
        if (4..=8).contains(&line.len()) && line.bytes().all(|b| b.is_ascii_digit()) {
            add(line);
            continue;
        }
        let lower = line.to_lowercase();
        let mut from = 0usize;
        while let Some(at) = CODE_WORDS
            .iter()
            .filter_map(|w| lower[from..].find(w).map(|i| (from + i, w.len())))
            .min()
        {
            let (start, wlen) = at;
            let rest_start = start + wlen;
            // Back to the original-case text (byte offsets agree for ASCII
            // prefixes; for the rest keep to what `lower` cut at).
            let Some(rest) = line.get(rest_start..) else {
                break;
            };
            for tok in rest
                .split(|c: char| c.is_whitespace() || matches!(c, ':' | ',' | ';' | '(' | ')'))
                .filter(|t| !t.is_empty())
                .take(4)
            {
                let t = tok.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
                if is_code_token(t) && !t.chars().all(|c| c.is_ascii_alphabetic()) {
                    add(t);
                    break;
                }
            }
            from = rest_start;
            if from >= lower.len() {
                break;
            }
        }
    }
    found
}

fn trim_url(raw: &str) -> &str {
    raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '\'', '"'])
}

fn unescape_attr(v: &str) -> String {
    v.replace("&amp;", "&")
        .replace("&#38;", "&")
        .replace("&#x26;", "&")
        .replace("&#61;", "=")
        .replace("&#x3d;", "=")
        .replace("&#x3D;", "=")
}

/// Absolute http(s) URLs written out in plain text, in order, without
/// duplicates and capped. `mailto:` and other schemes are not links here.
#[must_use]
pub fn urls_in_text(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut from = 0usize;
    while out.len() < MAX_LINKS_SCANNED {
        let h = lower[from..].find("https://");
        let p = lower[from..].find("http://");
        let Some(rel) = h.into_iter().chain(p).min() else {
            break;
        };
        let start = from + rel;
        let end = text[start..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\' | '|')
            })
            .map_or(text.len(), |e| start + e);
        let url = trim_url(&text[start..end]);
        if url.len() > "https://".len() && !out.iter().any(|u| u == url) {
            out.push(url.to_owned());
        }
        from = end.max(start + 1);
        if from >= text.len() {
            break;
        }
    }
    out
}

/// The `href` values of an HTML body (`<a href="...">`), unescaped, absolute
/// http(s) only, without duplicates and capped. Image sources and scripts are
/// not looked at.
#[must_use]
pub fn hrefs_in_html(html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut from = 0usize;
    while out.len() < MAX_LINKS_SCANNED {
        let Some(rel) = lower[from..].find("href") else {
            break;
        };
        let mut i = from + rel + 4;
        from = i;
        let bytes = html.as_bytes();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let (value_start, end) = match bytes.get(i) {
            Some(q @ (b'"' | b'\'')) => {
                let s = i + 1;
                let e = html[s..].find(*q as char).map_or(html.len(), |e| s + e);
                (s, e)
            }
            Some(_) => {
                let e = html[i..]
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .map_or(html.len(), |e| i + e);
                (i, e)
            }
            None => break,
        };
        from = end.max(from);
        let value = unescape_attr(html[value_start..end].trim());
        let lv = value.to_ascii_lowercase();
        if (lv.starts_with("https://") || lv.starts_with("http://"))
            && value.len() <= MAX_URL_CHARS
            && !out.contains(&value)
        {
            out.push(value);
        }
    }
    out
}

/// Merge link lists, keeping order and dropping duplicates, within the cap.
#[must_use]
pub fn merge_links(lists: &[Vec<String>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in lists.iter().flatten() {
        if out.len() >= MAX_LINKS_SCANNED {
            break;
        }
        if !out.contains(u) {
            out.push(u.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domains() {
        assert_eq!(
            registrable_domain("accounts.example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            registrable_domain("a.b.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
        assert_eq!(registrable_domain("co.uk"), None);
        assert_eq!(registrable_domain("localhost"), None);
        assert_eq!(registrable_domain("10.0.0.1"), None);
        // A tenant of a hosting platform is its own site.
        assert_eq!(
            registrable_domain("alice.github.io").as_deref(),
            Some("alice.github.io")
        );
        assert_ne!(
            registrable_domain("alice.github.io"),
            registrable_domain("bob.github.io")
        );
    }

    #[test]
    fn only_https_same_site_links_are_followable() {
        let site = Some("example.com");
        let go = |u: &str| classify_link(u, site, &[]);
        assert!(matches!(
            go("https://accounts.example.com/verify?t=1"),
            LinkVerdict::Followable(_)
        ));
        assert!(matches!(
            go("http://example.com/verify"),
            LinkVerdict::Refused(_, _)
        ));
        assert!(matches!(
            go("https://example.com.evil.test/verify"),
            LinkVerdict::External(_)
        ));
        assert!(matches!(
            go("https://evilexample.com/verify"),
            LinkVerdict::External(_)
        ));
        assert!(matches!(
            go("https://user:pw@example.com/verify"),
            LinkVerdict::Refused(_, _)
        ));
        assert!(matches!(
            go("https://93.184.216.34/verify"),
            LinkVerdict::Refused(_, _)
        ));
        assert!(matches!(
            go("https://example.com:8443/verify"),
            LinkVerdict::External(_)
        ));
        assert_eq!(go("mailto:a@example.com"), LinkVerdict::Ignored);
        assert_eq!(go("javascript:alert(1)"), LinkVerdict::Ignored);
        // No verified site: nothing is followable on its own.
        assert!(matches!(
            classify_link("https://example.com/verify", None, &[]),
            LinkVerdict::External(_)
        ));
        // The owner's list is the only other way in.
        let allow = vec!["mailer.example.net".to_owned()];
        assert!(matches!(
            classify_link("https://click.mailer.example.net/x", None, &allow),
            LinkVerdict::Followable(_)
        ));
        assert!(matches!(
            classify_link("https://notmailer.example.net/x", None, &allow),
            LinkVerdict::External(_)
        ));
    }

    #[test]
    fn analyse_lists_external_links_by_host_only() {
        let links = vec![
            "https://example.com/confirm?token=abc".to_owned(),
            "https://tracker.evil.test/pixel?u=1&secret=2".to_owned(),
            "https://example.com/unsubscribe".to_owned(),
        ];
        let v = analyse("Welcome", "hi", &links, Some("example.com"), &[]);
        assert_eq!(v.followable[0], "https://example.com/confirm?token=abc");
        assert_eq!(v.external_hosts, vec!["tracker.evil.test".to_owned()]);
        assert!(
            !format!("{v:?}").contains("secret=2"),
            "an external URL is never returned whole"
        );
    }

    #[test]
    fn codes_after_a_code_word_or_alone_on_a_line() {
        assert_eq!(
            extract_codes("Your verification code is 482913. It expires soon."),
            vec!["482913".to_owned()]
        );
        assert_eq!(
            extract_codes("Doğrulama kodu: 77Ab12"),
            vec!["77Ab12".to_owned()]
        );
        assert_eq!(
            extract_codes("Hello\n\n  123456  \nbye"),
            vec!["123456".to_owned()]
        );
        // A year or a plain word after the word is not a code.
        assert_eq!(
            extract_codes("Use the code below to join in 2026 please").len(),
            0
        );
        assert_eq!(extract_codes("no digits here at all").len(), 0);
    }

    #[test]
    fn links_from_html_and_text() {
        let html = r#"<a class="x" href="https://example.com/verify?a=1&amp;b=2">Go</a>
            <img src="https://tracker.test/p.gif"><a href='http://example.com/x'>x</a>
            <a href="mailto:a@b.c">m</a>"#;
        let h = hrefs_in_html(html);
        assert_eq!(
            h,
            vec![
                "https://example.com/verify?a=1&b=2".to_owned(),
                "http://example.com/x".to_owned()
            ]
        );
        let t = urls_in_text("Open (https://example.com/v?t=1), or http://example.com/y.\nbye");
        assert_eq!(
            t,
            vec![
                "https://example.com/v?t=1".to_owned(),
                "http://example.com/y".to_owned()
            ]
        );
    }

    #[test]
    fn allow_list_entries_are_normalised() {
        assert_eq!(
            normalize_allowed_domain(" Mail.Example.COM. ").unwrap(),
            "mail.example.com"
        );
        for bad in [
            "",
            "localhost",
            "https://x.com",
            "a b.com",
            "1.2.3.4",
            "*.x.com",
            "x..com",
        ] {
            assert!(normalize_allowed_domain(bad).is_err(), "{bad}");
        }
    }
}
