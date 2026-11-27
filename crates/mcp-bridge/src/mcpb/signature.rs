use super::{Result, err};

const START: &[u8] = b"MCPB_SIG_V1";
const END: &[u8] = b"MCPB_SIG_END";
const MAX_SIGNATURE: usize = 1024 * 1024;

pub(super) struct SignedData<'a> {
    pub signed_content: &'a [u8],
    pub der: &'a [u8],
}

/// Both appended signatures and signatures stored as the ZIP comment cover
/// everything before the start marker. The archive reader needs a complete
/// EOCD comment, so it sees the full file in the latter case.
pub(super) fn split(bytes: &[u8]) -> Result<(&[u8], Option<SignedData<'_>>)> {
    if let Some(trailer) = bytes.strip_suffix(END) {
        let tail_start = trailer
            .len()
            .saturating_sub(MAX_SIGNATURE + START.len() + 4);
        let candidates = trailer[tail_start..]
            .windows(START.len())
            .enumerate()
            .filter(|(_, marker)| *marker == START)
            .filter_map(|(offset, _)| {
                let offset = tail_start + offset;
                let (len, rest) = trailer[offset + START.len()..].split_first_chunk::<4>()?;
                let len = u32::from_le_bytes(*len) as usize;
                let (der, padding) = rest.split_at_checked(len)?;
                (!der.is_empty()).then_some((offset, der, padding))
            });
        // Bound repeated EOCD and padding checks for crafted trailers.
        for (offset, der, padding) in candidates.take(16) {
            let signed_content = &bytes[..offset];
            let archive = if padding.is_empty() && eocd_comment(signed_content, 0).is_some() {
                signed_content
            } else if eocd_comment(signed_content, bytes.len() - offset).is_some()
                && padding.iter().all(|&byte| byte == 0)
            {
                bytes
            } else {
                continue;
            };
            return Ok((
                archive,
                Some(SignedData {
                    signed_content,
                    der,
                }),
            ));
        }
    } else if eocd_comment(bytes, 0).is_some_and(|comment| !comment.starts_with(START)) {
        return Ok((bytes, None));
    }
    Err(err("the bundle ZIP or signature footer is malformed"))
}

fn eocd_comment(bytes: &[u8], trailing: usize) -> Option<&[u8]> {
    const EOCD: usize = 22;
    let tail = &bytes[bytes.len().saturating_sub(EOCD + usize::from(u16::MAX))..];
    tail.windows(EOCD)
        .enumerate()
        .rev()
        .find_map(|(offset, record)| {
            let comment = &tail[offset + EOCD..];
            (record.starts_with(b"PK\\x05\\x06")
                && comment.len() + trailing
                    == usize::from(u16::from_le_bytes([record[20], record[21]])))
            .then_some(comment)
        })
}

/// Check integrity against the embedded signer certificate(s). The certificates
/// themselves are supplied by the bundle and do not establish publisher trust.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn verify(zip: &[u8], der: &[u8]) -> Result<()> {
    use windows::Win32::Security::Cryptography::{
        CRYPT_VERIFY_MESSAGE_PARA, CryptGetMessageSignerCount, CryptVerifyDetachedMessageSignature,
        PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };

    let malformed = || err("the bundle signature is invalid");
    let encoding = PKCS_7_ASN_ENCODING.0 | X509_ASN_ENCODING.0;
    let parameters = CRYPT_VERIFY_MESSAGE_PARA {
        cbSize: u32::try_from(size_of::<CRYPT_VERIFY_MESSAGE_PARA>()).map_err(|_| malformed())?,
        dwMsgAndCertEncodingType: encoding,
        ..Default::default()
    };
    let size = u32::try_from(zip.len()).map_err(|_| malformed())?;
    // SAFETY: both slices and the one-element pointer/length arrays are live
    // for these synchronous calls. Crypt32 does not return a certificate handle.
    unsafe {
        let signers =
            u32::try_from(CryptGetMessageSignerCount(encoding, der)).map_err(|_| malformed())?;
        if !(1..=32).contains(&signers) {
            return Err(malformed());
        }
        for signer in 0..signers {
            CryptVerifyDetachedMessageSignature(
                &raw const parameters,
                signer,
                der,
                1,
                [zip.as_ptr()].as_ptr(),
                [size].as_ptr(),
                None,
            )
            .map_err(|_| malformed())?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn verify(zip: &[u8], der: &[u8]) -> Result<()> {
    use std::{
        io::Write as _,
        path::Path,
        process::{Command, Stdio},
    };

    // Use installed system tools, never an executable from the archive, the
    // current directory, or a caller-provided PATH.
    let openssl = [
        "/opt/homebrew/opt/openssl@3/bin/openssl",
        "/usr/local/opt/openssl@3/bin/openssl",
        "/usr/bin/openssl",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .ok_or_else(|| err("OpenSSL with CMS support is required to check the bundle signature"))?;
    let io = |_| err("could not check the bundle signature");
    let mut content = tempfile::NamedTempFile::new().map_err(io)?;
    content.write_all(zip).map_err(io)?;
    let mut signature = tempfile::NamedTempFile::new().map_err(io)?;
    signature.write_all(der).map_err(io)?;
    let verified = Command::new(openssl)
        .args([
            "cms",
            "-verify",
            "-binary",
            "-inform",
            "DER",
            "-noverify",
            "-in",
        ])
        .arg(signature.path())
        .arg("-content")
        .arg(content.path())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(io)?;
    if !verified.success() {
        return Err(err(
            "the bundle signature is invalid or OpenSSL lacks CMS support",
        ));
    }
    Ok(())
}
