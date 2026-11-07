//! Local TLS protocol servers. No external mail service is contacted.
#![allow(clippy::too_many_lines)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{ServerConfig, crypto::ring, pki_types::PrivatePkcs8KeyDer};

use crate::{AccountConfig, EmailConnector, ImapSettings, SmtpSecurity, SmtpSettings};

pub const PASSWORD: &str = "test-app-password";
pub const RAW: &[u8] = b"From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: Fixture\r\nDate: Sun, 8 Nov 2026 12:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello from fixture\r\n--B\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"a.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--B--\r\n";

pub struct Server {
    pub port: u16,
    pub pem: String,
    pub commands: Arc<Mutex<Vec<String>>>,
    pub messages: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Set when a FETCH response includes a body literal.
    pub body_literal_sent: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Copy)]
pub enum Protocol {
    Imap,
    Smtp,
    StartTls,
    NoStartTls,
    RejectLogin,
    Stall,
    /// `RFC822.SIZE` is over the fetch cap. A body literal sets `body_literal_sent`.
    Oversized,
    /// Login succeeds. The next command is `NO` and echoes the password.
    PasswordInNo,
}

pub async fn server(protocol: Protocol) -> Server {
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
    let commands = Arc::new(Mutex::new(Vec::new()));
    let messages = Arc::new(Mutex::new(Vec::new()));
    let body_literal_sent = Arc::new(AtomicBool::new(false));
    let log = commands.clone();
    let received = messages.clone();
    let body_flag = body_literal_sent.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            if matches!(protocol, Protocol::Stall) {
                std::future::pending::<()>().await;
            }
            if matches!(protocol, Protocol::StartTls | Protocol::NoStartTls) {
                let mut reader = BufReader::new(stream);
                reader
                    .get_mut()
                    .write_all(b"220 localhost ESMTP\r\n")
                    .await
                    .unwrap();
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                log.lock().unwrap().push(line.clone());
                if matches!(protocol, Protocol::NoStartTls) {
                    reader
                        .get_mut()
                        .write_all(b"250 localhost\r\n")
                        .await
                        .unwrap();
                    line.clear();
                    let _ = reader.read_line(&mut line).await;
                    log.lock().unwrap().push(line);
                    continue;
                }
                reader
                    .get_mut()
                    .write_all(b"250-localhost\r\n250 STARTTLS\r\n")
                    .await
                    .unwrap();
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                assert_eq!(line, "STARTTLS\r\n");
                log.lock().unwrap().push(line);
                reader
                    .get_mut()
                    .write_all(b"220 begin TLS\r\n")
                    .await
                    .unwrap();
                if let Ok(tls) = acceptor.accept(reader.into_inner()).await {
                    smtp(tls, &log, &received, false).await;
                }
            } else if let Ok(tls) = acceptor.accept(stream).await {
                if matches!(protocol, Protocol::Smtp) {
                    smtp(tls, &log, &received, true).await;
                } else {
                    imap(tls, &log, &received, protocol, &body_flag).await;
                }
            }
        }
    });
    Server {
        port,
        pem,
        commands,
        messages,
        body_literal_sent,
        task,
    }
}

fn fetch_uids(command: &str) -> Vec<u32> {
    let spec = command.split_whitespace().nth(2).unwrap_or("");
    let mut uids = Vec::new();
    for part in spec.split(',') {
        if part == "7" {
            uids.push(7);
        } else if part == "42" {
            uids.push(42);
        }
    }
    uids
}

