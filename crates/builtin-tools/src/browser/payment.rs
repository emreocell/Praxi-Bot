//! Recognize checkout surfaces in the managed browser. Positive signals can
//! require a card, but their absence cannot prove what a site will do.

use std::hash::{DefaultHasher, Hash, Hasher};

use serde_json::Value;
use tool_registry::{PaymentApproval, ToolError};

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

fn payment_label(label: &str) -> bool {
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
        let mut detected =
            url.as_deref().is_some_and(payment_url) || driver.has_payment_form().await;
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
                .unwrap_or_default(),
        )
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
                document_token(driver, url.as_deref()).await == Some(approved)
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
}
