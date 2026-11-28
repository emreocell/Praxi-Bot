//! Delivery status notifications (RFC 3464), read into plain fields.
//!
//! A bounce says which recipient failed (`Original-Recipient` / `Final-Recipient`), how
//! (`Action`, `Status`) and quotes the headers of the mail it is about, among them the
//! `Message-ID` we wrote ourselves. The text is untrusted: it is only ever used to find the
//! matching entry of the send log, never to name an address the mail did not go to.

/// Most field blocks read from one report.
const MAX_BLOCKS: usize = 50;
/// Longest line read.
const MAX_LINE: usize = 1000;
/// Most lines read from one report, blank or not.
const MAX_LINES: usize = 2000;
/// Most bytes of text read from one report.
const MAX_BYTES: usize = 64 * 1024;
/// Most `Name: value` fields kept in one block (a real one has a handful).
const MAX_FIELDS: usize = 40;

/// One recipient block of a delivery report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnRecipient {
    /// `Original-Recipient`, bare, lower-case.
    pub original_recipient: Option<String>,
    /// `Final-Recipient`, bare, lower-case.
    pub final_recipient: Option<String>,
    /// `Action`, lower-case (`failed`, `delayed`, ...).
    pub action: String,
    /// `Status` code (`5.1.1`).
    pub status: String,
}

impl DsnRecipient {
    /// The address the report is about: the final recipient, else the original one.
    #[must_use]
    pub fn address(&self) -> Option<&str> {
        self.final_recipient
            .as_deref()
            .or(self.original_recipient.as_deref())
    }

    /// The report says delivery failed for good.
    #[must_use]
    pub fn is_permanent_failure(&self) -> bool {
        self.action == "failed" && self.status.starts_with("5.")
    }

    /// The report says delivery is delayed or failed temporarily.
    #[must_use]
    pub fn is_temporary(&self) -> bool {
        self.action == "delayed" && self.status.starts_with("4.")
    }
}

/// What a delivery report says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryStatus {
    /// One entry per recipient block.
    pub recipients: Vec<DsnRecipient>,
    /// `Message-ID` of the mail the report is about, from the returned headers.
    pub original_message_id: Option<String>,
    /// The message was a `multipart/report` with a `message/delivery-status` part (`false`: the
    /// fields were read from ordinary body text, which anyone can write). Even a structured
    /// report proves nothing about who sent it.
    pub structured: bool,
}

fn bare(value: &str) -> Option<String> {
    // `rfc822; user@example.com` (the address type comes first).
    let v = value.rsplit(';').next().unwrap_or(value);
    let v = v.trim().trim_matches(['<', '>']).trim().to_lowercase();
    (!v.is_empty() && v.len() <= 320 && !v.contains(char::is_whitespace)).then_some(v)
}

