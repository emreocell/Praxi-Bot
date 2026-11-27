//! Risk class of an operation: its HTTP method gives the [`Effect`], and a
//! money-like path, operation id, tag, summary or description, or a generic
//! single-endpoint `POST` (`/graphql`, `/rpc`, ...) that could carry anything,
//! raises it to "always ask".

use tool_registry::{Effect, PermissionCategory};

use crate::import::{HttpMethod, Operation};
use crate::tool::effect_for;

/// Word stems that mark an operation as moving money. A name part matches when
/// it starts with a stem in [`PREFIX_STEMS`] or contains one in [`INNER_STEMS`].
///
/// `pay` is prefix-only so `payload` and `display` do not match; the long
/// stems are matched anywhere so `createcheckoutsession` still does.
const PREFIX_STEMS: &[&str] = &["pay", "buy", "transfer", "refund", "withdraw"];
const INNER_STEMS: &[&str] = &[
    "payment", "payout", "charge", "checkout", "purchase", "subscri", "transfer", "invoice",
    "refund", "billing", "money",
];
/// Whole words that start with a stem but are not about money.
const NOT_MONEY: &[&str] = &["payload", "payloads"];

/// Lower-case words of `text`: split on anything that is not a letter or digit
/// and on camelCase boundaries (`createPaymentIntent` -> create, payment, intent).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn word_is_money(w: &str) -> bool {
    if NOT_MONEY.contains(&w) {
        return false;
    }
    PREFIX_STEMS.iter().any(|s| w.starts_with(s)) || INNER_STEMS.iter().any(|s| w.contains(s))
}

/// Whether the path or the operation id talks about moving money (pay, charge,
/// checkout, purchase, subscription, transfer, invoice and close relatives).
/// Matching is by name only, so it errs on the side of asking.
#[must_use]
pub fn is_money_like(path: &str, operation_id: Option<&str>) -> bool {
    words(path)
        .iter()
        .chain(operation_id.map(words).iter().flatten())
        .any(|w| word_is_money(w))
}

/// Last path segments of APIs that serve every call from one endpoint, so the
/// path says nothing about what the request does.
const GENERIC_ENDPOINTS: &[&str] = &["graphql", "gql", "api", "query", "rpc", "jsonrpc"];

/// A `POST` to a generic single endpoint (`POST /graphql`, `POST /rpc`): the
/// request body decides what happens, and it may well be a charge.
fn is_generic_post(method: HttpMethod, path: &str) -> bool {
    method == HttpMethod::Post
        && path
            .rsplit('/')
            .find(|s| !s.is_empty())
            .is_some_and(|last| GENERIC_ENDPOINTS.contains(&last.to_ascii_lowercase().as_str()))
}

/// How an operation is treated by the permission system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Risk {
    /// From the HTTP method: GET/HEAD/OPTIONS read, DELETE destructive, the rest write.
    pub effect: Effect,
    /// Money-like: the call asks on every autonomy level, including Full access.
    pub always_ask: bool,
}

impl Risk {
    /// Permission category: [`PermissionCategory::Money`] when `always_ask`,
    /// otherwise `fallback`.
    #[must_use]
    pub fn category(self, fallback: PermissionCategory) -> PermissionCategory {
        if self.always_ask {
            PermissionCategory::Money
        } else {
            fallback
        }
    }
}

/// Risk of `op`.
#[must_use]
pub fn risk_of(op: &Operation) -> Risk {
    // The tags and the summary/description can name money even when the path
    // and the operation id are benign (`POST /v1/things` tagged "Payments").
    let described = op
        .tags
        .iter()
        .chain([&op.description])
        .any(|t| words(t).iter().any(|w| word_is_money(w)));
    Risk {
        effect: effect_for(op.method),
        always_ask: is_money_like(&op.path, op.operation_id.as_deref())
            || described
            || is_generic_post(op.method, &op.path),
    }
}

