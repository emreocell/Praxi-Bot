//! One-time pairing invites and the `bot-pair:<base32>` ticket format.

use std::time::{Duration, Instant};

use iroh::EndpointAddr;
use parking_lot::Mutex;
use qrcode::{QrCode, render::unicode};
use serde::{Deserialize, Serialize};

use crate::error::{PeerError, Result};
use crate::proto::PROTOCOL_VERSION;
use crate::store::Role;

/// Prefix of the QR-able pairing string.
pub const TICKET_PREFIX: &str = "bot-pair:";
/// How long an invite stays valid.
pub const INVITE_TTL: Duration = Duration::from_secs(120);
/// Wrong attempts tolerated before an invite is destroyed.
const MAX_FAILED: u32 = 5;

#[derive(Serialize, Deserialize)]
struct Payload {
    v: u32,
    addr: EndpointAddr,
    secret: String,
    code: String,
    role: Role,
}

/// A decoded pairing ticket (what the joining side scans or pastes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairTicket {
    /// Dialable address of the core that created the invite.
    pub addr: EndpointAddr,
    /// One-time secret (hex).
    pub secret: String,
    /// Six-digit code shown on the core for visual confirmation.
    pub code: String,
    /// The role the core expects the joiner to take.
    pub role: Role,
}

impl PairTicket {
    /// Encode as `bot-pair:<base32>`.
    ///
    /// # Panics
    ///
    /// Never in practice: serialising these plain types cannot fail.
    #[must_use]
    pub fn encode(&self) -> String {
        let payload = Payload {
            v: PROTOCOL_VERSION,
            addr: self.addr.clone(),
            secret: self.secret.clone(),
            code: self.code.clone(),
            role: self.role,
        };
        let json = serde_json::to_vec(&payload).expect("ticket payload serialises");
        format!(
            "{TICKET_PREFIX}{}",
            data_encoding::BASE32_NOPAD.encode(&json)
        )
    }

    /// Decode a ticket string (case-insensitive base32).
    ///
    /// # Errors
    ///
    /// Returns [`PeerError::Protocol`] for malformed input and
    /// [`PeerError::VersionMismatch`] for tickets from another protocol version.
    pub fn parse(s: &str) -> Result<Self> {
        let body = s.trim().strip_prefix(TICKET_PREFIX).ok_or_else(|| {
            PeerError::Protocol(format!("ticket must start with '{TICKET_PREFIX}'"))
        })?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|e| PeerError::Protocol(format!("ticket is not valid base32: {e}")))?;
        let p: Payload = serde_json::from_slice(&bytes)
            .map_err(|e| PeerError::Protocol(format!("ticket content is invalid: {e}")))?;
        if p.v != PROTOCOL_VERSION {
            return Err(PeerError::VersionMismatch {
                local: PROTOCOL_VERSION,
                remote: p.v,
            });
        }
        Ok(Self {
            addr: p.addr,
            secret: p.secret,
            code: p.code,
            role: p.role,
        })
    }
}

/// A freshly created invite, ready to display.
#[derive(Debug, Clone)]
pub struct Invite {
    /// `bot-pair:<base32>` string (put it in a QR code or paste it).
    pub ticket: String,
    /// Six-digit confirmation code.
    pub code: String,
    /// The role the joiner must take.
    pub role: Role,
    /// Validity window from creation.
    pub ttl: Duration,
}

impl Invite {
    /// Render the ticket as a terminal QR code (unicode half blocks).
    ///
    /// # Errors
    ///
    /// Returns an error if the ticket is too large for a QR code.
    pub fn qr_terminal(&self) -> Result<String> {
        let code = QrCode::new(self.ticket.as_bytes())
            .map_err(|e| PeerError::Protocol(format!("QR encoding failed: {e}")))?;
        Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
    }
}

/// Why a redemption failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedeemError {
    Invalid,
    Expired,
    RoleMismatch,
}

struct Pending {
    secret: String,
    code: String,
    role: Role,
    expires: Instant,
    failed: u32,
}

