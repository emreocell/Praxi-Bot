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

/// (kind, value) known provider shapes, high confidence. The rows are in the
/// shared corpus (a prefix and a body, joined here), which the TypeScript port
/// checks too.
fn known_shapes() -> Vec<(String, String)> {
    corpus()["shapes"]
        .as_array()
        .expect("shapes is an array")
        .iter()
        .map(|r| {
            let f = |k: &str| r[k].as_str().expect("string").to_owned();
            (f("kind"), join(&f("prefix"), &f("body")))
        })
        .collect()
}

#[test]
fn every_known_shape_is_detected_with_exact_span() {
    let shapes = known_shapes();
    assert!(shapes.len() >= 17);
    for (kind, value) in shapes {
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
            assert_eq!(c.kind.as_str(), kind, "template {template:?}");
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

/// Run `f` on a thread and fail, rather than hang the suite, if it is not done in `limit`.
fn within<T: Send + 'static>(
    limit: std::time::Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit)
        .expect("the detector did not finish in time on a long input")
}

/// Pathological long inputs from the shared corpus (the TypeScript port runs
/// the same rows): none may take more than seconds, and where the row says how
/// many secrets are in it the count is exact.
#[test]
fn long_pathological_inputs_finish_fast_with_the_expected_result() {
    let stress = corpus()["stress"].as_array().expect("stress rows").clone();
    assert!(stress.len() >= 10);
    for row in stress {
        let name = row["name"].as_str().unwrap().to_owned();
        let text = row["unit"]
            .as_str()
            .unwrap()
            .repeat(usize::try_from(row["times"].as_u64().unwrap()).unwrap());
        let expected = row["found"].as_u64();
        assert!(text.len() > 100_000, "{name} is a long input");
        let started = std::time::Instant::now();
        let (found, screened) = within(std::time::Duration::from_secs(60), move || {
            let found = detect_secrets(&text).len();
            let screened = screen_inbound(&text).map(|s| s.text.len());
            (found, screened)
        });
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{name} took {:?}",
            started.elapsed()
        );
        if let Some(n) = expected {
            assert_eq!(found as u64, n, "{name}");
            assert_eq!(screened.is_some(), n > 0, "{name}");
        }
    }
}

/// A key at the very end of a text much longer than one scan window is still
/// found, and one inside a long run of words is cut out whole.
#[test]
fn a_secret_deep_inside_a_long_text_is_cut_out() {
    let key = join("sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34");
    let words = "alpha beta gamma ".repeat(30_000);
    let text = format!("{words}{key} {words}");
    let screened = within(std::time::Duration::from_secs(60), move || {
        screen_inbound(&text)
    })
    .expect("a secret");
    assert_eq!(screened.text.len(), words.len() * 2 + REDACTED.len() + 1);
    assert!(!screened.text.contains("Ab12Cd34"));
}
