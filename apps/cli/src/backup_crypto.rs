//! Passphrase-encrypted, chunked container for backup streams.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! header  : magic "PRAXIBAK" | version u16 | m_cost u32 | t_cost u32 | p_cost u32
//!           | salt [16] | chunk_size u32                      (42 bytes)
//! chunk*  : ciphertext_len u32 | ChaCha20-Poly1305 ciphertext (plaintext + 16-byte tag)
//! ```
//!
//! The key is derived with Argon2id from the passphrase and the random salt, so
//! every file has its own key and a nonce never repeats under one key. Chunk
//! `i` is sealed with nonce `00 00 00 <last> | i as u64 big-endian`, where
//! `<last>` is 1 only for the final chunk, and with the entire header as
//! associated data. That makes the stream fail closed:
//!
//! * a changed header byte (including the KDF parameters or salt) changes the key
//!   or the associated data, so the first chunk no longer authenticates;
//! * a flipped, reordered or dropped chunk breaks the counter-bound nonce;
//! * a truncated file has no chunk sealed as `last`, and data appended after the
//!   final chunk is rejected.
//!
//! A wrong passphrase, a damaged file and a truncated file are deliberately
//! reported with one and the same message. Plaintext chunks are only handed out
//! after they authenticated, and key material is zeroized on drop.

use std::io::{self, Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, OsRng, Payload, rand_core::RngCore},
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"PRAXIBAK";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 8 + 2 + 12 + 16 + 4;
const SALT_LEN: usize = 16;
const TAG_LEN: usize = 16;
/// Plaintext bytes per chunk.
pub const CHUNK_SIZE: usize = 64 * 1024;
const MAX_CHUNK_SIZE: u32 = 1024 * 1024;
// Upper bounds for KDF parameters read from an untrusted header.
const MAX_M_COST_KIB: u32 = 1024 * 1024;
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 16;

/// Argon2id cost parameters stored in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kdf {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Kdf {
    /// 64 MiB, 3 passes, 1 lane (above the OWASP minimum; a backup is created rarely).
    fn default() -> Self {
        Self {
            m_cost: 64 * 1024,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// Why a container could not be opened or read.
#[derive(Debug)]
pub enum SealError {
    NotABackup,
    UnsupportedVersion(u16),
    BadHeader,
    /// Wrong passphrase, flipped bytes, reordering or truncation: indistinguishable on purpose.
    WrongPassphraseOrDamaged,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotABackup => f.write_str("this file is not an encrypted Praxi backup"),
            Self::UnsupportedVersion(v) => write!(
                f,
                "this backup uses container version {v}, which this version of Praxi cannot read; update Praxi"
            ),
            Self::BadHeader => f.write_str("the backup header is invalid"),
            Self::WrongPassphraseOrDamaged => f.write_str(
                "wrong passphrase, or the backup file is damaged or incomplete (nothing was changed)",
            ),
        }
    }
}

impl std::error::Error for SealError {}

fn seal_err(e: SealError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

fn nonce(counter: u64, last: bool) -> Nonce {
    let mut n = [0u8; 12];
    n[3] = u8::from(last);
    n[4..].copy_from_slice(&counter.to_be_bytes());
    *Nonce::from_slice(&n)
}

fn derive(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<ChaCha20Poly1305, SealError> {
    let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|_| SealError::BadHeader)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| SealError::BadHeader)?;
    Ok(ChaCha20Poly1305::new(Key::from_slice(key.as_ref())))
}

/// Encrypting writer. Call [`SealWriter::finish`]; a dropped writer leaves a
/// file that fails verification (no final chunk).
pub struct SealWriter<W: Write> {
    out: W,
    cipher: ChaCha20Poly1305,
    header: [u8; HEADER_LEN],
    buf: Zeroizing<Vec<u8>>,
    counter: u64,
}

impl<W: Write> SealWriter<W> {
    /// Write the header and derive the key.
    ///
    /// # Errors
    /// I/O errors or invalid KDF parameters.
    pub fn new(mut out: W, passphrase: &str, kdf: Kdf) -> io::Result<Self> {
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(MAGIC);
        header[8..10].copy_from_slice(&VERSION.to_le_bytes());
        header[10..14].copy_from_slice(&kdf.m_cost.to_le_bytes());
        header[14..18].copy_from_slice(&kdf.t_cost.to_le_bytes());
        header[18..22].copy_from_slice(&kdf.p_cost.to_le_bytes());
        header[22..38].copy_from_slice(&salt);
        header[38..42].copy_from_slice(&(CHUNK_SIZE as u32).to_le_bytes());
        let cipher = derive(passphrase, &salt, kdf).map_err(seal_err)?;
        out.write_all(&header)?;
        Ok(Self {
            out,
            cipher,
            header,
            buf: Zeroizing::new(Vec::with_capacity(CHUNK_SIZE)),
            counter: 0,
        })
    }

