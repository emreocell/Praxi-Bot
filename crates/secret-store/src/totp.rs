//! Time-based one-time passwords (RFC 6238 over the HOTP of RFC 4226) for the
//! two-factor codes of accounts a bot registered itself.
//!
//! A site that offers an authenticator app shows a setup secret: an
//! `otpauth://totp/...` URI (also the text behind its QR code) or a plain
//! base32 key. [`TotpSpec::parse`] accepts both. The spec is kept in the secret
//! store in one canonical `otpauth://` form ([`TotpSpec::canonical`]); the
//! current code is computed on demand ([`TotpSpec::code_at`]) and typed by the
//! fill tools without the model ever holding it.
//!
//! Nothing here prints the key: `Debug` is redacted and every error message is
//! fixed text.

use data_encoding::{BASE32_NOPAD, DecodeKind};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use url::Url;
use zeroize::Zeroizing;

use crate::fill::FillError;

/// Shortest accepted key (80 bits: what 16 base32 characters carry).
const MIN_KEY_BYTES: usize = 10;
/// Longest accepted key.
const MAX_KEY_BYTES: usize = 128;
/// Longest accepted setup text.
const MAX_INPUT_BYTES: usize = 2048;
/// Allowed range of a code period in seconds.
const PERIOD_RANGE: std::ops::RangeInclusive<u64> = 1..=300;
/// Allowed range of the number of digits.
const DIGITS_RANGE: std::ops::RangeInclusive<u32> = 6..=8;
/// Period when the setup text names none.
pub const DEFAULT_PERIOD_SECS: u64 = 30;
/// Digits when the setup text names none.
pub const DEFAULT_DIGITS: u32 = 6;

/// The HMAC hash of a TOTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotpAlgorithm {
    /// HMAC-SHA1: what authenticator apps use unless told otherwise.
    Sha1,
    /// HMAC-SHA256.
    Sha256,
    /// HMAC-SHA512.
    Sha512,
}

impl TotpAlgorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha512 => "SHA512",
        }
    }

    fn from_name(raw: &str) -> Option<Self> {
        match raw.to_ascii_uppercase().as_str() {
            "SHA1" => Some(Self::Sha1),
            "SHA256" => Some(Self::Sha256),
            "SHA512" => Some(Self::Sha512),
            _ => None,
        }
    }
}

/// A parsed two-factor setup: the key and how a code is made from it.
#[derive(Clone)]
pub struct TotpSpec {
    key: Zeroizing<Vec<u8>>,
    algorithm: TotpAlgorithm,
    digits: u32,
    period: u64,
}

impl std::fmt::Debug for TotpSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TotpSpec(<redacted>)")
    }
}

fn bad(why: &str) -> FillError {
    FillError::Invalid(format!("the two-factor setup secret is not usable: {why}"))
}

/// Decode a base32 key the way sites print it: spaces and dashes in groups,
/// any case, optional `=` padding.
fn decode_base32(raw: &str) -> Result<Vec<u8>, FillError> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '='))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    BASE32_NOPAD
        .decode(cleaned.as_bytes())
        .map_err(|e| match e.kind {
            DecodeKind::Symbol => bad("the key has a character that is not base32"),
            _ => bad("the key is not valid base32"),
        })
}

impl TotpSpec {
    /// Build a spec from raw parts (RFC vectors, tests).
    ///
    /// # Errors
    /// A key outside 10 to 128 bytes, or digits / period out of range.
    pub fn new(
        key: &[u8],
        algorithm: TotpAlgorithm,
        digits: u32,
        period: u64,
    ) -> Result<Self, FillError> {
        if !(MIN_KEY_BYTES..=MAX_KEY_BYTES).contains(&key.len()) {
            return Err(bad("the key is too short or too long"));
        }
        if !DIGITS_RANGE.contains(&digits) {
            return Err(bad("digits must be 6 to 8"));
        }
        if !PERIOD_RANGE.contains(&period) {
            return Err(bad("the period must be 1 to 300 seconds"));
        }
        Ok(Self {
            key: Zeroizing::new(key.to_vec()),
            algorithm,
            digits,
            period,
        })
    }