impl DeliveryStatus {
    /// Read the fields out of `text` (the `message/delivery-status` part followed by the
    /// returned headers, or a whole body). `None` when no block names a recipient.
    #[must_use]
    pub fn parse(text: &str, structured: bool) -> Option<Self> {
        let mut recipients = Vec::new();
        let mut original_message_id = None;
        let mut block: Vec<(String, String)> = Vec::new();
        let mut flush = |block: &mut Vec<(String, String)>| {
            if block.is_empty() {
                return;
            }
            let get = |name: &str| {
                block
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
            };
            if get("final-recipient").is_some() || get("original-recipient").is_some() {
                recipients.push(DsnRecipient {
                    original_recipient: get("original-recipient").and_then(bare),
                    final_recipient: get("final-recipient").and_then(bare),
                    action: get("action")
                        .and_then(|a| a.split_whitespace().next())
                        .unwrap_or_default()
                        .to_lowercase(),
                    status: get("status")
                        .and_then(|a| a.split_whitespace().next())
                        .unwrap_or_default()
                        .to_owned(),
                });
            } else if original_message_id.is_none()
                && get("reporting-mta").is_none()
                && let Some(id) = get("message-id")
            {
                // The returned headers of the mail we sent.
                original_message_id = bare(id);
            }
            block.clear();
        };
        let mut blocks = 0;
        let mut bytes = 0;
        for line in text.lines().take(MAX_LINES) {
            bytes += line.len() + 1;
            if bytes > MAX_BYTES {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                flush(&mut block);
                blocks += 1;
                if blocks >= MAX_BLOCKS {
                    break;
                }
                continue;
            }
            if line.starts_with('>') || line.starts_with([' ', '\t']) || line.len() > MAX_LINE {
                continue;
            }
            // One block never grows without bound: later fields of it are dropped.
            if block.len() < MAX_FIELDS
                && let Some((name, value)) = line.split_once(':')
            {
                block.push((name.trim().to_lowercase(), value.trim().to_owned()));
            }
        }
        flush(&mut block);
        (!recipients.is_empty()).then_some(Self {
            recipients,
            original_message_id,
            structured,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "Reporting-MTA: dns; mx.example.net\n\nFinal-Recipient: rfc822; Ann@Example.com\nOriginal-Recipient: rfc822;ann@example.com\nAction: failed\nStatus: 5.1.1\nDiagnostic-Code: smtp; 550 no such user\n\nReceived: from x\nMessage-ID: <abc123@mail.example.org>\nSubject: Hi\n";

    #[test]
    fn reads_the_recipient_and_the_original_message_id() {
        let d = DeliveryStatus::parse(REPORT, true).unwrap();
        assert_eq!(d.recipients.len(), 1);
        let r = &d.recipients[0];
        assert_eq!(r.address(), Some("ann@example.com"));
        assert!(r.is_permanent_failure() && !r.is_temporary());
        assert_eq!(
            d.original_message_id.as_deref(),
            Some("abc123@mail.example.org")
        );
        assert!(d.structured);
    }

    #[test]
    fn each_recipient_keeps_its_own_action_and_status() {
        let text = "Final-Recipient: rfc822; a@x.com\nAction: failed\nStatus: 4.2.0\n\nFinal-Recipient: rfc822; b@x.com\nAction: delayed\nStatus: 5.1.1\n";
        let d = DeliveryStatus::parse(text, false).unwrap();
        assert!(!d.recipients[0].is_permanent_failure());
        assert!(!d.recipients[0].is_temporary());
        assert!(!d.recipients[1].is_permanent_failure());
        assert!(!d.recipients[1].is_temporary());
    }

    #[test]
    fn one_block_and_the_whole_text_are_bounded() {
        // A single block of a million header lines is cut short, the recipient before it kept.
        let mut text =
            String::from("Final-Recipient: rfc822; a@x.com\nAction: failed\nStatus: 5.1.1\n");
        text.push_str(&"X-Filler: y\n".repeat(100_000));
        let d = DeliveryStatus::parse(&text, true).unwrap();
        assert_eq!(d.recipients.len(), 1);
        assert!(d.recipients[0].is_permanent_failure());
        // The block never holds more than the field cap, so a field past it is not seen.
        let mut late = "X-Filler: y\n".repeat(MAX_FIELDS);
        late.push_str("Final-Recipient: rfc822; late@x.com\nAction: failed\nStatus: 5.1.1\n");
        assert!(DeliveryStatus::parse(&late, true).is_none());
        // Past the line cap nothing is read, however many recipients follow.
        let mut many = "> quoted\n".repeat(MAX_LINES);
        many.push_str("Final-Recipient: rfc822; z@x.com\nAction: failed\nStatus: 5.1.1\n");
        assert!(DeliveryStatus::parse(&many, true).is_none());
    }

    #[test]
    fn quoted_lines_and_text_without_a_recipient_are_ignored() {
        assert!(
            DeliveryStatus::parse(
                "> Final-Recipient: rfc822; a@x.com\n> Action: failed",
                false
            )
            .is_none()
        );
        assert!(DeliveryStatus::parse("Thanks for your mail.\nAction: failed", false).is_none());
        // A reporting block's own Message-ID is not the original one.
        let text = "Reporting-MTA: dns; mx\nMessage-ID: <own@mx>\n\nFinal-Recipient: rfc822; a@x.com\nAction: failed\nStatus: 5.0.0\n";
        assert_eq!(
            DeliveryStatus::parse(text, true)
                .unwrap()
                .original_message_id,
            None
        );
    }
}