/// Pending invites held in memory only (they never outlive the process).
#[derive(Default)]
pub(crate) struct InviteBook {
    pending: Mutex<Vec<Pending>>,
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl InviteBook {
    /// Create an invite; returns `(secret, code)`.
    pub(crate) fn create(&self, role: Role, ttl: Duration) -> (String, String) {
        // The iroh key generator is a CSPRNG; reuse it instead of another rand dependency.
        let secret = data_encoding::HEXLOWER.encode(&iroh::SecretKey::generate().to_bytes());
        let seed = iroh::SecretKey::generate().to_bytes();
        let n = u32::from_be_bytes([seed[0], seed[1], seed[2], seed[3]]);
        let code = format!("{:06}", n % 1_000_000);
        let now = Instant::now();
        let mut guard = self.pending.lock();
        guard.retain(|p| p.expires + Duration::from_secs(600) > now);
        guard.push(Pending {
            secret: secret.clone(),
            code: code.clone(),
            role,
            expires: now + ttl,
            failed: 0,
        });
        (secret, code)
    }

    /// Validate and consume an invite (single use).
    pub(crate) fn redeem(
        &self,
        secret: &str,
        code: &str,
        role: Role,
    ) -> std::result::Result<(), RedeemError> {
        let now = Instant::now();
        let mut guard = self.pending.lock();
        let Some(idx) = guard.iter().position(|p| ct_eq(&p.secret, secret)) else {
            // Unknown secret: an unauthenticated caller must not be able to
            // invalidate other people's invites, so leave the book untouched.
            // (The 256-bit secret is unguessable; the per-invite `failed`
            // counter below caps code guessing once the secret is known.)
            return Err(RedeemError::Invalid);
        };
        if guard[idx].expires <= now {
            guard.remove(idx);
            return Err(RedeemError::Expired);
        }
        if !ct_eq(&guard[idx].code, code) {
            guard[idx].failed += 1;
            if guard[idx].failed >= MAX_FAILED {
                guard.remove(idx);
            }
            return Err(RedeemError::Invalid);
        }
        if guard[idx].role != role {
            return Err(RedeemError::RoleMismatch);
        }
        guard.remove(idx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> EndpointAddr {
        EndpointAddr::new(iroh::SecretKey::generate().public())
    }

    #[test]
    fn ticket_round_trip() {
        let t = PairTicket {
            addr: addr(),
            secret: "ab".repeat(32),
            code: "123456".into(),
            role: Role::Node,
        };
        let s = t.encode();
        assert!(s.starts_with("bot-pair:"));
        assert_eq!(PairTicket::parse(&s).unwrap(), t);
        let lower = format!("bot-pair:{}", s["bot-pair:".len()..].to_ascii_lowercase());
        assert_eq!(PairTicket::parse(&lower).unwrap(), t);
    }

    #[test]
    fn garbage_tickets_fail() {
        assert!(PairTicket::parse("nope").is_err());
        assert!(PairTicket::parse("bot-pair:!!!").is_err());
        assert!(PairTicket::parse("bot-pair:MFRGG").is_err());
    }

    #[test]
    fn single_use() {
        let b = InviteBook::default();
        let (s, c) = b.create(Role::Node, INVITE_TTL);
        assert_eq!(b.redeem(&s, &c, Role::Node), Ok(()));
        assert_eq!(b.redeem(&s, &c, Role::Node), Err(RedeemError::Invalid));
    }

    #[test]
    fn expired_is_reported_and_removed() {
        let b = InviteBook::default();
        let (s, c) = b.create(Role::Node, Duration::ZERO);
        assert_eq!(b.redeem(&s, &c, Role::Node), Err(RedeemError::Expired));
        assert_eq!(b.redeem(&s, &c, Role::Node), Err(RedeemError::Invalid));
    }

    #[test]
    fn wrong_code_and_role_do_not_consume_until_cap() {
        let b = InviteBook::default();
        let (s, c) = b.create(Role::Node, INVITE_TTL);
        assert_eq!(
            b.redeem(&s, "000000x", Role::Node),
            Err(RedeemError::Invalid)
        );
        assert_eq!(
            b.redeem(&s, &c, Role::Phone),
            Err(RedeemError::RoleMismatch)
        );
        assert_eq!(b.redeem(&s, &c, Role::Node), Ok(()));
    }

    #[test]
    fn guessing_burns_the_invite() {
        let b = InviteBook::default();
        let (s, c) = b.create(Role::Node, INVITE_TTL);
        for _ in 0..MAX_FAILED {
            let _ = b.redeem(&s, "wrong!", Role::Node);
        }
        assert_eq!(b.redeem(&s, &c, Role::Node), Err(RedeemError::Invalid));
    }

    #[test]
    fn unknown_secret_does_not_destroy_live_invites() {
        let b = InviteBook::default();
        let (s, c) = b.create(Role::Node, INVITE_TTL);
        for _ in 0..(MAX_FAILED * 4) {
            assert_eq!(
                b.redeem("deadbeef", "000000", Role::Node),
                Err(RedeemError::Invalid)
            );
        }
        assert_eq!(b.redeem(&s, &c, Role::Node), Ok(()));
    }

    #[test]
    fn code_is_six_digits() {
        let b = InviteBook::default();
        let (_, c) = b.create(Role::Node, INVITE_TTL);
        assert_eq!(c.len(), 6);
        assert!(c.chars().all(|ch| ch.is_ascii_digit()));
    }
}
