//! The payment APIs a call must not reach unasked.
//!
//! One list, read by the daemon when it recognises a shell command or a page script as money
//! (`agent-core`) and by the managed browser when it blocks the same hosts at the network
//! (`builtin-tools`). A match is a positive signal only: an unknown API, or a host spelled in a
//! way a script computes at run time, is not recognised here (the browser's network block
//! catches the second kind while an unapproved script runs).

/// Hosts of payment, banking, crypto-exchange and domain-purchase APIs. A command or script that
/// names one of them (`curl`, `wget`, `python -c`, `node -e`, `fetch(...)`) is treated as a
/// payment call even when it only reads: when unsure, ask.
pub const PAYMENT_HOSTS: &[&str] = &[
    "api.stripe.com",
    "checkout.stripe.com",
    "buy.stripe.com",
    "api.paypal.com",
    "api-m.paypal.com",
    "paypal.com/v1/payments",
    "connect.squareup.com",
    "api.squareup.com",
    "api.braintreegateway.com",
    "payments.braintree-api.com",
    "checkout-live.adyen.com",
    "api.mollie.com",
    "api.razorpay.com",
    "api.paddle.com",
    "vendors.paddle.com",
    "api.lemonsqueezy.com",
    "api.gumroad.com",
    "api.wise.com",
    "api.transferwise.com",
    "api.revolut.com",
    "b2b.revolut.com",
    "api.gocardless.com",
    "api.klarna.com",
    "api.plaid.com",
    "api.coinbase.com",
    "api.exchange.coinbase.com",
    "api.binance.com",
    "api.kraken.com",
    "api.godaddy.com/v1/domains/purchase",
    "api.namecheap.com",
    "api.iyzipay.com",
    "api.iyzico.com",
    "pay.google.com",
    "payments.google.com",
];

/// The hosts above that serve a hosted checkout page rather than an API. The managed browser may
/// open one (its page check decides what is done there), so the network block leaves them alone.
const CHECKOUT_PAGE_HOSTS: &[&str] = &[
    "checkout.stripe.com",
    "buy.stripe.com",
    "vendors.paddle.com",
    "pay.google.com",
    "payments.google.com",
];

/// Whether plain text (a shell command, already unquoted) names a payment host.
#[must_use]
pub fn text_names_payment_host(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    PAYMENT_HOSTS.iter().any(|h| lower.contains(h))
}

/// What a script's string literals spell once the ways of writing one out in pieces are gone:
/// quotes, `+` joins, white space, backslashes, and `\xNN` / `\uNNNN` escapes (`"api." +
/// "stripe" + ".com"`, `api\x2estripe.com`). Lower case.
fn flatten_script(script: &str) -> String {
    let chars: Vec<char> = script.to_lowercase().chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let width = match chars.get(i + 1) {
                Some('x') => 2,
                Some('u') => 4,
                _ => 0,
            };
            let decoded = (width > 0)
                .then(|| chars.get(i + 2..i + 2 + width))
                .flatten()
                .and_then(|hex| u32::from_str_radix(&hex.iter().collect::<String>(), 16).ok())
                .and_then(char::from_u32);
            if let Some(d) = decoded {
                out.push(d);
                i += 2 + width;
            } else {
                // `\/` is `/`: drop the backslash, keep what it escapes.
                i += 1;
            }
            continue;
        }
        if !(c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '+')) {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Whether a script (the JavaScript of `browser_eval`) names a payment host, in plain text or
/// written out in pieces.
#[must_use]
pub fn script_names_payment_host(script: &str) -> bool {
    text_names_payment_host(script) || text_names_payment_host(&flatten_script(script))
}

/// URL patterns (`*` matches anything) for the browser's network block: every payment API host,
/// without the hosted checkout pages.
#[must_use]
pub fn blocked_url_patterns() -> Vec<String> {
    PAYMENT_HOSTS
        .iter()
        .filter(|h| !CHECKOUT_PAGE_HOSTS.contains(h))
        .map(|h| format!("*{h}*"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_that_call_a_payment_api_are_recognised_however_they_are_written() {
        for js in [
            "fetch('https://api.stripe.com/v1/charges', {method:'POST'})",
            "new XMLHttpRequest().open('POST', \"https://API.PayPal.com/v2/payments\")",
            "navigator.sendBeacon(`https://api.revolut.com/x`, d)",
            "fetch('https://api.' + 'stripe' + '.com/v1/charges')",
            "fetch('https://api\\x2estripe.com/v1/charges')",
            "fetch('https:\\/\\/api.stripe.com\\/v1\\/charges')",
            "fetch('https://api\\u002estripe\\u002ecom/v1')",
        ] {
            assert!(script_names_payment_host(js), "{js}");
        }
        // A host computed at run time is not in the text: the browser's network block is what
        // stops that one while an unapproved script runs.
        assert!(!script_names_payment_host(
            "fetch(['https://api.str', 'ipe.com/v1'].join(''))"
        ));
        for js in [
            "document.title",
            "fetch('https://example.com/api/charges')",
            "document.querySelector('#stripe-form').submit()",
        ] {
            assert!(!script_names_payment_host(js), "{js}");
        }
    }

    #[test]
    fn the_network_block_covers_apis_and_leaves_hosted_checkout_pages_open() {
        let patterns = blocked_url_patterns();
        assert!(patterns.contains(&"*api.stripe.com*".to_owned()));
        assert!(patterns.contains(&"*paypal.com/v1/payments*".to_owned()));
        assert!(!patterns.iter().any(|p| p.contains("checkout.stripe.com")));
        assert!(!patterns.iter().any(|p| p.contains("pay.google.com")));
    }
}
