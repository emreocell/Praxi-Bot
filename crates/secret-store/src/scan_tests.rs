use super::*;
use crate::redact::{REDACTED, Redactor};

/// Key-shaped fixtures are assembled from pieces at run time so that no
/// complete fake key sits in the source (GitHub push protection and other
/// scanners would flag it). None of these is a real credential.
fn join(parts: &[&str]) -> String {
    parts.concat()
}

fn golden() -> Vec<(&'static str, String)> {
    let mut all = golden_ai_and_vcs();
    all.extend(golden_cloud_and_misc());
    all
}

const B36: &str = "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5";

fn golden_ai_and_vcs() -> Vec<(&'static str, String)> {
    let b36 = B36;
    vec![
        (
            "openai-api-key",
            join(&[
                "sk-proj-",
                "Zx9Qw3Er5Ty7Ui1Op2As4Df6Gh8Jk0LmNb2Vc4Xz6Aq8Sw0De2Fr4Gt6Hy",
                "T3Blbk",
                "FJ",
                "Zx9Qw3Er5Ty7Ui1Op2As4Df6Gh8Jk0LmNb2Vc4Xz6Aq8Sw0De2Fr4Gt6Hy",
            ]),
        ),
        (
            "shape:openai",
            join(&["sk-or-v1-", "a1b2c3d4e5f60718293a4b5c6d7e8f90"]),
        ),
        (
            "anthropic-api-key",
            join(&[
                "sk-ant-",
                "api03-",
                "Zx9Qw3Er5Ty7Ui1Op2As4Df6Gh8Jk0LmNb2Vc4Xz6Aq8Sw0De2Fr4Gt6HyLo4Pa6Sx8Cd0Vf2Bg4Nh6Mj8Kl0Qw2Er4Ty",
                "AA",
            ]),
        ),
        (
            "gcp-api-key",
            join(&["AIza", "SyD4n3xQw9ZkL2mV8bT6rYh1jUe5oPcA7sG"]),
        ),
        ("github-pat", join(&["gh", "p_", b36])),
        (
            "github-fine-grained-pat",
            join(&[
                "github_",
                "pat_",
                "11ABCDEFG0aB3dE5gH7jK9_mN1pQ3sT5vW7yZ9bC1dE3fG5hJ7kL9mN1pQ3sT5vW7yZ9bC1dE3fG5hJ7kp",
            ]),
        ),
        ("github-oauth", join(&["gh", "o_", b36])),
    ]
}

