//! Risk class of a Composio action, decided here from what Composio returns
//! about it (HTTP method, tags, the words of its slug) and never from anything
//! the model says. Unknown or ambiguous means `write`.
//!
//! - `read`: only looks something up, and Composio says so: an explicit `GET` /
//!   `HEAD` or a read-only hint. A POST, a missing or unknown method is at
//!   least `write`, whatever the name or the shipped read table says. Runs
//!   without a card on every level that allows reads.
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
/// reads, `CREATE_CHARGE` charges). `UPGRADE` covers `UPGRADE_PLAN_TO_PRO`,
/// `ORDER` covers `PLACE_ORDER`.
const MONEY_WORDS: &[&str] = &[
    "CHARGE", "TRANSFER", "REFUND", "PAYMENT", "PAYOUT", "CHECKOUT", "MONEY", "FUNDS", "TOPUP",
    "INVOICE", "INVOICES", "BILLING", "ORDER", "ORDERS", "UPGRADE",
];

/// Words that reach other people or change who may do what. Nouns too (`POST`,
/// `ROLE`), so only a pure `LIST` / `SEARCH` over a `GET` may carry them and
/// stay a read (`LIST_POSTS`, `LIST_ROLES`); `GET_INVITE_LINK` and
/// `GET_A_POST` are `write`.
const SIDE_EFFECT_WORDS: &[&str] = &[
    "SHARE",
    "SHARED",
    "INVITE",
    "INVITES",
    "INVITATION",
    "FORWARD",
    "REPLY",
    "POST",
    "POSTS",
    "TWEET",
    "TWEETS",
    "PERMISSION",
    "PERMISSIONS",
    "PERMIT",
    "ROLE",
    "ROLES",
    "MEMBER",
    "MEMBERS",
];

/// Verbs that change something. Words that are also nouns (`COMMENT`, `LABEL`,
/// `RUN`) are left out on purpose: a name without any listed word is `write`
/// anyway (a read needs explicit evidence).
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

