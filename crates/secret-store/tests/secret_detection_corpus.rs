//! Corpus tests for `detect_secrets` (issue #421, slice 2).
//!
//! Positive values are FAKE and are assembled at run time from a prefix and a
//! body, so no secret-shaped literal sits in the source (it would trip secret
//! scanners and push protection). Hard targets: every positive is detected
//! with the expected kind and exact span, and nothing in the negative corpus
//! is reported.

use secret_store::{
    Confidence, REDACTED, SecretKind, detect_secrets, redact_candidates, screen_inbound,
};

fn join(prefix: &str, body: &str) -> String {
    format!("{prefix}{body}")
}

/// (kind, value) known provider shapes, high confidence.
fn known_shapes() -> Vec<(SecretKind, String)> {
    vec![
        (
            SecretKind::OpenAiStyleKey,
            join("sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34"),
        ),
        (
            SecretKind::OpenAiStyleKey,
            join("sk-", "ant-api03-Zx98Yw76Vu54Ts32Rq10Po98"),
        ),
        (
            SecretKind::OpenAiStyleKey,
            join("sk-", "or-v1-0a1b2c3d4e5f60718293a4b5c6d7e8f9"),
        ),
        (
            SecretKind::GoogleApiKey,
            join("AI", "zaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"),
        ),
        (
            SecretKind::GithubToken,
            join("gh", "p_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"),
        ),
        (
            SecretKind::GithubToken,
            join(
                "github",
                "_pat_11AAAAAAA0abcdefGHIJKL_mnopqrstuvwxyz0123456789",
            ),
        ),
        (
            SecretKind::SlackToken,
            join("xo", "xb-123456789012-abcdefghijklmnop"),
        ),
        (
            SecretKind::NvidiaKey,
            join("nv", "api-A1b2C3d4E5f6G7h8I9j0K1l2"),
        ),
        (
            SecretKind::Jwt,
            join(
                "ey",
                "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop",
            ),
        ),
        (SecretKind::AwsAccessKeyId, join("AK", "IAABCD1234EFGH5678")),
        (
            SecretKind::StripeKey,
            join("sk_li", "ve_A1b2C3d4E5f6G7h8I9j0K1l2"),
        ),
        (
            SecretKind::TelegramBotToken,
            join("123456789:", "AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw1"),
        ),
        (
            SecretKind::NpmToken,
            join("np", "m_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"),
        ),
        (
            SecretKind::GitlabToken,
            join("gl", "pat-A1b2C3d4E5f6G7h8I9j0"),
        ),
        (
            SecretKind::HuggingFaceToken,
            join("h", "f_abcdefghijklmnopqrstuvwxyzABCDEFGH"),
        ),
        (
            SecretKind::GroqKey,
            join("gs", "k_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0"),
        ),
        (
            SecretKind::DigitalOceanToken,
            join(
                "dop",
                "_v1_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
        ),
    ]
}

#[test]
fn every_known_shape_is_detected_with_exact_span() {
    for (kind, value) in known_shapes() {
        for template in [
            "{}",
            "here is my key: {} thanks",
            "export FOO={}\n",
            "(\"{}\")",
            "anahtarım {} bu",
        ] {
            let text = template.replace("{}", &value);
            let found = detect_secrets(&text);
            assert_eq!(found.len(), 1, "kind {kind} in {template:?}: {found:?}");
            let c = found[0];
            assert_eq!(c.kind, kind, "template {template:?}");
            assert_eq!(c.confidence, Confidence::High);
            assert_eq!(c.slice(&text), Some(value.as_str()), "span for {kind}");
        }
    }
}

#[test]
fn private_key_block_is_detected_whole_and_when_truncated() {
    let begin = join("-----BEGIN ", "OPENSSH PRIVATE KEY-----");
    let end = join("-----END ", "OPENSSH PRIVATE KEY-----");
    let full = format!("see {begin}\nb3BlbnNzaC1rZXktdjEAAAAA\n{end} ok");
    let found = detect_secrets(&full);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, SecretKind::PrivateKey);
    assert!(found[0].slice(&full).unwrap().ends_with(&end));
    let cut = format!("{begin}\nb3BlbnNzaC1rZXktdjEAAAAA");
    let found = detect_secrets(&cut);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].end, cut.len());
}

/// The labelled, card and negative corpora live in one JSON file that the
/// desktop UI's TypeScript port of the detector reads too, so the two cannot
/// drift apart.
fn corpus() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/detect-corpus.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("corpus fixture"))
        .expect("corpus fixture is valid JSON")
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is an array"))
        .iter()
        .map(|s| s.as_str().expect("string").to_owned())
        .collect()
}

