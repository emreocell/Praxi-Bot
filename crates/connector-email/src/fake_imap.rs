//! A small stateful IMAP server over TLS for tests: several folders with real
//! messages, `LIST` with special-use attributes, `UID SEARCH` that really
//! filters (`FROM`, `SUBJECT`, `BODY`, `UNSEEN`, `SEEN`), `UID FETCH` of sizes,
//! headers and whole messages, and mail that can be delivered while a client is
//! already waiting. No external mail service is contacted.
#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::unused_self
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{ServerConfig, crypto::ring, pki_types::PrivatePkcs8KeyDer};

use crate::{AccountConfig, EmailConnector, IdentitySettings, ImapSettings, SmtpSettings};

pub const PASSWORD: &str = "fake-imap-password";

/// One stored message.
#[derive(Clone)]
pub struct Stored {
    pub uid: u32,
    pub raw: Vec<u8>,
    pub seen: bool,
    /// The `RFC822.SIZE` the server claims, when it should differ from the real size (a
    /// server that lies about how much it is about to send).
    pub reported_size: Option<usize>,
}

type Folders = Arc<Mutex<BTreeMap<String, Vec<Stored>>>>;

pub struct FakeImap {
    pub port: u16,
    pub pem: String,
    pub folders: Folders,
    attributes: Arc<Mutex<BTreeMap<String, String>>>,
    task: JoinHandle<()>,
}

impl Drop for FakeImap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A plain-text mail.
pub fn text_mail(from: &str, subject: &str, message_id: &str, date: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: bot@example.com\r\nSubject: {subject}\r\nMessage-ID: <{message_id}>\r\nDate: {date}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// An HTML mail with a plain-text twin.
pub fn html_mail(from: &str, subject: &str, message_id: &str, date: &str, html: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: bot@example.com\r\nSubject: {subject}\r\nMessage-ID: <{message_id}>\r\nDate: {date}\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n"
    )
    .into_bytes()
}

/// A mail with one text attachment (`notes.txt`).
pub fn mail_with_attachment(subject: &str, date: &str) -> Vec<u8> {
    format!(
        "From: Ann <ann@example.com>\r\nTo: bot@example.com\r\nSubject: {subject}\r\nMessage-ID: <att1@example.com>\r\nDate: {date}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee the attachment.\r\n--B\r\nContent-Type: text/plain; name=\"notes.txt\"\r\nContent-Disposition: attachment; filename=\"notes.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8gYXR0YWNobWVudA==\r\n--B--\r\n"
    )
    .into_bytes()
}

/// A mail with one attachment of the given name, declared type and bytes.
pub fn attachment_mail(
    subject: &str,
    date: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Vec<u8> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!(
        "From: Ann <ann@example.com>\r\nTo: bot@example.com\r\nSubject: {subject}\r\nMessage-ID: <att2@example.com>\r\nDate: {date}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee the attachment.\r\n--B\r\nContent-Type: {content_type}\r\nContent-Disposition: attachment; filename=\"{filename}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{b64}\r\n--B--\r\n"
    )
    .into_bytes()
}

