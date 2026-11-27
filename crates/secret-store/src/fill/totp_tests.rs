//! Two-factor codes of bot-made accounts: storage, scope, origin binding and
//! the guarantee that the key never leaves the store.

use super::*;
use crate::memory::MemoryStore;
use data_encoding::BASE32_NOPAD;

const BOT: &str = "bot-a";
const OTHER_BOT: &str = "bot-b";
const SITE: &str = "https://example.com";
const NAME: &str = "login/example.com-aaaa1111";
/// RFC 6238 appendix B SHA-1 key.
const RFC_KEY: &[u8] = b"12345678901234567890";

fn rfc_b32() -> String {
    BASE32_NOPAD.encode(RFC_KEY)
}

fn rfc_uri_8_digits() -> String {
    format!("otpauth://totp/Example:bot?secret={}&digits=8", rfc_b32())
}

fn account(name: &str) -> NewAccount<'_> {
    NewAccount {
        name,
        site: SITE,
        login_id: "bot@example.com",
        email_account: None,
        bot_id: BOT,
        note: None,
        password_len: 20,
        symbols: true,
    }
}

fn with_account() -> (Arc<MemoryStore>, FillSecrets) {
    let store = Arc::new(MemoryStore::new());
    let s = FillSecrets::in_memory(store.clone());
    s.register_account(&account(NAME)).unwrap();
    (store, s)
}

fn totp_ref() -> String {
    format!("{TOTP_REF_PREFIX}{NAME}")
}

#[test]
fn the_reference_form_never_collides_with_a_stored_name() {
    assert_eq!(totp_reference("totp:login/x"), Some("login/x"));
    assert_eq!(totp_reference("TOTP:login/x"), Some("login/x"));
    assert_eq!(totp_reference("login/x"), None);
    assert_eq!(totp_reference("totp"), None);
    // `:` is not allowed in a fill name, so `totp:x` can never be one.
    assert!(validate_fill_name("totp:login/x").is_err());
    // Neither can anything map onto the seed's storage key.
    assert!(matches!(
        validate_fill_name("__totp/login/x"),
        Err(FillError::Reserved(_))
    ));
}

#[test]
fn the_code_for_a_saved_key_matches_the_rfc_vector() {
    let (_, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_uri_8_digits()).unwrap();
    let code = s.resolve_totp(NAME, Some(BOT), 59).unwrap();
    assert_eq!(code.expose(), "94287082");
    let code = s.resolve_totp(NAME, Some(BOT), 1_111_111_109).unwrap();
    assert_eq!(code.expose(), "07081804");
}

#[test]
fn resolve_value_for_a_totp_reference_gives_the_current_code_not_the_password() {
    let (_, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    let spec = TotpSpec::parse(&rfc_b32()).unwrap();
    let before = unix_secs();
    let got = s.resolve_value_for(&totp_ref(), Some(BOT)).unwrap();
    let after = unix_secs();
    assert!(
        (before..=after).any(|t| spec.code_at(t) == got.expose()),
        "not the code of the current period"
    );
    assert_eq!(got.expose().len(), 6);
    assert!(got.expose().bytes().all(|b| b.is_ascii_digit()));
    let password = s.resolve_value_for(NAME, Some(BOT)).unwrap();
    assert_ne!(password.expose(), got.expose());
}

#[test]
fn the_key_is_stored_apart_and_never_listed_or_revealed() {
    let (store, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_uri_8_digits()).unwrap();
    // Stored under a key no fill name can address.
    assert!(store.get(&totp_key(NAME)).unwrap().is_some());
    assert!(s.accounts(Some(BOT)).unwrap()[0].has_totp);
    // Nothing the owner or a bot may read names it.
    let shown = format!(
        "{:?}{:?}{:?}",
        s.list().unwrap(),
        s.accounts(None).unwrap(),
        s.list_for_bot(Some(BOT)).unwrap()
    );
    let b32 = rfc_b32();
    assert!(!shown.contains(&b32), "{shown}");
    assert!(!shown.contains("otpauth"), "{shown}");
    // The owner reveal path refuses the reference form (it is not a fill name).
    assert!(s.resolve_value_for_owner(&totp_ref()).is_err());
    assert!(
        s.resolve_value_for_owner(&format!("__totp/{NAME}"))
            .is_err()
    );
    // The metadata file would hold names and flags only.
    let meta = serde_json::to_string(&*s.meta.lock()).unwrap();
    assert!(!meta.contains(&b32) && !meta.contains("otpauth"), "{meta}");
}

#[test]
fn the_redactor_hook_sees_the_key_and_every_code() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let store = Arc::new(MemoryStore::new());
    let s = FillSecrets::in_memory(store).with_value_hook(Arc::new(move |v| {
        sink.lock().push(v.to_owned());
    }));
    s.register_account(&account(NAME)).unwrap();
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    let code = s.resolve_value_for(&totp_ref(), Some(BOT)).unwrap();
    let seen = seen.lock();
    assert!(
        seen.contains(&rfc_b32()),
        "the base32 key was not registered"
    );
    assert!(seen.iter().any(|v| v.starts_with("otpauth://totp/")));
    assert!(
        seen.contains(&code.expose().to_owned()),
        "the code was not registered"
    );
    drop(seen);
    let redacted = s.values_for_redaction().unwrap();
    assert!(redacted.contains(&rfc_b32()));
}

