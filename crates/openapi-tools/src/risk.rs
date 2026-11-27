//! Risk class of an operation: its HTTP method gives the [`Effect`], and a
//! money-like path or operation id raises it to "always ask".

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
    "refund", "billing",
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
    Risk {
        effect: effect_for(op.method),
        always_ask: is_money_like(&op.path, op.operation_id.as_deref()),
    }
}

/// Risk from loose parts (used where no [`Operation`] exists yet).
#[must_use]
pub fn risk_of_parts(method: HttpMethod, path: &str, operation_id: Option<&str>) -> Risk {
    Risk {
        effect: effect_for(method),
        always_ask: is_money_like(path, operation_id),
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
