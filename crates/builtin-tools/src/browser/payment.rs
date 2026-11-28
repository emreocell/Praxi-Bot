//! Recognize checkout surfaces in the managed browser. Positive signals can
//! require a card, but their absence cannot prove what a site will do.

use std::hash::{DefaultHasher, Hash, Hasher};

use serde_json::Value;
use tool_registry::{CheckoutFacts, PaymentApproval, ToolError};

use super::{BrowserDriver, BrowserTool, Op, PAYMENT_APPROVAL, act};

fn payment_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let provider = matches!(
        url.host_str(),
        Some("checkout.stripe.com" | "buy.stripe.com" | "pay.google.com" | "payments.google.com")
    );
    provider
        || url.path_segments().is_some_and(|mut segments| {
            segments.any(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "checkout" | "payment" | "pay" | "confirm-purchase"
                )
            })
        })
}

pub(crate) fn payment_label(label: &str) -> bool {
    let label = label.to_lowercase();
    let label = label.trim();
    [
        "pay now",
        "confirm payment",
        "complete purchase",
        "place order",
        "buy now",
        "confirm transfer",
        "confirm subscription",
        "ödeme yap",
        "ödemeyi onayla",
        "şimdi satın al",
        "siparişi ver",
    ]
    .iter()
    .any(|p| {
        label == *p
            || label
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

fn takes_payment_input(op: Op) -> bool {
    matches!(
        op,
        Op::Click | Op::Fill | Op::Select | Op::Press | Op::Act | Op::Eval
    )
}

/// The token of a payment page that cannot be bound to a document (no capture epoch). `None`
/// from `payment_page` means "not a payment", so such a page is reported with this value
/// instead: it is asked about like any payment, and no approval can ever match it.
pub(super) const UNBOUND: u64 = 0;

/// Identifies one document of the page: its capture epoch and URL. Never [`UNBOUND`];
/// `None` when the driver reports no epoch.
async fn document_token(driver: &dyn BrowserDriver, url: Option<&str>) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    driver.capture_epoch().await?.hash(&mut hasher);
    url.hash(&mut hasher);
    Some(hasher.finish().max(1))
}

impl BrowserTool {
    pub(super) async fn payment_page_with(
        &self,
        driver: &dyn BrowserDriver,
        input: &Value,
    ) -> Option<u64> {
        if !takes_payment_input(self.op) {
            return None;
        }
        let url = driver.current_url().await;
        // A script that names a payment API is a payment on any page, not only on a checkout.
        let mut detected = self.op == Op::Eval
            && input["js"]
                .as_str()
                .is_some_and(tool_registry::payment::script_names_payment_host);
        detected =
            detected || url.as_deref().is_some_and(payment_url) || driver.has_payment_form().await;
        if !detected {
            let refs = if self.op == Op::Act {
                act::parse_steps(input)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|step| step.ref_of().map(str::to_owned))
                    .collect()
            } else {
                input["ref"]
                    .as_str()
                    .map(str::to_owned)
                    .into_iter()
                    .collect::<Vec<_>>()
            };
            for r in refs {
                if driver
                    .ref_identity(&r)
                    .await
                    .is_some_and(|(_, label)| payment_label(&label))
                {
                    detected = true;
                    break;
                }
            }
            if self.op == Op::Press && !detected {
                detected = driver
                    .focused_target()
                    .await
                    .is_ok_and(|field| payment_label(&field.name));
            }
        }
        if !detected {
            return None;
        }
        // A recognized payment surface without a document identity may be shown
        // for approval, but nothing is sent if it cannot be bound to that page.
        Some(
            document_token(driver, url.as_deref())
                .await
                .unwrap_or(UNBOUND),
        )
    }

    /// Before a script of the model's own runs: unless the owner approved a payment on this
    /// page, requests to a payment API are refused at the network until the document changes.
    /// The script text is checked by name ([`payment_page_with`](Self::payment_page_with)); this
    /// catches a host the script only spells out at run time, and a timer it leaves behind.
    pub(super) async fn arm_payment_block(&self) -> Result<(), ToolError> {
        let armed = matches!(
            PAYMENT_APPROVAL.try_with(Clone::clone).unwrap_or_default(),
            PaymentApproval::Check
        );
        self.shared
            .driver
            .set_payment_block(armed)
            .await
            .or_else(|e| {
                // Lifting a block that did not go on is harmless; failing to put it on is not.
                if armed {
                    Err(ToolError::PermissionDenied(format!(
                        "Script withheld: the guard against payment requests could not be put on the page ({e})."
                    )))
                } else {
                    Ok(())
                }
            })
    }

    // Each batch step goes through its single tool here, after taking
    // control. A page can enter checkout while an earlier step runs.
    pub(super) async fn check_payment_approval(
        &self,
        target: Option<&str>,
    ) -> Result<(), ToolError> {
        let payment = PAYMENT_APPROVAL.try_with(Clone::clone).unwrap_or_default();
        let driver = &*self.shared.driver;
        let valid = match payment {
            PaymentApproval::Unchecked => true,
            PaymentApproval::Check => {
                let input = serde_json::json!({ "ref": target });
                self.payment_page_with(driver, &input).await.is_none()
            }
            PaymentApproval::Approved(approved) => {
                let url = driver.current_url().await;
                // An approval of an unbound page binds nothing: two pages without an identity
                // are not the same document.
                let same = approved != UNBOUND
                    && document_token(driver, url.as_deref()).await == Some(approved);
                if same {
                    // The owner approved a payment on this document: its own requests to a payment
                    // API (a checkout tokenising a card) must go through.
                    let _ = driver.set_payment_block(false).await;
                }
                same
            }
        };
        if !valid {
            return Err(ToolError::PermissionDenied(
                "Payment input withheld. Take a fresh snapshot and request the payment action as a separate call."
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Page script that reads the merchant and the first total-like amount. It only reads; what it
/// returns is the page's claim (a page can rewrite the DOM functions it calls).
const CHECKOUT_FACTS_JS: &str = r#"(() => {
  const og = document.querySelector('meta[property="og:site_name"]');
  const site = ((og && og.getAttribute('content')) || document.title || '').slice(0, 200);
  const cur = '(?:[$€£¥₺]|\\b(?:USD|EUR|GBP|TRY|JPY|CHF|CAD|AUD|TL)\\b)';
  const num = '\\d[\\d.,  ]*\\d';
  const money = new RegExp(cur + '\\s?' + num + '|' + num + '\\s?' + cur);
  const label = /total|amount due|to pay|toplam|tutar|ödenecek/i;
  let amount = '';
  const nodes = document.body ? document.body.querySelectorAll('*') : [];
  for (let i = 0; i < nodes.length && i < 3000; i++) {
    const el = nodes[i];
    if (el.children.length > 0) continue;
    const own = (el.textContent || '').trim();
    if (!own || own.length > 60 || !label.test(own)) continue;
    const box = (el.parentElement && el.parentElement.innerText) || own;
    const m = box.slice(0, 300).match(money);
    if (m) { amount = m[0]; break; }
  }
  if (!amount && document.body) {
    const m = (document.body.innerText || '').slice(0, 20000).match(money);
    if (m) amount = m[0];
  }
  return { site, amount };
})()"#;

/// Longest merchant or amount text kept.
const MAX_FACT_CHARS: usize = 80;
/// Largest screenshot kept for the phone page.
const MAX_SHOT_BYTES: usize = 4 * 1024 * 1024;
/// How long reading the checkout may delay the card.
const FACTS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// One line of page text: whitespace collapsed, control characters removed, at most
/// [`MAX_FACT_CHARS`] characters.
fn clean_fact(raw: &str) -> Option<String> {
    let line: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FACT_CHARS)
        .collect();
    (!line.is_empty()).then_some(line)
}

fn image_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// What the current page shows for the owner's money card: the browser's own host name, the
/// page's merchant and amount, and a viewport screenshot. Every part is best effort.
pub(super) async fn read_checkout(driver: &dyn BrowserDriver) -> CheckoutFacts {
    let read = async {
        let domain = driver
            .current_url()
            .await
            .and_then(|u| url::Url::parse(&u).ok())
            .and_then(|u| u.host_str().map(str::to_owned));
        let page = driver.eval(CHECKOUT_FACTS_JS).await.unwrap_or_default();
        let screenshot = driver
            .screenshot(false, None)
            .await
            .ok()
            .filter(|(bytes, _)| !bytes.is_empty() && bytes.len() <= MAX_SHOT_BYTES)
            .and_then(|(bytes, ext)| image_mime(ext).map(|mime| (bytes, mime)));
        CheckoutFacts {
            domain,
            merchant: page["site"].as_str().and_then(clean_fact),
            amount: page["amount"].as_str().and_then(clean_fact),
            screenshot,
        }
    };
    tokio::time::timeout(FACTS_TIMEOUT, read)
        .await
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_urls_and_labels_are_specific() {
        assert!(payment_url("https://checkout.stripe.com/session"));
        assert!(payment_url("https://shop.example/checkout/confirm"));
        assert!(!payment_url("https://shop.example/help?article=checkout"));
        assert!(!payment_url("https://example.org/checkout-help"));
        assert!(payment_label("Pay now €12"));
        assert!(payment_label("Ödemeyi onayla"));
        assert!(!payment_label("Pay history"));
        assert!(!payment_label("Buy newsletter subscription tips"));
    }

    #[test]
    fn checkout_facts_are_one_short_line() {
        assert_eq!(
            clean_fact("  Shop\n\t Name\u{7}  ").as_deref(),
            Some("Shop Name")
        );
        assert_eq!(clean_fact(" \n "), None);
        assert_eq!(
            clean_fact(&"x".repeat(500)).map(|s| s.len()),
            Some(MAX_FACT_CHARS)
        );
        assert_eq!(image_mime("png"), Some("image/png"));
        assert_eq!(image_mime("svg"), None);
    }
}