    /// Parse what a site shows: an `otpauth://totp/...` URI or a base32 key.
    /// An `otpauth://hotp/` (counter based) URI is refused: it needs state the
    /// site and the bot would have to keep in step.
    ///
    /// # Errors
    /// [`FillError::Invalid`] with a fixed message; the text is never echoed.
    pub fn parse(text: &str) -> Result<Self, FillError> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_INPUT_BYTES {
            return Err(bad("it must be 1 to 2048 characters"));
        }
        if text
            .get(..10)
            .is_some_and(|p| p.eq_ignore_ascii_case("otpauth://"))
        {
            return Self::parse_uri(text);
        }
        Self::new(
            &decode_base32(text)?,
            TotpAlgorithm::Sha1,
            DEFAULT_DIGITS,
            DEFAULT_PERIOD_SECS,
        )
    }

    fn parse_uri(text: &str) -> Result<Self, FillError> {
        let url = Url::parse(text).map_err(|_| bad("the otpauth address is malformed"))?;
        match url.host_str() {
            Some(h) if h.eq_ignore_ascii_case("totp") => {}
            Some(h) if h.eq_ignore_ascii_case("hotp") => {
                return Err(bad("counter-based (hotp) codes are not supported"));
            }
            _ => return Err(bad("the otpauth address is not a totp address")),
        }
        let mut secret = None;
        let mut algorithm = TotpAlgorithm::Sha1;
        let mut digits = DEFAULT_DIGITS;
        let mut period = DEFAULT_PERIOD_SECS;
        for (k, v) in url.query_pairs() {
            match k.to_ascii_lowercase().as_str() {
                "secret" => secret = Some(decode_base32(&v)?),
                "algorithm" => {
                    algorithm = TotpAlgorithm::from_name(&v)
                        .ok_or_else(|| bad("the algorithm is not SHA1, SHA256 or SHA512"))?;
                }
                "digits" => digits = v.parse().map_err(|_| bad("digits is not a number"))?,
                "period" => period = v.parse().map_err(|_| bad("the period is not a number"))?,
                _ => {}
            }
        }
        let key = secret.ok_or_else(|| bad("the address has no secret"))?;
        Self::new(&key, algorithm, digits, period)
    }

    /// The one form this spec is stored in: a minimal `otpauth://` URI that
    /// [`Self::parse`] reads back to an equal spec. It holds the KEY: it goes
    /// only into the secret store and the redactor.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!(
            "otpauth://totp/praxi?secret={}&algorithm={}&digits={}&period={}",
            self.key_base32(),
            self.algorithm.name(),
            self.digits,
            self.period
        )
    }

    /// The key as base32 without padding (what a site would print). For the
    /// redactor only.
    #[must_use]
    pub fn key_base32(&self) -> String {
        BASE32_NOPAD.encode(&self.key)
    }

    /// Number of digits in a code.
    #[must_use]
    pub fn digits(&self) -> u32 {
        self.digits
    }

    /// Seconds a code stays valid.
    #[must_use]
    pub fn period(&self) -> u64 {
        self.period
    }

    /// Seconds left of the code valid at `unix_secs`.
    #[must_use]
    pub fn seconds_left(&self, unix_secs: u64) -> u64 {
        self.period - unix_secs % self.period
    }

    /// The code valid at `unix_secs` (seconds since the epoch), zero-padded.
    #[must_use]
    pub fn code_at(&self, unix_secs: u64) -> String {
        self.hotp(unix_secs / self.period)
    }

    /// RFC 4226 HOTP of `counter` with this key, algorithm and digit count.
    #[must_use]
    pub fn hotp(&self, counter: u64) -> String {
        let msg = counter.to_be_bytes();
        let digest: Zeroizing<Vec<u8>> = Zeroizing::new(match self.algorithm {
            TotpAlgorithm::Sha1 => mac::<Hmac<Sha1>>(&self.key, &msg),
            TotpAlgorithm::Sha256 => mac::<Hmac<Sha256>>(&self.key, &msg),
            TotpAlgorithm::Sha512 => mac::<Hmac<Sha512>>(&self.key, &msg),
        });
        // Dynamic truncation (RFC 4226 section 5.3).
        let offset = usize::from(digest[digest.len() - 1] & 0x0f);
        let bin = u32::from_be_bytes([
            digest[offset] & 0x7f,
            digest[offset + 1],
            digest[offset + 2],
            digest[offset + 3],
        ]);
        let code = bin % 10u32.pow(self.digits);
        format!("{code:0width$}", width = self.digits as usize)
    }
}