/// Lower case letters and digits only, so `readOnlyHint`, `read-only-hint` and
/// `READ_ONLY_HINT` are one tag.
#[must_use]
pub fn normalize_tag(tag: &str) -> String {
    tag.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether `meta` carries one of the (already normalised) spellings of a hint.
fn tag(meta: ActionMeta<'_>, spellings: &[&str]) -> bool {
    meta.tags
        .iter()
        .any(|t| spellings.contains(&normalize_tag(t).as_str()))
}

const READ_ONLY_TAGS: &[&str] = &["readonlyhint", "readonlyhinttrue", "readonly"];
const DESTRUCTIVE_TAGS: &[&str] = &["destructivehint", "destructivehinttrue", "destructive"];

/// Whether a normalised tag is one of the hints `classify` reads.
#[must_use]
pub fn is_hint_tag(normalized: &str) -> bool {
    READ_ONLY_TAGS.contains(&normalized) || DESTRUCTIVE_TAGS.contains(&normalized)
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
        || tag(meta, DESTRUCTIVE_TAGS);
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
    if has(&tokens, WRITE) || matches!(method, Some("PUT" | "PATCH")) {
        return Risk::Write;
    }
    let safe_method = matches!(method, Some("GET" | "HEAD"));
    // Only a pure lookup over a GET may carry a side-effect word.
    let pure_lookup = matches!(tokens.first(), Some(&("LIST" | "SEARCH"))) && safe_method;
    if has(&tokens, SIDE_EFFECT_WORDS) && !pure_lookup {
        return Risk::Write;
    }
    let read_hint = tag(meta, READ_ONLY_TAGS);
    // Evidence a read needs: an explicit GET/HEAD, or the read-only hint on a
    // request that is not a PUT/PATCH/DELETE (a POST that only reads, or a
    // method Composio did not give). Without it the action is a write, even
    // for a lookup name or a slug of the shipped table.
    let evidence = safe_method || (read_hint && matches!(method, None | Some("POST")));
    if evidence && (read_head || read_hint || known_read) {
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

    /// The class of `slug` when Composio says it is a GET.
    fn c(slug: &str) -> Risk {
        classify("x", slug, meta(&[], Some("GET")), false)
    }

    fn tags(t: &[&str]) -> Vec<String> {
        t.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn names_decide_over_a_get() {
        for (slug, want) in [
            ("X_LIST_ALL_CHANNELS", Risk::Read),
            ("X_GET_A_USER", Risk::Read),
            ("X_SEARCH_MESSAGES", Risk::Read),
            ("X_FETCH_EMAILS", Risk::Read),
            ("X_LIST_CHARGES", Risk::Read),
            ("X_GET_PAYMENT", Risk::Read),
            ("X_LIST_SUBSCRIPTIONS", Risk::Read),
            ("X_LIST_UPDATED_FILES", Risk::Read),
            ("X_GET_A_POST", Risk::Write),
            ("X_LIST_POSTS", Risk::Read),
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
    }

    #[test]
    fn money_covers_invoices_billing_orders_and_plan_upgrades() {
        for slug in [
            "X_CREATE_INVOICE",
            "X_CREATE_INVOICES",
            "X_SEND_INVOICE",
            "X_UPDATE_BILLING",
            "X_UPDATE_BILLING_DETAILS",
            "X_CREATE_ORDER",
            "X_PLACE_ORDER",
            "X_PLACE_ORDERS",
            "X_UPGRADE_PLAN_TO_PRO",
            "X_UPGRADE_PLAN",
            "X_UPGRADE_SUBSCRIPTION",
        ] {
            assert_eq!(c(slug), Risk::Money, "{slug}");
            assert_eq!(
                classify("x", slug, ActionMeta::default(), false),
                Risk::Money,
                "{slug} without a method"
            );
        }
        // Lookups stay reads; a delete is a delete, not a spend.
        for slug in [
            "X_LIST_INVOICES",
            "X_GET_INVOICE",
            "X_LIST_ORDERS",
            "X_GET_ORDER",
            "X_GET_BILLING_INFO",
            "X_SEARCH_ORDERS",
        ] {
            assert_eq!(c(slug), Risk::Read, "{slug}");
        }
        assert_eq!(c("X_DELETE_INVOICE"), Risk::Destructive);
        assert_eq!(c("X_CANCEL_ORDER"), Risk::Destructive);
    }

    #[test]
    fn the_shipped_read_table_cannot_launder_a_post_or_a_missing_method() {
        let none: Vec<String> = Vec::new();
        let ro = tags(&["readOnlyHint"]);
        for (method, want) in [
            (Some("GET"), Risk::Read),
            (Some("HEAD"), Risk::Read),
            (Some("POST"), Risk::Write),
            (Some("PUT"), Risk::Write),
            (Some("PATCH"), Risk::Write),
            (Some("DELETE"), Risk::Destructive),
            (Some("TRACE"), Risk::Write),
            (None, Risk::Write),
        ] {
            assert_eq!(
                classify("x", "X_LIST_ITEMS", meta(&none, method), true),
                want,
                "{method:?}"
            );
            // The table counts as a name, never as evidence by itself.
            assert_eq!(
                classify("x", "X_ODD_NAME", meta(&none, method), true),
                if method.is_some_and(|m| m == "GET" || m == "HEAD") {
                    Risk::Read
                } else if method == Some("DELETE") {
                    Risk::Destructive
                } else {
                    Risk::Write
                },
                "{method:?} odd name"
            );
        }
        // The hint is evidence on a POST and on an unknown method.
        assert_eq!(
            classify("x", "X_ODD_NAME", meta(&ro, Some("POST")), true),
            Risk::Read
        );
        assert_eq!(
            classify("x", "X_ODD_NAME", meta(&ro, None), true),
            Risk::Read
        );
        assert_eq!(
            classify("x", "X_ODD_NAME", meta(&ro, Some("PUT")), true),
            Risk::Write
        );
    }

    #[test]
    fn a_lookup_name_alone_is_not_a_read() {
        // Missing or misspelled method keys reach classify as `None`.
        for slug in [
            "X_LIST_ITEMS",
            "X_GET_A_USER",
            "X_FETCH_EMAILS",
            "X_SEARCH_X",
        ] {
            assert_eq!(
                classify("x", slug, ActionMeta::default(), false),
                Risk::Write,
                "{slug}"
            );
        }
        let ro = tags(&["readOnlyHint"]);
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&ro, None), false),
            Risk::Read
        );
        // Unrecognised tags are no evidence.
        let odd = tags(&["safe", "idempotent", "read"]);
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&odd, Some("POST")), false),
            Risk::Write
        );
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&odd, None), false),
            Risk::Write
        );
    }

    #[test]
    fn side_effect_words_make_a_write_unless_a_pure_list_or_search_over_get() {
        for slug in [
            "X_GET_INVITE_LINK",
            "X_GET_SHARED_LINK",
            "X_FETCH_PERMISSIONS_FOR_FILE",
            "X_GET_MEMBER",
            "X_GET_A_POST",
            "X_FIND_ROLE",
            "X_FORWARD_MESSAGE",
            "X_REPLY_TO_THREAD",
            "X_TWEET",
            "X_POST_A_TWEET",
            "X_INVITE_USER",
            "X_SHARE_FILE",
        ] {
            assert_eq!(c(slug), Risk::Write, "{slug} over GET");
            assert_eq!(
                classify("x", slug, ActionMeta::default(), false),
                Risk::Write,
                "{slug} without a method"
            );
            // Not even the hint or the shipped table lifts them off a POST.
            let ro = tags(&["readOnlyHint"]);
            assert_eq!(
                classify("x", slug, meta(&ro, Some("POST")), true),
                Risk::Write,
                "{slug} with hint on POST"
            );
        }
        for slug in [
            "X_LIST_MEMBERS",
            "X_LIST_ROLES",
            "X_LIST_PERMISSIONS",
            "X_SEARCH_POSTS",
            "X_LIST_INVITES",
            "X_LIST_SHARED_DRIVES",
        ] {
            assert_eq!(c(slug), Risk::Read, "{slug}");
            assert_eq!(
                classify("x", slug, ActionMeta::default(), false),
                Risk::Write,
                "{slug} without a method"
            );
        }
        // Removing a member is a delete, not a plain write.
        assert_eq!(c("X_REMOVE_MEMBER"), Risk::Destructive);
    }

    #[test]
    fn tag_spellings_are_normalised() {
        let none: Vec<String> = Vec::new();
        for spelling in [
            "readOnlyHint",
            "read-only-hint",
            "READ_ONLY_HINT",
            "readOnlyHint:true",
            "read only hint",
        ] {
            let t = tags(&[spelling]);
            assert_eq!(
                classify("x", "X_DO_THING", meta(&t, Some("POST")), false),
                Risk::Read,
                "{spelling}"
            );
        }
        // `:false` is not the hint.
        let t = tags(&["readOnlyHint:false"]);
        assert_eq!(
            classify("x", "X_DO_THING", meta(&t, Some("POST")), false),
            Risk::Write
        );
        for spelling in [
            "destructiveHint",
            "destructive-hint",
            "DESTRUCTIVE",
            "destructive:hint",
        ] {
            let t = tags(&[spelling]);
            assert_eq!(
                classify("x", "X_LIST_ITEMS", meta(&t, Some("GET")), false),
                Risk::Destructive,
                "{spelling}"
            );
        }
        assert_eq!(normalize_tag("Read-Only/Hint:1"), "readonlyhint1");
        assert!(is_hint_tag("readonlyhint") && !is_hint_tag("safe"));
        assert_eq!(
            classify("x", "X_LIST_ITEMS", meta(&none, Some("GET")), false),
            Risk::Read
        );
    }

    #[test]
    fn the_toolkit_prefix_is_not_part_of_the_name() {
        assert_eq!(
            classify(
                "stripe",
                "STRIPE_LIST_CHARGES",
                meta(&[], Some("GET")),
                false
            ),
            Risk::Read
        );
        // A toolkit named like a verb must not change the class of its actions.
        assert_eq!(
            classify("send", "SEND_LIST_ITEMS", meta(&[], Some("GET")), false),
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
