//! Risk class of a Composio action, decided here from what Composio returns
//! about it (HTTP method, tags, the words of its slug) and never from anything
//! the model says. Unknown or ambiguous means `write`.
//!
//! - `read`: only looks something up. Runs without a card on every level that
//!   allows reads.
//! - `write` / `destructive`: change something in the app. They follow the
//!   bot's level (on Full nothing asks).
//! - `money`: spends, moves or commits money. Asks on every level, Full
//!   included, and no grant waives it.
//!
//! UNVERIFIED against the live catalogue: the exact `tags` and method keys of
//! Composio's `/tools` items (`readOnlyHint`, `destructiveHint`, `http_method`
//! are read when present; the slug words carry the decision when they are not).

use std::fmt;

use serde::Serialize;

/// How risky one action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Only reads.
    Read,
    /// Creates or changes something; also the default when unsure.
    Write,
    /// Deletes, revokes or otherwise cannot be undone.
    Destructive,
    /// Spends, moves or commits money.
    Money,
}

impl Risk {
    /// Stable lowercase name used on cards, in the API and in the audit log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
            Self::Money => "money",
        }
    }
}

impl fmt::Display for Risk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for Risk {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// What Composio says about an action besides its slug.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActionMeta<'a> {
    /// Tags as returned (`readOnlyHint`, `destructiveHint`, ...).
    pub tags: &'a [String],
    /// HTTP method of the underlying request, when Composio gives one.
    pub method: Option<&'a str>,
}

/// Words that start a lookup.
const READ_VERBS: &[&str] = &[
    "LIST", "GET", "SEARCH", "FETCH", "FIND", "RETRIEVE", "READ", "LOOKUP", "QUERY", "COUNT",
    "DESCRIBE", "VIEW", "CHECK", "SHOW", "EXPORT", "DOWNLOAD", "LOOK",
];

/// Deleting words that are never a noun in a slug.
const DESTRUCTIVE: &[&str] = &[
    "DELETE",
    "REMOVE",
    "DESTROY",
    "REVOKE",
    "PURGE",
    "WIPE",
    "ERASE",
    "TRUNCATE",
    "CANCEL",
    "TERMINATE",
    "DEACTIVATE",
    "UNINSTALL",
];

/// Deleting words that are also nouns (`GET_REPOSITORY_ARCHIVE`): they count
/// only when the name does not start with a lookup verb.
const DESTRUCTIVE_NOUNS: &[&str] = &["ARCHIVE", "TRASH", "DROP", "BAN", "KICK", "CLEAR", "RESET"];

/// Verbs that spend or move money wherever they stand in the name.
const MONEY_VERBS: &[&str] = &["PAY", "PURCHASE", "BUY", "SUBSCRIBE", "WITHDRAW", "DONATE"];

/// Words that mean money only when the action is not a lookup (`LIST_CHARGES`
/// reads, `CREATE_CHARGE` charges).
const MONEY_WORDS: &[&str] = &[
    "CHARGE", "TRANSFER", "REFUND", "PAYMENT", "PAYOUT", "CHECKOUT", "MONEY", "FUNDS", "TOPUP",
];

/// Verbs that change something. Words that are also nouns (`POST`, `COMMENT`,
/// `LABEL`, `RUN`) are left out on purpose: a name without any listed word is
/// `write` anyway, and a lookup like `GET_A_POST` must stay a read.
const WRITE: &[&str] = &[
    "CREATE",
    "SEND",
    "PUBLISH",
    "UPDATE",
    "EDIT",
    "MODIFY",
    "PATCH",
    "PUT",
    "SET",
    "ADD",
    "INSERT",
    "UPLOAD",
    "WRITE",
    "APPEND",
    "REPLACE",
    "MOVE",
    "COPY",
    "RENAME",
    "ASSIGN",
    "UPSERT",
    "ENABLE",
    "DISABLE",
    "EXECUTE",
    "APPROVE",
    "REJECT",
    "SUBMIT",
    "REGISTER",
    "CONNECT",
    "DISCONNECT",
    "GRANT",
    "RESTORE",
    "IMPORT",
    "GENERATE",
    "CONVERT",
    "DUPLICATE",
    "CLONE",
    "REOPEN",
    "ATTACH",
    "DETACH",
    "NOTIFY",
    "ACCEPT",
    "DECLINE",
];

fn has(tokens: &[&str], words: &[&str]) -> bool {
    tokens.iter().any(|t| words.contains(t))
}

fn tag(meta: ActionMeta<'_>, name: &str) -> bool {
    meta.tags.iter().any(|t| t.eq_ignore_ascii_case(name))
}