/// Risk from loose parts (used where no [`Operation`] exists yet).
#[must_use]
pub fn risk_of_parts(method: HttpMethod, path: &str, operation_id: Option<&str>) -> Risk {
    Risk {
        effect: effect_for(method),
        always_ask: is_money_like(path, operation_id) || is_generic_post(method, path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_like_names_are_found() {
        for (path, id) in [
            ("/v1/payments", None),
            ("/pay", None),
            ("/orders/{id}/pay", None),
            ("/v1/charges", Some("createCharge")),
            ("/checkout/sessions", None),
            ("/x", Some("createCheckoutSession")),
            ("/cart/purchase", None),
            ("/subscriptions/{id}", None),
            ("/x", Some("cancelSubscription")),
            ("/accounts/{id}/transfers", None),
            ("/invoices/{invoice_id}", None),
            ("/x", Some("invoice_pay")),
            ("/payouts", None),
            ("/x", Some("PaymentIntents_create")),
        ] {
            assert!(is_money_like(path, id), "{path} {id:?}");
        }
    }

    #[test]
    fn ordinary_names_are_not() {
        for (path, id) in [
            ("/pets", Some("listPets")),
            ("/users/{id}", None),
            ("/files/{id}/payload", None),
            ("/display/{id}", None),
            ("/repos/{owner}/{repo}/issues", Some("issues_list")),
            ("/weather", None),
        ] {
            assert!(!is_money_like(path, id), "{path} {id:?}");
        }
    }

    fn op(method: HttpMethod, path: &str, tags: &[&str], description: &str) -> Operation {
        Operation {
            name: "t".into(),
            operation_id: Some("doIt".into()),
            method,
            path: path.into(),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            description: description.into(),
            deprecated: false,
            params: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn tags_summary_and_description_can_make_an_operation_money() {
        let benign = op(
            HttpMethod::Post,
            "/v1/things",
            &["Things"],
            "Create a thing.",
        );
        assert!(!risk_of(&benign).always_ask);
        for o in [
            op(
                HttpMethod::Post,
                "/v1/things",
                &["Payments"],
                "Create a thing.",
            ),
            op(
                HttpMethod::Post,
                "/v1/things",
                &[],
                "Charges the customer's card.",
            ),
            op(
                HttpMethod::Get,
                "/v1/things",
                &[],
                "Create a thing. Pay-out report",
            ),
            op(HttpMethod::Put, "/v1/things", &["billing_admin"], ""),
        ] {
            assert!(risk_of(&o).always_ask, "{o:?}");
        }
        // The money rule is about money only: other writes keep their category.
        let w = risk_of(&op(
            HttpMethod::Delete,
            "/v1/things/{id}",
            &["Things"],
            "Remove.",
        ));
        assert!(!w.always_ask);
        assert_eq!(w.effect, Effect::Destructive);
    }

    #[test]
    fn a_generic_single_endpoint_post_asks_but_reads_do_not() {
        for p in ["/graphql", "/api", "/v2/query", "/rpc/", "/JSONRPC"] {
            let r = risk_of(&op(HttpMethod::Post, p, &[], "Run it."));
            assert!(r.always_ask, "{p}");
            assert_eq!(r.effect, Effect::Write);
            assert_eq!(
                r.category(PermissionCategory::Accounts),
                PermissionCategory::Money
            );
            assert!(risk_of_parts(HttpMethod::Post, p, None).always_ask, "{p}");
        }
        assert!(!risk_of(&op(HttpMethod::Get, "/graphql", &[], "Schema.")).always_ask);
        assert!(!risk_of(&op(HttpMethod::Post, "/apis/{id}/keys", &[], "Make.")).always_ask);
        assert!(!risk_of(&op(HttpMethod::Post, "/queries", &[], "Make.")).always_ask);
    }

    #[test]
    fn camel_case_is_split() {
        assert_eq!(
            words("createPaymentIntent"),
            ["create", "payment", "intent"]
        );
        assert_eq!(words("/v1/pay-now/{id}"), ["v1", "pay", "now", "id"]);
    }

    #[test]
    fn method_gives_effect_and_money_gives_the_category() {
        let r = risk_of_parts(HttpMethod::Get, "/pets", None);
        assert_eq!(r.effect, Effect::Read);
        assert!(!r.always_ask);
        assert_eq!(
            r.category(PermissionCategory::Accounts),
            PermissionCategory::Accounts
        );
        let r = risk_of_parts(HttpMethod::Delete, "/subscriptions/{id}", None);
        assert_eq!(r.effect, Effect::Destructive);
        assert!(r.always_ask);
        assert_eq!(
            r.category(PermissionCategory::Accounts),
            PermissionCategory::Money
        );
        assert_eq!(
            risk_of_parts(HttpMethod::Put, "/x", None).effect,
            Effect::Write
        );
    }
}
