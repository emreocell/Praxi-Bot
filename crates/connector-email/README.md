# connector-email

Standalone IMAP/SMTP tools for Praxi-Bot. Registration into `builtin-tools` is a
separate integration task.

Create `EmailConnector::new(accounts, Arc<dyn SecretStore>)`, wrap it in an
`Arc`, then call `connector_email::tools(&connector)` or
`connector_email::register(&registry, &connector)`. Registration returns the
number of new tools and leaves existing registrations intact.

| Tool | Effect | Inputs |
| --- | --- | --- |
| `email_list_folders` | Read | optional account |
| `email_search` | Read | account, folder, from, subject, since, before, unread, limit |
| `email_read` | Read | uid, account, folder, max_body_chars |
| `email_mark_read` | Write | uid, account, folder, read |
| `email_draft` | Write | account, to, cc, bcc, subject, body |
| `email_send` | Destructive | account, to, cc, bcc, subject, body |

All tools use `PermissionCategory::Messaging`. **The host must obtain approval
before invoking `email_send`.** As with builtin tools, a `ToolSpec` describes
effects; the tool registry itself is not an approval gate. Direct connector and
SMTP API callers are responsible for approval too. An unrestricted host policy
can waive approval; the future integration must enforce the desired send policy.

`AccountConfig.password` is a `SecretRef`, never a literal password. For example,
the serialized value `"email/work/app-password"` names a secret that the host
has stored in its `SecretStore`. Credentials are resolved lazily at connection
time. Unknown configuration/tool fields are rejected. IMAP requires implicit
TLS; SMTP supports implicit TLS or mandatory STARTTLS. Both verify certificate
chains and host names with Mozilla roots plus optional private PEM roots.
There is no insecure transport or certificate-verification switch.

The default folder is INBOX. Date criteria use `YYYY-MM-DD` and server received
dates: `since` is inclusive, `before` exclusive. All supplied criteria must
match. Search returns highest UIDs first, defaults to 20 results, and caps at
100. UIDs are scoped to an account/folder and can change after a UIDVALIDITY
reset. Reading uses EXAMINE and BODY.PEEK so it does not mark messages read.

Drafts are appended to the configured drafts folder with `\Draft` and `\Seen`;
they preserve Bcc and never contact SMTP. Sending uses a separate envelope so
Bcc recipients do not appear in transmitted headers. Outgoing mail is plain
text, with at most 50 recipients and a 1 MiB body. SMTP success means server
acceptance, not final delivery. There are no automatic send retries.

Full messages, including attachment bytes, are fetched and parsed in memory;
only decoded text and attachment metadata are returned. The text limit
(20,000 characters by default) is an output limit, not a download limit. Treat
all message content, headers, and attachment names as untrusted input.

Limitations: modified UTF-7 folder conversion, outgoing attachments, OAuth,
UIDVALIDITY tracking, connection pooling, streaming/download limits, Sent
folder copies, delivery receipts, and provider-specific behavior are not
implemented. Drafts require at least one recipient. Non-ASCII search relies on
server support for `CHARSET UTF-8`.

Dependencies use async-imap, mail-send, mail-builder, mail-parser and rustls
(ring provider); there is no OpenSSL/native-tls dependency. The TLS provider
uses ring's native crypto implementation. The new registry package licenses
were checked against `deny.toml`: all have an MIT, Apache-2.0 or ISC option.
`self_cell` is used under Apache-2.0. Lettre was replaced because its
quoted_printable dependency uses 0BSD, which this workspace does not allow.

Tests use in-process TLS servers bound to loopback with ephemeral ports and
generated certificates, plus MIME fixtures. They require no external network
or real email credentials.