/// The risk class of one action. `known_read` is the shipped table of
/// well-known read-only slugs.
#[must_use]
pub fn classify(toolkit: &str, tool_slug: &str, meta: ActionMeta<'_>, known_read: bool) -> Risk {
    let upper = tool_slug.to_ascii_uppercase();
    let prefix = format!("{}_", toolkit.to_ascii_uppercase());
    let name = upper.strip_prefix(&prefix).unwrap_or(&upper);
    let tokens: Vec<&str> = name.split('_').filter(|t| !t.is_empty()).collect();
    let method = meta.method.map(str::to_ascii_uppercase);
    let method = method.as_deref();

    let read_head = tokens.first().is_some_and(|t| READ_VERBS.contains(t));
    let destructive = has(&tokens, DESTRUCTIVE)
        || (!read_head && has(&tokens, DESTRUCTIVE_NOUNS))
        || method == Some("DELETE")
        || tag(meta, "destructiveHint");
    let money_verb = has(&tokens, MONEY_VERBS);
    let money_word = !read_head && !destructive && has(&tokens, MONEY_WORDS);
    // A slug that mentions a charge or a transfer after a lookup verb is
    // still a lookup; a destructive word next to a money noun is not a spend.
    if money_verb || money_word {
        return Risk::Money;
    }
    if destructive {
        return Risk::Destructive;
    }
    if known_read && !matches!(method, Some("PUT" | "PATCH" | "DELETE")) {
        return Risk::Read;
    }
    let writes = has(&tokens, WRITE) || matches!(method, Some("PUT" | "PATCH"));
    if writes {
        return Risk::Write;
    }
    let read_hint = tag(meta, "readOnlyHint");
    // A POST that only reads says so with the hint; a lookup name alone is
    // not enough then.
    let method_allows = match method {
        None | Some("GET" | "HEAD") => true,
        Some("POST") => read_hint,
        Some(_) => false,
    };
    if (read_head || read_hint) && method_allows {
        return Risk::Read;
    }
    Risk::Write
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta<'a>(tags: &'a [String], method: Option<&'a str>) -> ActionMeta<'a> {
        ActionMeta { tags, method }
    }

    fn c(slug: &str) -> Risk {
        classify("x", slug, ActionMeta::default(), false)
    }

    #[test]
    fn names_decide_when_metadata_is_silent() {
        for (slug, want) in [
            ("X_LIST_ALL_CHANNELS", Risk::Read),
            ("X_GET_A_USER", Risk::Read),
            ("X_SEARCH_MESSAGES", Risk::Read),
            ("X_FETCH_EMAILS", Risk::Read),
            ("X_LIST_CHARGES", Risk::Read),
            ("X_GET_PAYMENT", Risk::Read),
            ("X_LIST_SUBSCRIPTIONS", Risk::Read),
            ("X_LIST_UPDATED_FILES", Risk::Read),
            ("X_GET_A_POST", Risk::Read),
            ("X_GET_REPOSITORY_ARCHIVE", Risk::Read),
            ("X_ARCHIVE_REPOSITORY", Risk::Destructive),
            ("X_POST_A_TWEET", Risk::Write),
            ("X_CREATE_AN_ISSUE", Risk::Write),
            ("X_SEND_MESSAGE", Risk::Write),
            ("X_UPDATE_A_PAGE", Risk::Write),
            ("X_GET_OR_CREATE_USER", Risk::Write),
            ("X_DELETE_A_REPOSITORY", Risk::Destructive),
            ("X_REMOVE_MEMBER", Risk::Destructive),
            ("X_DELETE_PAYMENT_METHOD", Risk::Destructive),
            ("X_PAY_INVOICE", Risk::Money),
            ("X_CREATE_CHARGE", Risk::Money),
            ("X_CREATE_PAYMENT_INTENT", Risk::Money),
            ("X_PURCHASE_ITEM", Risk::Money),
            ("X_SUBSCRIBE_TO_PLAN", Risk::Money),
            ("X_TRANSFER_FUNDS", Risk::Money),
            ("X_GET_AND_PAY", Risk::Money),
            ("X_DO_THING", Risk::Write),
            ("X_PAYLOAD_DUMP", Risk::Write),
        ] {
            assert_eq!(c(slug), want, "{slug}");
        }
    }

    #[test]
    fn metadata_can_only_raise_a_class_except_for_the_read_hint() {
        let tags = |t: &[&str]| t.iter().map(ToString::to_string).collect::<Vec<_>>();
        let ro = tags(&["readOnlyHint"]);
        let destructive = tags(&["destructiveHint"]);
        let none: Vec<String> = Vec::new();
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&none, Some("DELETE")), false),
            Risk::Destructive
        );
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&destructive, None), false),
            Risk::Destructive
        );
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&none, Some("PUT")), false),
            Risk::Write
        );
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&none, Some("POST")), false),
            Risk::Write
        );
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&ro, Some("POST")), false),
            Risk::Read
        );
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&none, Some("GET")), false),
            Risk::Read
        );
        assert_eq!(
            classify("x", "X_DO_THING", meta(&ro, Some("GET")), false),
            Risk::Read
        );
        // The hint never beats a write, delete or money word in the name.
        assert_eq!(
            classify("x", "X_DELETE_ITEM", meta(&ro, None), false),
            Risk::Destructive
        );
        assert_eq!(
            classify("x", "X_SEND_ITEM", meta(&ro, None), false),
            Risk::Write
        );
        assert_eq!(
            classify("x", "X_PAY_ITEM", meta(&ro, None), false),
            Risk::Money
        );
        // The shipped table of read-only slugs counts as read evidence, but a
        // write method still wins.
        assert_eq!(
            classify("x", "X_ODD_NAME", meta(&none, None), true),
            Risk::Read
        );
        assert_eq!(
            classify("x", "X_ODD_NAME", meta(&none, Some("PUT")), true),
            Risk::Write
        );
    }

    #[test]
    fn the_toolkit_prefix_is_not_part_of_the_name() {
        assert_eq!(
            classify(
                "stripe",
                "STRIPE_LIST_CHARGES",
                ActionMeta::default(),
                false
            ),
            Risk::Read
        );
        // A toolkit named like a verb must not change the class of its actions.
        assert_eq!(
            classify("send", "SEND_LIST_ITEMS", ActionMeta::default(), false),
            Risk::Read
        );
    }

    #[test]
    fn names_render_and_compare() {
        assert_eq!(Risk::Money.as_str(), "money");
        assert_eq!(Risk::Read.to_string(), "read");
        assert!(Risk::Money > Risk::Destructive && Risk::Destructive > Risk::Write);
        assert_eq!(Risk::Write, "write");
    }
}