#[test]
fn only_the_bot_that_owns_the_account_may_save_or_use_a_key() {
    let (_, s) = with_account();
    for who in [Some(OTHER_BOT), None, Some("")] {
        assert!(matches!(
            s.set_totp(NAME, who, &rfc_b32()),
            Err(FillError::NotAllowed(_))
        ));
    }
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    for who in [Some(OTHER_BOT), None] {
        assert!(s.check_bot(&totp_ref(), who).is_err());
        assert!(s.resolve_value_for(&totp_ref(), who).is_err());
        assert!(
            s.check_browser(&totp_ref(), Some(SITE), who).is_err(),
            "{who:?}"
        );
    }
    s.check_bot(&totp_ref(), Some(BOT)).unwrap();
}

#[test]
fn the_owners_own_secrets_have_no_two_factor_codes_here() {
    let (_, s) = with_account();
    let owner = "login/owners-own";
    s.upsert_with(
        owner,
        FillSecretUpdate {
            value: Some("hunter2hunter2"),
            origins: Some(&[SITE.to_owned()]),
            bot_ids: Some(&[BOT.to_owned()]),
            ..FillSecretUpdate::default()
        },
    )
    .unwrap();
    assert!(matches!(
        s.set_totp(owner, Some(BOT), &rfc_b32()),
        Err(FillError::NotAllowed(_))
    ));
    let reference = format!("{TOTP_REF_PREFIX}{owner}");
    assert!(s.check_bot(&reference, Some(BOT)).is_err());
    assert!(s.resolve_value_for(&reference, Some(BOT)).is_err());
    assert!(s.check_browser(&reference, Some(SITE), Some(BOT)).is_err());
    // An unknown name is refused too, never "free".
    assert!(matches!(
        s.check_bot("totp:login/nobody", Some(BOT)),
        Err(FillError::Unknown(_))
    ));
}

#[test]
fn a_code_needs_a_saved_key() {
    let (_, s) = with_account();
    let e = s.check_bot(&totp_ref(), Some(BOT)).unwrap_err().to_string();
    assert!(e.contains("account_totp_save"), "{e}");
    assert!(s.resolve_value_for(&totp_ref(), Some(BOT)).is_err());
    assert!(!s.accounts(None).unwrap()[0].has_totp);
}