/// RFC 2822 date `secs_ago` seconds before now.
pub fn date_ago(secs_ago: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let t = now - secs_ago;
    let days = i64::try_from(t / 86_400).unwrap();
    let rem = t % 86_400;
    // civil-from-days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][usize::try_from(days % 7).unwrap()];
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][usize::try_from(m - 1).unwrap()];
    format!(
        "{wd}, {d} {mon} {y} {:02}:{:02}:{:02} +0000",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl FakeImap {
    /// Start with these folders: `(name, special-use attribute)`.
    pub async fn start(folders: &[(&str, Option<&str>)]) -> Self {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let pem = cert.pem();
        let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let store: Folders = Arc::new(Mutex::new(
            folders
                .iter()
                .map(|(n, _)| ((*n).to_owned(), Vec::new()))
                .collect(),
        ));
        let attributes = Arc::new(Mutex::new(
            folders
                .iter()
                .filter_map(|(n, a)| a.map(|a| ((*n).to_owned(), a.to_owned())))
                .collect::<BTreeMap<_, _>>(),
        ));
        let (st, at) = (store.clone(), attributes.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let (st, at) = (st.clone(), at.clone());
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(stream).await {
                        session(tls, st, at).await;
                    }
                });
            }
        });
        Self {
            port,
            pem,
            folders: store,
            attributes,
            task,
        }
    }

    /// Put a message into `folder`; returns its uid.
    pub fn deliver(&self, folder: &str, raw: Vec<u8>) -> u32 {
        let mut all = self.folders.lock().unwrap();
        let list = all.get_mut(folder).expect("folder exists");
        let uid = list.iter().map(|m| m.uid).max().unwrap_or(0) + 1;
        list.push(Stored {
            uid,
            raw,
            seen: false,
            reported_size: None,
        });
        uid
    }

    #[allow(dead_code)]
    pub fn attribute_of(&self, folder: &str) -> Option<String> {
        self.attributes.lock().unwrap().get(folder).cloned()
    }

    /// An account for this server (SMTP points nowhere unless `smtp_port` is given).
    pub fn account(
        &self,
        name: &str,
        bot: Option<&str>,
        smtp_port: u16,
        pem: &str,
    ) -> AccountConfig {
        AccountConfig {
            name: name.into(),
            address: "bot@example.com".into(),
            username: String::new(),
            password: SecretRef::new(format!("email/{name}/password")).unwrap(),
            imap: ImapSettings {
                host: "localhost".into(),
                port: self.port,
            },
            smtp: SmtpSettings {
                host: "localhost".into(),
                port: smtp_port,
                security: crate::SmtpSecurity::Implicit,
            },
            drafts_folder: "Drafts".into(),
            extra_root_pem: Some(format!("{}{pem}", self.pem)),
            timeout_secs: 5,
            bot: bot.map(str::to_owned),
            allowed_senders: Vec::new(),
            identity: IdentitySettings::default(),
            kind: crate::AccountKind::ImapSmtp,
            inbox_id: None,
        }
    }

    /// A connector that owns `account` (password stored).
    pub fn connector(&self, account: AccountConfig) -> Arc<EmailConnector> {
        let store = Arc::new(MemoryStore::new());
        store.set(account.password.name(), PASSWORD).unwrap();
        let c = EmailConnector::reloadable(store);
        c.replace_accounts(vec![account]).unwrap();
        Arc::new(c)
    }
}