/// What replaces a setup key or `otpauth://` address in masked page text.
pub const SEED_MASK: &str = "[REDACTED 2FA setup key]";

/// Group sizes sites print a key in (`JBSW Y3DP`, `JBSWY-3DPEH`, ...).
const GROUP_SIZES: [usize; 6] = [3, 4, 5, 6, 7, 8];

impl TotpSpec {
    /// Find a usable setup in text a page showed: an `otpauth://totp/` address
    /// anywhere in it, else the whole text (optionally after a `Label:`) read
    /// as a base32 key. For the host-side read of a setup field: the text never
    /// reaches the model.
    ///
    /// # Errors
    /// [`FillError::Invalid`] with a fixed message; the text is never echoed.
    pub fn find_in_text(text: &str) -> Result<Self, FillError> {
        let lower = text.to_ascii_lowercase();
        if let Some(at) = lower.find("otpauth://") {
            let uri: String = text[at..]
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>'))
                .collect();
            return Self::parse(&uri);
        }
        // Plain text parses as base32 too ("Enter the 6-digit code" is valid base32
        // letters), so the element must read as a key: one run, or equal groups.
        let is_key = |t: &str| mask_setup_secrets(t.trim()) == SEED_MASK;
        if is_key(text)
            && let Ok(spec) = Self::parse(text)
        {
            return Ok(spec);
        }
        // "Setup key: ABCD EFGH ..." / "Key - ABCD ...": try what follows a label.
        for sep in [':', '\n'] {
            if let Some((head, tail)) = text.split_once(sep)
                && head.len() <= 48
                && is_key(tail)
                && let Ok(spec) = Self::parse(tail)
            {
                return Ok(spec);
            }
        }
        Err(bad(
            "the element does not hold a setup key or an otpauth address",
        ))
    }

