//! Conservative, deterministic hints for outreach replies. These hints are not proof of
//! identity or delivery status and must not be used alone to alter a recipient's state.

/// A first-pass classification of untrusted message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyClass {
    /// A delivery-status report explicitly says delivery failed permanently.
    HardBounce,
    /// An unquoted line asks to stop future messages.
    Stop,
    /// An automatic response indicates the sender is away.
    OutOfOffice,
    /// No sufficiently specific signal was found.
    Other,
}

/// Classify a reply without a model. Only the unquoted part of the message is examined.
/// A result is a hint; the caller must correlate it with an outgoing message and recipient
/// before taking any action. Delivery-status text is untrusted and can be forged.
#[must_use]
pub fn classify_reply(subject: &str, body: &str, auto_submitted: Option<&str>) -> ReplyClass {
    let mut lines = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('>')
            || line == "--"
            || line.starts_with("-----Original Message-----")
            || (line.starts_with("On ") && line.ends_with(" wrote:"))
        {
            break;
        }
        lines.push(line.to_lowercase());
    }
    // DSNs use Action and Status fields; requiring both avoids treating a temporary
    // failure or ordinary conversation mentioning a bounce as a permanent failure.
    let failed = lines.iter().any(|l| l.trim() == "action: failed");
    let permanent = lines.iter().any(|l| {
        l.strip_prefix("status:")
            .is_some_and(|s| s.trim().starts_with("5."))
    });
    if failed && permanent {
        return ReplyClass::HardBounce;
    }
    if lines.iter().any(|l| {
        matches!(
            l.trim_matches(|c: char| c.is_whitespace() || c == '.' || c == '!'),
            "stop" | "durdur" | "unsubscribe"
        )
    }) {
        return ReplyClass::Stop;
    }
    let automatic = auto_submitted.is_some_and(|a| {
        let a = a.trim();
        !a.is_empty() && !a.eq_ignore_ascii_case("no")
    });
    let subject = subject.to_lowercase();
    if automatic
        && [
            "out of office",
            "automatic reply",
            "auto reply",
            "ofis dışında",
            "otomatik yanıt",
        ]
        .iter()
        .any(|marker| subject.contains(marker) || lines.iter().any(|l| l.contains(marker)))
    {
        return ReplyClass::OutOfOffice;
    }
    ReplyClass::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_only_in_the_new_reply() {
        assert_eq!(
            classify_reply("Re: Hello", "STOP!\n\n> reply STOP", None),
            ReplyClass::Stop
        );
        assert_eq!(
            classify_reply("Re: Hello", "Thanks\n> reply STOP", None),
            ReplyClass::Other
        );
        assert_eq!(classify_reply("", "DURDUR", None), ReplyClass::Stop);
        assert_eq!(
            classify_reply("", "Please stop by tomorrow", None),
            ReplyClass::Other
        );
    }

    #[test]
    fn only_permanent_explicit_delivery_failures_count() {
        assert_eq!(
            classify_reply("", "Action: failed\nStatus: 5.1.1", None),
            ReplyClass::HardBounce
        );
        assert_eq!(
            classify_reply("", "Action: delayed\nStatus: 4.2.0", None),
            ReplyClass::Other
        );
        assert_eq!(
            classify_reply("", "Action: failed\nStatus: 4.2.0", None),
            ReplyClass::Other
        );
    }

    #[test]
    fn away_requires_an_automatic_response_signal() {
        assert_eq!(
            classify_reply("Out of office", "Back next week", Some("auto-replied")),
            ReplyClass::OutOfOffice
        );
        assert_eq!(
            classify_reply("Out of office", "I saw your email", Some("no")),
            ReplyClass::Other
        );
    }
}