fn golden_cloud_and_misc() -> Vec<(&'static str, String)> {
    vec![
        (
            "slack-bot-token",
            join(&[
                "xo",
                "xb-",
                "2345678901234-3456789012345-aB3dE5gH7jK9mN1pQ3sT5vW7",
            ]),
        ),
        (
            "slack-user-token",
            join(&[
                "xo",
                "xp-",
                "2345678901-3456789012-4567890123-aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE",
            ]),
        ),
        ("aws-access-token", join(&["AK", "IAZ3MSJV4WWQ7KXPLR"])),
        (
            "stripe-access-token",
            join(&["sk_", "live_", "aB3dE5gH7jK9mN1pQ3sT5vW7"]),
        ),
        (
            "jwt",
            join(&[
                "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.",
                "eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.",
                "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
            ]),
        ),
        (
            "private-key",
            join(&[
                "-----BEGIN RSA PRIVATE",
                " KEY-----\n",
                "MIIEowIBAAKCAQEAu1SU1LfVLPHCozMxH2Mo4lgOEePzNm0tRgeLezV6ffAt0gun\n",
                "VTLw7onLRnrq0/IzW7yWR7QkrmBL7jTKEn5u+qKhbwKfBstIs+bMY2Zkp18gnTxK\n",
                "-----END RSA PRIVATE",
                " KEY-----",
            ]),
        ),
        ("gitlab-pat", join(&["gl", "pat-", "aB3dE5gH7jK9mN1pQ3sT"])),
        (
            "huggingface-access-token",
            join(&["h", "f_", "aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgH"]),
        ),
        (
            "sendgrid-api-token",
            join(&[
                "S",
                "G.",
                "aB3dE5gH7jK9mN1pQ3sT5v",
                ".",
                "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5hJ7kL9m",
            ]),
        ),
        (
            "shopify-access-token",
            join(&["shp", "at_", "a1b2c3d4e5f60718293a4b5c6d7e8f90"]),
        ),
        (
            "digitalocean-pat",
            join(&[
                "do",
                "p_v1_",
                "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90",
            ]),
        ),
        (
            "shape:nvidia",
            join(&["nv", "api-", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ"]),
        ),
        (
            "npm-access-token",
            join(&["np", "m_", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5"]),
        ),
    ]
}

#[test]
fn every_rule_compiles() {
    let e = engine();
    let bad: Vec<&str> = (0..RULES.len())
        .filter(|&i| compiled(e, i).is_none())
        .map(|i| RULES[i].id)
        .collect();
    assert!(bad.is_empty(), "rules the regex engine rejects: {bad:?}");
}

#[test]
fn golden_key_shapes_are_found_whole() {
    for (id, key) in golden() {
        let text = format!("here you go: {key} thanks");
        let found = scan_text(&text);
        assert_ne!(found.len(), 0, "{id}: nothing found");
        // The finding must cover the secret, not only a prefix of it.
        let start = text.find(&key).unwrap();
        let covered = found
            .iter()
            .any(|f| f.start <= start + 12 && f.end >= start + key.len() - 1);
        assert!(covered, "{id}: findings {found:?} do not cover the key");
        // Masked output never keeps the tail of the key.
        let masked = Redactor::new().redact(&text);
        let tail = &key[key.len() - 8..];
        assert!(!masked.contains(tail), "{id}: tail leaked: {masked}");
        assert!(masked.contains(REDACTED), "{id}");
    }
}

#[test]
fn golden_keys_report_the_expected_rule() {
    for (id, key) in golden() {
        let found = scan_text(&format!("x = {key}"));
        // Hand-tuned shapes win ties (same start): accept either the gitleaks
        // id or any finding at all for shape-only cases.
        let ok = found.iter().any(|f| f.rule_id == id) || id.starts_with("shape:");
        let ids: Vec<_> = found.iter().map(|f| f.rule_id).collect();
        assert!(ok, "{id}: got {ids:?}");
    }
}

#[test]
fn findings_never_print_the_value() {
    let key = join(&["gh", "p_", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5"]);
    let found = scan_text(&key);
    assert_ne!(found.len(), 0);
    assert!(!format!("{found:?}").contains("aB3dE5"));
}

#[test]
fn placeholder_keys_from_docs_are_not_findings() {
    // gitleaks' own allowlist: the AWS docs example key.
    assert_eq!(scan_text("AKIAIOSFODNN7EXAMPLE").len(), 0);
}

#[test]
fn entropy_separates_random_from_repetitive() {
    assert!(entropy(&"a".repeat(36)) < 0.1);
    assert!(entropy("aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5") > 4.5);
}

#[test]
fn ordinary_prose_has_no_findings() {
    let corpus = [
        // English
        "Please keep the API key out of the repository, and rotate the token every quarter.",
        "The secret to a good stew is patience; add salt and a bay leaf, then wait.",
        "My password manager asked me to update the master password, so I did.",
        "We use sk-learn and the task-runner to tokenize 'ghp' in the unit tests.",
        "Commit 5f6ec56cfa6f8c67648d863582c7ae2f206d5c1c fixed the build on main.",
        "Visit https://example.com/docs/getting-started?lang=en#install for the guide.",
        "let key = \"name\"; let token = lexer.next_token(); // not a credential",
        "Our monkey, Turkey and keyboard topics are in chapter 4: key-value stores.",
        "Meeting at 14:30 in room B-204; bring the quarterly report and your laptop.",
        "The access control list was reviewed by the security team on Tuesday.",
        // Turkish
        "Şifremi unuttum, lütfen bana yardımcı olur musun? Anahtar teslim bir çözüm istiyorum.",
        "Gizli anahtar kelimesini cümle içinde kullandım ama bu bir parola değil.",
        "Bugün hava çok güzel; öğleden sonra Kadıköy'de sahil boyunca yürüyüşe çıkacağız.",
        "API anahtarı nedir, nasıl çalışır? Sorusunu arkadaşıma anlattım, jeton sistemini de.",
        "Toplantı notları: bütçe, takvim ve sorumluluklar bir sonraki cuma günü paylaşılacak.",
        "İstanbul'dan Ankara'ya giden tren saat 09:15'te kalkıyor, biletimi çoktan aldım.",
        "Parolanızı kimseyle paylaşmayın; bankanız sizden asla şifre veya jeton istemez.",
    ];
    for line in corpus {
        let found = scan_text(line);
        assert_eq!(found.len(), 0, "false positive in {line:?}: {found:?}");
        assert_eq!(Redactor::new().redact(line), line);
    }
}

#[test]
fn keyword_prefilter_only_selects_rules_with_a_keyword_hit() {
    assert_eq!(candidate_rules("zzz qqq www plain words").len(), 0);
    let picked = candidate_rules("my AKIA key");
    assert!(
        !picked.is_empty() && picked.len() < RULES.len() / 4,
        "{}",
        picked.len()
    );
    assert!(picked.iter().any(|&i| RULES[i].id == "aws-access-token"));
}