/// Quoted arguments of `keyword` in an IMAP search expression.
fn quoted_after(command: &str, keyword: &str) -> Option<String> {
    let upper = command.to_ascii_uppercase();
    let at = upper.find(&format!("{keyword} \""))? + keyword.len() + 2;
    let rest = &command[at..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

fn split_mail(raw: &[u8]) -> (String, String) {
    let text = String::from_utf8_lossy(raw).into_owned();
    match text.split_once("\r\n\r\n") {
        Some((h, b)) => (h.to_owned(), b.to_owned()),
        None => (text, String::new()),
    }
}

fn matches(command: &str, m: &Stored) -> bool {
    let (head, body) = split_mail(&m.raw);
    let header_has = |name: &str, needle: &str| {
        head.lines()
            .filter(|l| {
                l.to_ascii_lowercase()
                    .starts_with(&format!("{}:", name.to_lowercase()))
            })
            .any(|l| l.to_lowercase().contains(&needle.to_lowercase()))
    };
    if let Some(v) = quoted_after(command, "FROM")
        && !header_has("from", &v)
    {
        return false;
    }
    if let Some(v) = quoted_after(command, "SUBJECT")
        && !header_has("subject", &v)
    {
        return false;
    }
    if let Some(v) = quoted_after(command, "BODY")
        && !body.to_lowercase().contains(&v.to_lowercase())
    {
        return false;
    }
    let upper = command.to_ascii_uppercase();
    if upper.contains("UNSEEN") && m.seen {
        return false;
    }
    if upper.split_whitespace().any(|t| t == "SEEN") && !m.seen {
        return false;
    }
    true
}

fn uids_of(spec: &str, all: &[Stored]) -> Vec<u32> {
    spec.split(',')
        .filter_map(|p| p.parse::<u32>().ok())
        .filter(|u| all.iter().any(|m| m.uid == *u))
        .collect()
}

async fn session<S: AsyncReadExt + AsyncWrite + Unpin>(
    stream: S,
    store: Folders,
    attributes: Arc<Mutex<BTreeMap<String, String>>>,
) {
    let mut reader = BufReader::new(stream);
    reader
        .get_mut()
        .write_all(b"* OK fake IMAP\r\n")
        .await
        .unwrap();
    let mut current = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let Some((tag, command)) = line.trim_end().split_once(' ') else {
            break;
        };
        let upper = command.to_ascii_uppercase();
        let mut out: Vec<u8> = Vec::new();
        if upper.starts_with("LOGIN") {
            out.extend(format!("{tag} OK authenticated\r\n").as_bytes());
        } else if upper.starts_with("LIST") {
            let attrs = attributes.lock().unwrap().clone();
            for name in store.lock().unwrap().keys() {
                let a = attrs.get(name).cloned().unwrap_or_default();
                out.extend(format!("* LIST ({a}) \"/\" \"{name}\"\r\n").as_bytes());
            }
            out.extend(format!("{tag} OK listed\r\n").as_bytes());
        } else if upper.starts_with("EXAMINE") || upper.starts_with("SELECT") {
            let name = command
                .split_once(' ')
                .map(|(_, n)| n.trim().trim_matches('"').to_owned())
                .unwrap_or_default();
            let count = store.lock().unwrap().get(&name).map(Vec::len);
            match count {
                Some(n) => {
                    current = name;
                    out.extend(
                        format!(
                            "* FLAGS (\\Seen)\r\n* {n} EXISTS\r\n* OK [UIDVALIDITY 7] stable\r\n{tag} OK selected\r\n"
                        )
                        .as_bytes(),
                    );
                }
                None => out.extend(format!("{tag} NO no such mailbox\r\n").as_bytes()),
            }
        } else if upper.starts_with("UID SEARCH") {
            let all = store
                .lock()
                .unwrap()
                .get(&current)
                .cloned()
                .unwrap_or_default();
            let hits: Vec<String> = all
                .iter()
                .filter(|m| matches(command, m))
                .map(|m| m.uid.to_string())
                .collect();
            out.extend(format!("* SEARCH {}\r\n{tag} OK searched\r\n", hits.join(" ")).as_bytes());
        } else if upper.starts_with("UID FETCH") {
            let spec = command.split_whitespace().nth(2).unwrap_or("");
            let all = store
                .lock()
                .unwrap()
                .get(&current)
                .cloned()
                .unwrap_or_default();
            for uid in uids_of(spec, &all) {
                let m = all.iter().find(|m| m.uid == uid).unwrap();
                let flags = if m.seen { "\\Seen" } else { "" };
                let size = m.reported_size.unwrap_or(m.raw.len());
                if upper.contains("BODY.PEEK[HEADER]") {
                    let (head, _) = split_mail(&m.raw);
                    let header = format!("{head}\r\n\r\n");
                    out.extend(format!("* 1 FETCH (UID {uid} FLAGS ({flags}) RFC822.SIZE {size} BODY[HEADER] {{{}}}\r\n", header.len()).as_bytes());
                    out.extend(header.as_bytes());
                    out.extend(b" BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL))\r\n");
                } else if upper.contains("BODY.PEEK[]") {
                    out.extend(format!("* 1 FETCH (UID {uid} FLAGS ({flags}) RFC822.SIZE {size} BODY[] {{{}}}\r\n", m.raw.len()).as_bytes());
                    out.extend(&m.raw);
                    out.extend(b")\r\n");
                } else {
                    out.extend(
                        format!("* 1 FETCH (UID {uid} FLAGS ({flags}) RFC822.SIZE {size})\r\n")
                            .as_bytes(),
                    );
                }
            }
            out.extend(format!("{tag} OK fetched\r\n").as_bytes());
        } else if upper.starts_with("UID STORE") {
            let spec = command.split_whitespace().nth(2).unwrap_or("");
            let seen = upper.contains("+FLAGS");
            if let Some(list) = store.lock().unwrap().get_mut(&current) {
                for uid in spec.split(',').filter_map(|p| p.parse::<u32>().ok()) {
                    if let Some(m) = list.iter_mut().find(|m| m.uid == uid) {
                        m.seen = seen;
                    }
                }
            }
            out.extend(format!("{tag} OK stored\r\n").as_bytes());
        } else if upper.starts_with("LOGOUT") {
            reader
                .get_mut()
                .write_all(format!("* BYE bye\r\n{tag} OK logout\r\n").as_bytes())
                .await
                .unwrap();
            break;
        } else {
            out.extend(format!("{tag} BAD unsupported\r\n").as_bytes());
        }
        if reader.get_mut().write_all(&out).await.is_err() {
            break;
        }
    }
}