    fn seal(&mut self, plain: &[u8], last: bool) -> io::Result<()> {
        let ct = self
            .cipher
            .encrypt(
                &nonce(self.counter, last),
                Payload {
                    msg: plain,
                    aad: &self.header,
                },
            )
            .map_err(|_| io::Error::other("encryption failed"))?;
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| io::Error::other("backup too large"))?;
        let len = u32::try_from(ct.len()).map_err(|_| io::Error::other("chunk too large"))?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(&ct)
    }

    /// Seal the remaining bytes as the final chunk and return the sink.
    ///
    /// # Errors
    /// I/O errors.
    pub fn finish(mut self) -> io::Result<W> {
        let rest = Zeroizing::new(self.buf.to_vec());
        self.seal(&rest, true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        // Keep at least one byte back so `finish` always seals a non-empty tail
        // unless the stream was empty; full chunks are emitted eagerly.
        while self.buf.len() > CHUNK_SIZE {
            let chunk = Zeroizing::new(self.buf.drain(..CHUNK_SIZE).collect::<Vec<u8>>());
            self.seal(&chunk, false)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decrypting reader. Yields only authenticated plaintext; returns an error
/// instead of EOF when the final chunk is missing.
pub struct SealReader<R: Read> {
    src: R,
    cipher: ChaCha20Poly1305,
    header: [u8; HEADER_LEN],
    chunk_size: usize,
    plain: Zeroizing<Vec<u8>>,
    pos: usize,
    counter: u64,
    done: bool,
}

impl<R: Read> SealReader<R> {
    /// Read the header, derive the key and authenticate the first chunk, so a
    /// wrong passphrase fails before any plaintext is produced.
    ///
    /// # Errors
    /// [`SealError`] (wrapped in `io::Error`) or I/O errors.
    pub fn open(mut src: R, passphrase: &str) -> io::Result<Self> {
        let mut header = [0u8; HEADER_LEN];
        read_full(&mut src, &mut header).map_err(|_| seal_err(SealError::NotABackup))?;
        if &header[..8] != MAGIC {
            return Err(seal_err(SealError::NotABackup));
        }
        let version = u16::from_le_bytes([header[8], header[9]]);
        if version != VERSION {
            return Err(seal_err(SealError::UnsupportedVersion(version)));
        }
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap_or([0; 4]));
        let kdf = Kdf {
            m_cost: word(10),
            t_cost: word(14),
            p_cost: word(18),
        };
        let chunk_size = word(38);
        if kdf.m_cost == 0
            || kdf.m_cost > MAX_M_COST_KIB
            || kdf.t_cost == 0
            || kdf.t_cost > MAX_T_COST
            || kdf.p_cost == 0
            || kdf.p_cost > MAX_P_COST
            || chunk_size == 0
            || chunk_size > MAX_CHUNK_SIZE
        {
            return Err(seal_err(SealError::BadHeader));
        }
        let cipher = derive(passphrase, &header[22..38], kdf).map_err(seal_err)?;
        let mut reader = Self {
            src,
            cipher,
            header,
            chunk_size: chunk_size as usize,
            plain: Zeroizing::new(Vec::new()),
            pos: 0,
            counter: 0,
            done: false,
        };
        reader.next_chunk()?;
        Ok(reader)
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let damaged = || seal_err(SealError::WrongPassphraseOrDamaged);
        let mut len = [0u8; 4];
        read_full(&mut self.src, &mut len).map_err(|_| damaged())?;
        let len = u32::from_le_bytes(len) as usize;
        if !(TAG_LEN..=self.chunk_size + TAG_LEN).contains(&len) {
            return Err(damaged());
        }
        let mut ct = vec![0u8; len];
        read_full(&mut self.src, &mut ct).map_err(|_| damaged())?;
        let aad = &self.header[..];
        let mut plain = None;
        // Only a full-size chunk can be a non-final chunk.
        if len == self.chunk_size + TAG_LEN {
            plain = self
                .cipher
                .decrypt(&nonce(self.counter, false), Payload { msg: &ct, aad })
                .ok();
        }
        if let Some(p) = plain {
            self.plain = Zeroizing::new(p);
        } else {
            let p = self
                .cipher
                .decrypt(&nonce(self.counter, true), Payload { msg: &ct, aad })
                .map_err(|_| damaged())?;
            self.plain = Zeroizing::new(p);
            self.done = true;
            // Nothing may follow the final chunk.
            let mut probe = [0u8; 1];
            if self.src.read(&mut probe)? != 0 {
                return Err(damaged());
            }
        }
        self.pos = 0;
        self.counter += 1;
        Ok(())
    }
}

impl<R: Read> Read for SealReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.plain.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = out.len().min(self.plain.len() - self.pos);
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn read_full(src: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    src.read_exact(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Kdf = Kdf {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };

    fn seal(data: &[u8]) -> Vec<u8> {
        let mut w = SealWriter::new(Vec::new(), "pass-1234", FAST).unwrap();
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    fn open_all(bytes: &[u8], pass: &str) -> io::Result<Vec<u8>> {
        let mut r = SealReader::open(bytes, pass)?;
        let mut out = Vec::new();
        r.read_to_end(&mut out)?;
        Ok(out)
    }

    fn sample() -> Vec<u8> {
        (0..CHUNK_SIZE * 3 + 123).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn round_trips_empty_small_and_multi_chunk_data() {
        for data in [Vec::new(), b"hi".to_vec(), vec![7; CHUNK_SIZE], sample()] {
            assert_eq!(open_all(&seal(&data), "pass-1234").unwrap(), data);
        }
    }

    #[test]
    fn wrong_passphrase_and_every_tamper_fail_with_the_same_message() {
        let data = sample();
        let sealed = seal(&data);
        let want = SealError::WrongPassphraseOrDamaged.to_string();
        let msg = |r: io::Result<Vec<u8>>| r.unwrap_err().to_string();
        assert_eq!(msg(open_all(&sealed, "other-pass")), want);
        // Flip one byte in the KDF params, the salt, the middle and the tail.
        for at in [11, 25, sealed.len() / 2, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[at] ^= 1;
            assert_eq!(msg(open_all(&bad, "pass-1234")), want, "byte {at}");
        }
        // Truncation at several points, including exactly on a chunk boundary.
        let first_chunk = HEADER_LEN + 4 + CHUNK_SIZE + TAG_LEN;
        for cut in [HEADER_LEN + 2, first_chunk, sealed.len() - 1] {
            assert_eq!(
                msg(open_all(&sealed[..cut], "pass-1234")),
                want,
                "cut {cut}"
            );
        }
        // Appended data after the final chunk.
        let mut longer = sealed.clone();
        longer.push(0);
        assert_eq!(msg(open_all(&longer, "pass-1234")), want);
    }

    #[test]
    fn reordered_chunks_are_rejected() {
        let sealed = seal(&sample());
        let step = 4 + CHUNK_SIZE + TAG_LEN;
        let mut bad = sealed.clone();
        let (a, b) = (HEADER_LEN, HEADER_LEN + step);
        let first = bad[a..a + step].to_vec();
        let second = bad[b..b + step].to_vec();
        bad[a..a + step].copy_from_slice(&second);
        bad[b..b + step].copy_from_slice(&first);
        assert!(open_all(&bad, "pass-1234").is_err());
        // Dropping a middle chunk also fails.
        let mut dropped = sealed[..a].to_vec();
        dropped.extend_from_slice(&sealed[a + step..]);
        assert!(open_all(&dropped, "pass-1234").is_err());
    }

    #[test]
    fn plaintext_never_appears_in_the_container_and_junk_is_not_a_backup() {
        let marker = b"PLAINTEXT-MARKER-0123456789";
        let mut data = marker.repeat(50);
        data.extend_from_slice(&sample());
        let sealed = seal(&data);
        assert!(!sealed.windows(marker.len()).any(|w| w == marker));
        let err = SealReader::open(&b"definitely not a backup, just text"[..], "x")
            .err()
            .unwrap();
        assert_eq!(err.to_string(), SealError::NotABackup.to_string());
    }

    #[test]
    fn absurd_kdf_parameters_in_a_header_are_refused_before_hashing() {
        let mut sealed = seal(b"x");
        sealed[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = SealReader::open(&sealed[..], "pass-1234").err().unwrap();
        assert_eq!(err.to_string(), SealError::BadHeader.to_string());
    }
}