    /// Every spelling of this key a page or a person can show or type: base32
    /// upper and lower case, with and without `=` padding, in groups of 3 to 8
    /// characters split by a space or a dash, and the stored `otpauth://` form.
    /// For the redactor, so no later echo of the key survives in any layout.
    #[must_use]
    pub fn seed_spellings(&self) -> Vec<String> {
        let plain = self.key_base32();
        let mut out = vec![self.canonical(), plain.clone()];
        let padded = |s: &str| format!("{s}{}", "=".repeat((8 - s.len() % 8) % 8));
        let cased = |s: String, out: &mut Vec<String>| {
            out.push(s.clone());
            out.push(s.to_ascii_lowercase());
        };
        cased(padded(&plain), &mut out);
        out.push(plain.to_ascii_lowercase());
        for size in GROUP_SIZES {
            let groups: Vec<&str> = plain
                .as_bytes()
                .chunks(size)
                .filter_map(|c| std::str::from_utf8(c).ok())
                .collect();
            for sep in [" ", "-"] {
                let joined = groups.join(sep);
                cased(joined.clone(), &mut out);
                cased(
                    format!("{joined}{}", "=".repeat((8 - plain.len() % 8) % 8)),
                    &mut out,
                );
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Mask what looks like a two-factor setup in `text` (page text a model could
/// read): every `otpauth://` address, and a base32 key written as one run of
/// 16 or more characters or as 4 or more space or dash separated groups. A run
/// must have a digit 2 to 7 or be all upper case, so ordinary long words stay.
/// Shape-based and independent of what is stored: a key is hidden from the very
/// first read of the page, before anything is saved.
#[must_use]
pub fn mask_setup_secrets(text: &str) -> String {
    use std::sync::LazyLock;
    static URI: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)otpauth://[^\s"'<>]+"#).unwrap_or_else(|_| unreachable!())
    });
    static RUN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?:[a-z2-7]{4,8}(?:[ \-][a-z2-7]{1,8}){3,}|[a-z2-7]{16,})=*(?:[^a-z0-9]|$)",
        )
        .unwrap_or_else(|_| unreachable!())
    });
    let step = URI.replace_all(text, SEED_MASK);
    RUN.replace_all(&step, |caps: &regex::Captures<'_>| {
        let whole = &caps[0];
        // The class above also swallowed one trailing non-word character: keep it.
        let (core, tail) = match whole.char_indices().last() {
            Some((i, c)) if !c.is_ascii_alphanumeric() && c != '=' => (&whole[..i], &whole[i..]),
            _ => (whole, ""),
        };
        let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let has_digit = letters.iter().any(char::is_ascii_digit);
        let all_upper = letters.iter().all(|c| !c.is_ascii_lowercase());
        let sizes: Vec<usize> = core
            .split([' ', '-'])
            .map(|g| g.trim_end_matches('=').len())
            .collect();
        let grouped = sizes.len() > 1;
        // Sites print equal groups (the last may be short): `JBSW Y3DP EHPK 3PXP`.
        let even = sizes.split_last().is_some_and(|(last, rest)| {
            rest.first()
                .is_some_and(|f| rest.iter().all(|n| n == f) && last <= f)
        });
        let long_enough = letters.len() >= 16;
        let seedlike = long_enough
            && if grouped {
                even && has_digit
            } else {
                has_digit || all_upper
            };
        if seedlike {
            format!("{SEED_MASK}{tail}")
        } else {
            whole.to_owned()
        }
    })
    .into_owned()
}