async fn imap<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    log: &Mutex<Vec<String>>,
    received: &Mutex<Vec<Vec<u8>>>,
    protocol: Protocol,
    body_literal: &AtomicBool,
) {
    let reject = matches!(protocol, Protocol::RejectLogin);
    let mut reader = BufReader::new(stream);
    reader
        .get_mut()
        .write_all(b"* OK fixture IMAP\r\n")
        .await
        .unwrap();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        log.lock().unwrap().push(line.clone());
        let (tag, command) = line.trim_end().split_once(' ').unwrap();
        let response = if command.starts_with("LOGIN") {
            if reject {
                format!("{tag} NO authentication failed\r\n")
            } else {
                format!("{tag} OK authenticated\r\n")
            }
        } else if matches!(protocol, Protocol::PasswordInNo) && !command.starts_with("LOGOUT") {
            format!("{tag} NO marker-secret-echo {PASSWORD}\r\n")
        } else if command.starts_with("LIST") {
            format!(
                "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n* LIST (\\Drafts) \"/\" \"Drafts\"\r\n{tag} OK listed\r\n"
            )
        } else if command.starts_with("EXAMINE") || command.starts_with("SELECT") {
            format!(
                "* FLAGS (\\Seen \\Draft)\r\n* 2 EXISTS\r\n* OK [UIDVALIDITY 123] stable\r\n{tag} OK selected\r\n"
            )
        } else if command.starts_with("UID SEARCH") {
            format!("* SEARCH 7 42\r\n{tag} OK searched\r\n")
        } else if command.starts_with("UID FETCH") {
            if command.contains("999") {
                format!("{tag} OK absent\r\n")
            } else if matches!(protocol, Protocol::Oversized) {
                if command.contains("BODY") {
                    body_literal.store(true, Ordering::SeqCst);
                }
                format!("* 1 FETCH (UID 42 FLAGS () RFC822.SIZE 50000000)\r\n{tag} OK size\r\n")
            } else if !command.contains("BODY") {
                let mut data = String::new();
                for uid in fetch_uids(command) {
                    let _ = write!(
                        data,
                        "* 1 FETCH (UID {uid} FLAGS () RFC822.SIZE {})\r\n",
                        RAW.len()
                    );
                }
                let _ = write!(data, "{tag} OK size\r\n");
                data
            } else {
                let header = command.contains("HEADER");
                let bytes = if header {
                    &RAW[..RAW.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4]
                } else {
                    RAW
                };
                let section = if header { "HEADER" } else { "" };
                let mut data = Vec::new();
                body_literal.store(true, Ordering::SeqCst);
                for uid in fetch_uids(command) {
                    data.extend_from_slice(format!("* 1 FETCH (UID {uid} FLAGS () RFC822.SIZE {} BODY[{section}] {{{}}}\r\n", RAW.len(), bytes.len()).as_bytes());
                    data.extend_from_slice(bytes);
                    data.extend_from_slice(b")\r\n");
                }
                reader.get_mut().write_all(&data).await.unwrap();
                format!("{tag} OK fetched\r\n")
            }
        } else if command.starts_with("UID STORE") {
            format!("* 1 FETCH (UID 42 FLAGS (\\Seen))\r\n{tag} OK stored\r\n")
        } else if command.starts_with("APPEND") {
            let len: usize = command
                .rsplit_once('{')
                .unwrap()
                .1
                .trim_end_matches('}')
                .parse()
                .unwrap();
            reader.get_mut().write_all(b"+ ready\r\n").await.unwrap();
            let mut body = vec![0; len];
            reader.read_exact(&mut body).await.unwrap();
            received.lock().unwrap().push(body);
            let mut end = [0; 2];
            reader.read_exact(&mut end).await.unwrap();
            format!("{tag} OK appended\r\n")
        } else if command.starts_with("LOGOUT") {
            reader
                .get_mut()
                .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
                .await
                .unwrap();
            break;
        } else {
            panic!("unexpected command: {command}");
        };
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .unwrap();
    }
}

async fn smtp<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    log: &Mutex<Vec<String>>,
    received: &Mutex<Vec<Vec<u8>>>,
    greeting: bool,
) {
    let mut reader = BufReader::new(stream);
    if greeting {
        reader
            .get_mut()
            .write_all(b"220 localhost ESMTP\r\n")
            .await
            .unwrap();
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        log.lock().unwrap().push(line.clone());
        let response = if line.starts_with("EHLO") {
            "250-localhost\r\n250 AUTH PLAIN\r\n"
        } else if line.starts_with("AUTH PLAIN") {
            "235 authenticated\r\n"
        } else if line.starts_with("MAIL FROM:") || line.starts_with("RCPT TO:") {
            "250 accepted\r\n"
        } else if line == "DATA\r\n" {
            reader
                .get_mut()
                .write_all(b"354 send data\r\n")
                .await
                .unwrap();
            let mut data = Vec::new();
            loop {
                let mut body = String::new();
                assert!(reader.read_line(&mut body).await.unwrap() > 0);
                if body == ".\r\n" {
                    break;
                }
                data.extend_from_slice(body.as_bytes());
            }
            received.lock().unwrap().push(data);
            "250 queued fixture\r\n"
        } else if line == "QUIT\r\n" {
            reader.get_mut().write_all(b"221 bye\r\n").await.unwrap();
            break;
        } else {
            panic!("unexpected SMTP command: {line}");
        };
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .unwrap();
    }
}

pub fn account(imap: &Server, smtp: &Server, security: SmtpSecurity) -> AccountConfig {
    AccountConfig {
        name: "test".into(),
        address: "me@example.com".into(),
        username: String::new(),
        password: SecretRef::new("email/test").unwrap(),
        imap: ImapSettings {
            host: "localhost".into(),
            port: imap.port,
        },
        smtp: SmtpSettings {
            host: "localhost".into(),
            port: smtp.port,
            security,
        },
        drafts_folder: "Drafts".into(),
        extra_root_pem: Some(format!("{}{}", imap.pem, smtp.pem)),
        timeout_secs: 3,
    }
}

pub fn connector(config: AccountConfig) -> Arc<EmailConnector> {
    let store = Arc::new(MemoryStore::new());
    store.set("email/test", PASSWORD).unwrap();
    Arc::new(EmailConnector::new(vec![config], store).unwrap())
}