#[test]
fn the_code_is_typed_only_on_the_accounts_own_origin_and_never_by_grant() {
    let (_, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    for off in [
        "https://evil.example",
        "http://example.com",
        "https://example.com:8443",
    ] {
        assert!(
            matches!(
                s.check_browser(&totp_ref(), Some(off), Some(BOT)),
                Err(FillError::NotAllowed(_))
            ),
            "{off}"
        );
    }
    // No verifiable origin: refused.
    assert!(s.check_browser(&totp_ref(), None, Some(BOT)).is_err());
    // A frame of another origin inside the account's page is refused as well.
    assert!(
        s.check_browser_in(
            &totp_ref(),
            Some("https://evil.example"),
            Some(SITE),
            Some(BOT)
        )
        .is_err()
    );
    assert_eq!(
        s.check_browser_in(&totp_ref(), Some(SITE), Some(SITE), Some(BOT))
            .unwrap(),
        None
    );
    // A standing grant for the password does not cover the code.
    s.upsert_with(
        NAME,
        FillSecretUpdate {
            ask_before_use: Some(false),
            ..FillSecretUpdate::default()
        },
    )
    .unwrap();
    s.grant_create(NAME, SITE).unwrap();
    assert!(
        s.check_browser(NAME, Some(SITE), Some(BOT))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        s.check_browser(&totp_ref(), Some(SITE), Some(BOT)).unwrap(),
        None
    );
}

#[test]
fn a_desktop_program_never_receives_a_code() {
    let (_, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    assert!(
        s.check_desktop(&totp_ref(), Some("prog"), Some(BOT))
            .is_err()
    );
}

#[test]
fn an_unusable_setup_text_changes_nothing_and_is_not_echoed() {
    let (store, s) = with_account();
    let junk = "not-a-key-0189";
    let e = s.set_totp(NAME, Some(BOT), junk).unwrap_err().to_string();
    assert!(!e.contains(junk), "{e}");
    assert!(store.get(&totp_key(NAME)).unwrap().is_none());
    assert!(!s.accounts(None).unwrap()[0].has_totp);
}

#[test]
fn saving_again_replaces_the_key() {
    let (_, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_uri_8_digits()).unwrap();
    let other = BASE32_NOPAD.encode(b"ABCDEFGHIJABCDEFGHIJ");
    s.set_totp(
        NAME,
        Some(BOT),
        &format!("otpauth://totp/x?secret={other}&digits=8"),
    )
    .unwrap();
    let new = TotpSpec::parse(&other).unwrap();
    let got = s.resolve_totp(NAME, Some(BOT), 59).unwrap();
    assert_ne!(got.expose(), "94287082");
    assert_eq!(got.expose().len(), 8);
    // The 6-digit code is the last six digits of the 8-digit one (same HOTP).
    assert_eq!(&got.expose()[2..], new.code_at(59));
}

#[test]
fn a_failed_metadata_write_rolls_the_key_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let s = FillSecrets::open(store.clone(), dir.path()).unwrap();
    s.register_account(&account(NAME)).unwrap();
    std::fs::remove_dir_all(dir.path()).unwrap();
    assert!(s.set_totp(NAME, Some(BOT), &rfc_b32()).is_err());
    assert!(
        store.get(&totp_key(NAME)).unwrap().is_none(),
        "key left behind"
    );
    assert!(!s.accounts(None).unwrap()[0].has_totp);
}

#[test]
fn discarding_or_deleting_the_account_removes_the_key() {
    let (store, s) = with_account();
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    s.discard_account(NAME, Some(BOT)).unwrap();
    assert!(store.get(&totp_key(NAME)).unwrap().is_none());

    s.register_account(&account(NAME)).unwrap();
    // A fresh account of the same name starts without a key.
    assert!(!s.accounts(None).unwrap()[0].has_totp);
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    assert!(s.delete(NAME).unwrap());
    assert!(store.get(&totp_key(NAME)).unwrap().is_none());
}

#[test]
fn the_flag_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let s = FillSecrets::open(store.clone(), dir.path()).unwrap();
    s.register_account(&account(NAME)).unwrap();
    s.set_totp(NAME, Some(BOT), &rfc_b32()).unwrap();
    drop(s);
    let s = FillSecrets::open(store, dir.path()).unwrap();
    assert!(s.accounts(None).unwrap()[0].has_totp);
    s.check_bot(&totp_ref(), Some(BOT)).unwrap();
    // And the file holds no key material.
    let raw = std::fs::read_to_string(metadata_path(dir.path())).unwrap();
    assert!(
        !raw.contains(&rfc_b32()) && !raw.contains("otpauth"),
        "{raw}"
    );
}
