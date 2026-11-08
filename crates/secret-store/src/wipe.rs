//! Best-effort wiping of secret bytes after encryption, decryption, and drop.
//!
//! The helpers record what they wiped under `cfg(test)` so a regression test can
//! see that the real key, plaintext, and secret strings passed through them.

use std::ops::Deref;

use zeroize::{Zeroize, ZeroizeOnDrop};

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WipeKind {
    Key,
    Plaintext,
    Secret(usize),
}

#[cfg(test)]
thread_local! {
    static WIPES: std::cell::RefCell<Vec<WipeKind>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn take_wipes() -> Vec<WipeKind> {
    WIPES.with(|wipes| std::mem::take(&mut *wipes.borrow_mut()))
}

#[cfg(test)]
fn record(kind: WipeKind) {
    WIPES.with(|wipes| wipes.borrow_mut().push(kind));
}

pub(crate) fn wipe_key(key: &mut [u8; 32]) {
    key.zeroize();
    #[cfg(test)]
    record(WipeKind::Key);
}

pub(crate) fn wipe_plaintext(plain: &mut Vec<u8>) {
    plain.zeroize();
    #[cfg(test)]
    record(WipeKind::Plaintext);
}

pub(crate) fn wipe_string(secret: &mut String) {
    #[cfg(test)]
    let len = secret.len();
    secret.zeroize();
    #[cfg(test)]
    record(WipeKind::Secret(len));
}

/// Passphrase bytes. `Clone` copies them; each copy is wiped on drop.
#[derive(Clone)]
pub(crate) struct SecretString(String);

impl SecretString {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }
}

impl Deref for SecretString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl ZeroizeOnDrop for SecretString {}

/// Plaintext JSON. Wiped as soon as the value is dropped.
pub(crate) struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for SecretBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        wipe_plaintext(&mut self.0);
    }
}

impl ZeroizeOnDrop for SecretBytes {}

/// Derived 32-byte file key. Wiped on drop, including error paths.
pub(crate) struct DerivedKey([u8; 32]);

impl DerivedKey {
    pub(crate) fn new() -> Self {
        Self([0u8; 32])
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl Drop for DerivedKey {
    fn drop(&mut self) {
        wipe_key(&mut self.0);
    }
}

impl ZeroizeOnDrop for DerivedKey {}