/// (text, value[, kind]) rows of a positive corpus.
fn rows(v: &serde_json::Value, key: &str) -> Vec<(String, String, Option<String>)> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is an array"))
        .iter()
        .map(|r| {
            let f = |k: &str| r[k].as_str().map(str::to_owned);
            (f("text").unwrap(), f("value").unwrap(), f("kind"))
        })
        .collect()
}

#[test]
fn labelled_values_are_detected_as_medium_with_exact_span() {
    for (text, value, kind) in rows(&corpus(), "labelled") {
        let found = detect_secrets(&text);
        assert_eq!(found.len(), 1, "{text:?}: {found:?}");
        let c = found[0];
        assert_eq!(Some(c.kind.as_str()), kind.as_deref(), "{text:?}");
        assert_eq!(c.confidence, Confidence::Medium, "{text:?}");
        assert_eq!(c.slice(&text), Some(value.as_str()), "{text:?}");
    }
}

#[test]
fn several_secrets_in_one_message_are_all_found_in_order() {
    let a = join("gh", "p_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
    let text = format!("repo token {a} and sifre: Tr0ub4dor&3 done");
    let found = detect_secrets(&text);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].kind, SecretKind::GithubToken);
    assert_eq!(found[1].kind, SecretKind::LabelledPassword);
    assert!(found[0].end <= found[1].start);
}

#[test]
fn card_numbers_are_detected_with_exact_span() {
    for (text, value, _) in rows(&corpus(), "cards") {
        let found = detect_secrets(&text);
        assert_eq!(found.len(), 1, "{text:?}: {found:?}");
        assert_eq!(found[0].kind, SecretKind::PaymentCard, "{text:?}");
        assert_eq!(found[0].confidence, Confidence::Medium);
        assert_eq!(found[0].slice(&text), Some(value.as_str()), "{text:?}");
    }
}

#[test]
fn redact_candidates_removes_every_value_and_keeps_the_rest() {
    let key = join("sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34");
    let text = format!("a {key} b sifre: Tr0ub4dor&3 c");
    let found = detect_secrets(&text);
    assert_eq!(found.len(), 2);
    let out = redact_candidates(&text, &found);
    assert_eq!(out, format!("a {REDACTED} b sifre: {REDACTED} c"));
    assert!(!out.contains("Ab12Cd34") && !out.contains("Tr0ub4dor"));
    assert_eq!(redact_candidates("nothing here", &[]), "nothing here");
}

/// Everything here must produce ZERO candidates.
fn negatives() -> Vec<String> {
    strings(&corpus(), "negatives")
}

#[test]
fn negative_corpus_has_zero_false_positives() {
    let mut failures = Vec::new();
    for text in negatives() {
        let found = detect_secrets(&text);
        if !found.is_empty() {
            failures.push(format!("{text:?} -> {found:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "false positives:\n{}",
        failures.join("\n")
    );
}

#[test]
fn negatives_stay_clean_inside_longer_messages_and_unicode() {
    for text in negatives() {
        let wrapped = format!("Şu mesajı gönder 🙂 «{text}» çok teşekkürler\nsonra devam");
        assert_eq!(detect_secrets(&wrapped).len(), 0, "{wrapped:?}");
    }
}

#[test]
fn empty_and_huge_inputs_are_fine() {
    assert_eq!(detect_secrets("").len(), 0);
    let big = "ahahahah token: ".repeat(50_000);
    assert_eq!(detect_secrets(&big).len(), 0);
}

#[test]
fn screen_inbound_cuts_values_and_says_when_nothing_else_is_left() {
    let key = join("sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34");
    let none = screen_inbound("hello, what is the weather like? ahahahah");
    assert_eq!(none, None);

    let mixed = screen_inbound(&format!(
        "deploy with {key} and card 4111 1111 1111 1111 now"
    ))
    .expect("two secrets");
    assert_eq!(
        mixed.text,
        format!("deploy with {REDACTED} and card {REDACTED} now")
    );
    assert_eq!(
        mixed.kinds,
        vec![SecretKind::OpenAiStyleKey, SecretKind::PaymentCard]
    );
    assert!(!mixed.only_secrets);

    let bare = screen_inbound(&format!(" {key}\n")).expect("a bare key");
    assert!(bare.only_secrets);
    assert!(!bare.text.contains("Ab12Cd34"));

    let twice = screen_inbound(&format!("{key} {key}")).expect("same kind twice");
    assert_eq!(twice.kinds, vec![SecretKind::OpenAiStyleKey]);
    assert!(twice.only_secrets);

    // A label keeps its words, so the rest of the sentence is still there.
    let labelled = screen_inbound("sifre: Tr0ub4dor&3").expect("a password");
    assert!(!labelled.only_secrets);
    assert_eq!(labelled.text, format!("sifre: {REDACTED}"));
}