fn mac<M: Mac + hmac::digest::KeyInit>(key: &[u8], msg: &[u8]) -> Vec<u8> {
    // HMAC accepts a key of any length, so `new_from_slice` cannot fail.
    let mut m = <M as Mac>::new_from_slice(key).unwrap_or_else(|_| unreachable!());
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1_KEY: &[u8] = b"12345678901234567890";
    const SHA256_KEY: &[u8] = b"12345678901234567890123456789012";
    const SHA512_KEY: &[u8] = b"1234567890123456789012345678901234567890123456789012345678901234";

    fn spec(key: &[u8], a: TotpAlgorithm, digits: u32) -> TotpSpec {
        TotpSpec::new(key, a, digits, 30).unwrap()
    }

    /// RFC 4226 appendix D: HOTP values for counters 0..=9 (6 digits).
    #[test]
    fn rfc4226_hotp_vectors() {
        let s = spec(SHA1_KEY, TotpAlgorithm::Sha1, 6);
        let want = [
            "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
            "399871", "520489",
        ];
        for (counter, code) in want.iter().enumerate() {
            assert_eq!(s.hotp(counter as u64), *code, "counter {counter}");
        }
    }

    /// RFC 6238 appendix B: 8-digit codes for the three hashes.
    #[test]
    fn rfc6238_vectors() {
        let sha1 = spec(SHA1_KEY, TotpAlgorithm::Sha1, 8);
        let sha256 = spec(SHA256_KEY, TotpAlgorithm::Sha256, 8);
        let sha512 = spec(SHA512_KEY, TotpAlgorithm::Sha512, 8);
        let table: [(u64, &str, &str, &str); 6] = [
            (59, "94287082", "46119246", "90693936"),
            (1_111_111_109, "07081804", "68084774", "25091201"),
            (1_111_111_111, "14050471", "67062674", "99943326"),
            (1_234_567_890, "89005924", "91819424", "93441116"),
            (2_000_000_000, "69279037", "90698825", "38618901"),
            (20_000_000_000, "65353130", "77737706", "47863826"),
        ];
        for (t, c1, c256, c512) in table {
            assert_eq!(sha1.code_at(t), c1, "sha1 t={t}");
            assert_eq!(sha256.code_at(t), c256, "sha256 t={t}");
            assert_eq!(sha512.code_at(t), c512, "sha512 t={t}");
        }
    }

    #[test]
    fn six_digit_codes_keep_leading_zeros() {
        // 8-digit code 07081804 at t=1111111109 -> 6 digits is `081804`.
        let s = spec(SHA1_KEY, TotpAlgorithm::Sha1, 6);
        assert_eq!(s.code_at(1_111_111_109), "081804");
    }

    #[test]
    fn the_code_changes_exactly_at_the_period_boundary() {
        let s = spec(SHA1_KEY, TotpAlgorithm::Sha1, 6);
        assert_eq!(s.code_at(30), s.code_at(59));
        assert_ne!(s.code_at(59), s.code_at(60));
        assert_eq!(s.seconds_left(59), 1);
        assert_eq!(s.seconds_left(60), 30);
    }

    #[test]
    fn parses_a_base32_key_as_sites_print_it() {
        let b32 = BASE32_NOPAD.encode(SHA1_KEY);
        let spaced: String = b32
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        for text in [
            b32.clone(),
            spaced,
            format!("{b32}======"),
            format!("  {b32}\n"),
        ] {
            let s = TotpSpec::parse(&text).unwrap();
            assert_eq!(
                s.code_at(59),
                spec(SHA1_KEY, TotpAlgorithm::Sha1, 6).code_at(59)
            );
        }
    }

    #[test]
    fn parses_an_otpauth_uri_with_its_parameters() {
        let b32 = BASE32_NOPAD.encode(SHA256_KEY);
        let uri = format!(
            "otpauth://totp/Example:bot%40example.com?secret={b32}&issuer=Example&algorithm=SHA256&digits=8&period=30"
        );
        let s = TotpSpec::parse(&uri).unwrap();
        assert_eq!(s.digits(), 8);
        assert_eq!(s.period(), 30);
        assert_eq!(s.code_at(59), "46119246");
    }

    #[test]
    fn canonical_form_round_trips() {
        let b32 = BASE32_NOPAD.encode(SHA512_KEY);
        let s = TotpSpec::parse(&format!(
            "otpauth://totp/x?secret={b32}&algorithm=SHA512&digits=7&period=60"
        ))
        .unwrap();
        let again = TotpSpec::parse(&s.canonical()).unwrap();
        assert_eq!(again.canonical(), s.canonical());
        assert_eq!(again.code_at(1_234_567_890), s.code_at(1_234_567_890));
        assert!(s.canonical().contains(&s.key_base32()));
    }

    #[test]
    fn refuses_what_it_cannot_use_without_echoing_it() {
        let secretish = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJ1"; // `1` is not base32
        let cases = [
            "",
            "   ",
            secretish,
            "ABCD",
            "otpauth://hotp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&counter=1",
            "otpauth://totp/x",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=12",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&period=0",
            "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=MD5",
            "otpauth://other/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        ];
        for text in cases {
            let err = TotpSpec::parse(text).expect_err(text).to_string();
            assert!(!err.contains("GEZDGNBV"), "{err}");
        }
        assert!(TotpSpec::parse(&"A".repeat(MAX_INPUT_BYTES + 1)).is_err());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let s = spec(SHA1_KEY, TotpAlgorithm::Sha1, 6);
        let shown = format!("{s:?}");
        assert!(!shown.contains("1234"), "{shown}");
    }
}
