//! Money movement: the one category the Full level still asks about.
//!
//! The Full level allows every call without a card except the ones that spend money. A tool
//! that declares [`PermissionCategory::Money`] is a floor in the resolver already. The rest of
//! the surfaces cannot declare it, so they are recognised here from facts the daemon holds:
//!
//! * a shell command that starts a known payment program or contacts a known payment API;
//! * a connector action (Composio `app:<app>/<action>` and `apps_run_action`, MCP
//!   `mcp:<server>/<tool>`) of a payment app, or whose action name is a payment verb;
//! * a page in the managed browser that shows checkout (decided by the browser tool, which
//!   sees the live page; see `payment_page`).
//!
//! These are positive signals only. A charge behind a generic control, a script file, a desktop
//! window or an unknown API is not recognised: the PR text lists that as the residual risk. A
//! recognised surface is asked once per call and never covered by a grant.

use serde_json::Value;
use tool_registry::Effect;

/// Programs that move money or hold a payment account. Compared with the file name of a command
/// word, lower case, without a `.exe`/`.cmd` suffix.
const PAYMENT_PROGRAMS: &[&str] = &[
    "stripe",
    "paypal",
    "braintree",
    "adyen",
    "mollie",
    "razorpay",
    "gocardless",
    "lncli",
    "bitcoin-cli",
    "litecoin-cli",
    "electrum",
    "monero-wallet-cli",
    "wise",
    "revolut",
    "coinbase",
];

/// A program with the subcommand that sends funds (`cast send`, `solana transfer`).
const PAYMENT_SUBCOMMANDS: &[(&str, &str)] = &[
    ("cast", "send"),
    ("solana", "transfer"),
    ("near", "send"),
    ("xrpl", "payment"),
];

/// Hosts of payment, banking, crypto-exchange and domain-purchase APIs. A command that names one
/// of them (`curl`, `wget`, `python -c`, `node -e`, `httpie`) is treated as a payment call even
/// when it only reads: when unsure, ask.
const PAYMENT_HOSTS: &[&str] = &[
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

/// Payment apps (Composio app slugs, MCP server ids): every write action of one is a payment.
const PAYMENT_APPS: &[&str] = &[
    "stripe",
    "paypal",
    "square",
    "braintree",
    "adyen",
    "mollie",
    "razorpay",
    "paddle",
    "lemonsqueezy",
    "gumroad",
    "wise",
    "transferwise",
    "revolut",
    "gocardless",
    "klarna",
    "plaid",
    "coinbase",
    "binance",
    "kraken",
    "venmo",
    "cashapp",
    "iyzico",
    "iyzipay",
    "paytr",
    "shopify_payments",
];

/// Action-name words that mean money leaves the owner: split on non-alphanumerics (so `_` and `-`
/// separate words), compared whole, so `payroll_report` or `prepay_docs` do not match but `create_payment_intent` does.
const PAYMENT_VERBS: &[&str] = &[
    "pay",
    "payment",
    "payments",
    "payout",
    "payouts",
    "charge",
    "charges",
    "purchase",
    "buy",
    "checkout",
    "subscribe",
    "subscription",
    "subscriptions",
    "transfer",
    "withdraw",
    "withdrawal",
    "donate",
    "topup",
];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

/// The command's words as program names: path and `.exe` removed.
fn program_of(word: &str) -> &str {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    base.strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".cmd"))
        .or_else(|| base.strip_suffix(".bat"))
        .unwrap_or(base)
}

/// Words that run the next word as a program (`sudo stripe ...`, `npx stripe ...`).
const WRAPPERS: &[&str] = &[
    "sudo", "env", "command", "exec", "nohup", "time", "nice", "timeout", "xargs", "npx", "dlx",
    "bunx", "uvx", "pipx", "run",
];

/// The options of a wrapper that take their value as the next word (`sudo -u root`,
/// `env -C /tmp`, `nice -n 5`). A value left in place would be read as the program.
/// `env -S` and `npx -c` are not listed: their value is a command line, so it is read as one.
const WRAPPER_OPTIONS_WITH_VALUE: &[(&str, &[&str])] = &[
    (
        "sudo",
        &[
            "-u",
            "-g",
            "-h",
            "-p",
            "-C",
            "-D",
            "-R",
            "-T",
            "-U",
            "-r",
            "-t",
            "--user",
            "--group",
            "--host",
            "--prompt",
            "--chdir",
            "--role",
            "--type",
            "--other-user",
            "--close-from",
            "--command-timeout",
        ],
    ),
    (
        "env",
        &["-C", "-u", "-P", "-a", "--chdir", "--unset", "--argv0"],
    ),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"]),
    ("nice", &["-n", "--adjustment"]),
    ("time", &["-f", "-o", "--format", "--output"]),
    ("exec", &["-a"]),
    (
        "xargs",
        &[
            "-a",
            "-d",
            "-E",
            "-I",
            "-L",
            "-l",
            "-n",
            "-P",
            "-s",
            "--arg-file",
            "--delimiter",
            "--eof",
            "--replace",
            "--max-lines",
            "--max-args",
            "--max-procs",
            "--max-chars",
        ],
    ),
    ("npx", &["-p", "--package", "--node-arg"]),
    ("uvx", &["--from", "--with", "-p", "--python"]),
    ("pipx", &["--python", "--spec", "--index-url"]),
];

fn option_takes_value(wrapper: &str, option: &str) -> bool {
    !option.contains('=')
        && WRAPPER_OPTIONS_WITH_VALUE
            .iter()
            .any(|(w, options)| *w == wrapper && options.contains(&option))
}

fn is_assignment(word: &str) -> bool {
    word.contains('=') && !word.starts_with('-')
}

/// Skip a wrapper's own options, the values they take, `VAR=value` words (`env`) and the
/// duration `timeout` takes first. Returns the index of the word that follows them.
fn skip_wrapper_arguments(wrapper: &str, words: &[&str], mut i: usize) -> usize {
    let mut duration_pending = wrapper == "timeout";
    while let Some(&w) = words.get(i) {
        if w == "--" {
            return i + 1;
        }
        if w.len() > 1 && w.starts_with('-') {
            i += 1 + usize::from(option_takes_value(wrapper, w));
        } else if is_assignment(w) {
            i += 1;
        } else if duration_pending && w.starts_with(|c: char| c.is_ascii_digit()) {
            duration_pending = false;
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Shells whose `-c` payload is a command line of its own.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "cmd",
    "powershell",
    "pwsh",
];

/// The program a command segment starts, after wrappers (and their options) and `VAR=value`
/// words, with the words that follow it.
fn segment_program<'a>(words: &[&'a str]) -> Option<(String, Vec<&'a str>)> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let prog = program_of(w).to_ascii_lowercase();
        if is_assignment(w) || (w.starts_with('-') && i > 0) {
            i += 1;
        } else if WRAPPERS.contains(&prog.as_str()) {
            i = skip_wrapper_arguments(&prog, words, i + 1);
        } else {
            return Some((prog, words[i + 1..].to_vec()));
        }
    }
    None
}

fn segments_move_money(command: &str, depth: u8) -> bool {
    command
        .split(['\n', ';', '&', '|'])
        .map(|seg| seg.trim().trim_matches(['(', ')', '{', '}', '`']).trim())
        .filter(|seg| !seg.is_empty())
        .any(|seg| {
            let words: Vec<&str> = seg.split_whitespace().collect();
            let Some((prog, rest)) = segment_program(&words) else {
                return false;
            };
            if PAYMENT_PROGRAMS.contains(&prog.as_str()) {
                return true;
            }
            if PAYMENT_SUBCOMMANDS.iter().any(|(p, sub)| {
                prog == *p
                    && rest
                        .iter()
                        .take(3)
                        .any(|next| next.eq_ignore_ascii_case(sub))
            }) {
                return true;
            }
            // `sh -c "stripe ..."`: the payload is a command line of its own.
            depth < 4
                && SHELLS.contains(&prog.as_str())
                && rest
                    .iter()
                    .position(|w| !w.starts_with('-') && !w.starts_with('/'))
                    .is_some_and(|at| {
                        let payload = rest[at..].join(" ");
                        segments_move_money(&payload, depth + 1)
                    })
        })
}

/// What the shell would read once its quoting is gone, in two spellings: a backslash escapes
/// the next character (`\stripe`, `s\tripe`) or, on Windows, separates a path
/// (`C:\tools\stripe.exe`). A quote only joins what it surrounds (`"stripe"`, `str""ipe`,
/// `$'stripe'`). Both are read: the command is a payment if either spelling is. The case is
/// kept: wrapper options differ by case (`sudo -H` takes no value, `sudo -h` takes one).
fn unquoted(command: &str) -> [String; 2] {
    let plain = command.replace("$'", "'").replace("$\"", "\"");
    let without_quotes = plain.replace(['"', '\''], "");
    let escapes_removed = without_quotes.replace('\\', "");
    [without_quotes, escapes_removed]
}

/// A shell command that starts a payment program or names a payment API.
#[must_use]
pub(crate) fn command_moves_money(command: &str) -> bool {
    let spellings = unquoted(command);
    PAYMENT_HOSTS.iter().any(|h| {
        command.to_ascii_lowercase().contains(h)
            || spellings.iter().any(|s| s.to_ascii_lowercase().contains(h))
    }) || spellings.iter().any(|s| segments_move_money(s, 0))
}

/// A connector action that pays: the app is a payment app, or the action name has a payment verb.
/// Reads never count (they cannot spend); `effect` is the tool's declared effect.
#[must_use]
pub(crate) fn connector_moves_money(name: &str, effect: Effect, input: &Value) -> bool {
    // `app:<app>/<action>`, `mcp:<server>/<tool>`, or `apps_run_action` with app and action in
    // the input. Anything else is not a connector.
    let (app, action) = if let Some(rest) = name.strip_prefix("app:") {
        rest.split_once('/').unwrap_or((rest, ""))
    } else if let Some(rest) = name
        .strip_prefix("mcp:")
        .or_else(|| name.strip_prefix("mcp/"))
    {
        rest.split_once('/').unwrap_or((rest, ""))
    } else if name == "apps_run_action" {
        (
            input.get("app").and_then(Value::as_str).unwrap_or_default(),
            input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    } else {
        return false;
    };
    if effect == Effect::Read {
        return false;
    }
    let app_words: Vec<String> = words(app).collect();
    let app_flat = app.to_ascii_lowercase().replace(['-', '.'], "_");
    if PAYMENT_APPS.iter().any(|a| {
        app_flat == *a || app_flat.starts_with(&format!("{a}_")) || app_words.iter().any(|w| w == a)
    }) {
        return true;
    }
    let action_words: Vec<String> = words(action).collect();
    action_words
        .iter()
        .any(|w| PAYMENT_VERBS.contains(&w.as_str()))
        || action_words
            .windows(2)
            .any(|p| p[0] == "send" && matches!(p[1].as_str(), "money" | "funds" | "payment"))
        || action_words
            .windows(2)
            .any(|p| p[0] == "place" && p[1] == "order")
        || action_words
            .windows(2)
            .any(|p| p[0] == "create" && matches!(p[1].as_str(), "order" | "charge" | "payment"))
}

/// Whether this call is a payment the daemon can recognise without looking at a page.
#[must_use]
pub(crate) fn call_moves_money(
    name: &str,
    effect: Effect,
    raw_command: &str,
    input: &Value,
) -> bool {
    (!raw_command.is_empty() && command_moves_money(raw_command))
        || connector_moves_money(name, effect, input)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn payment_programs_and_hosts_are_recognised_in_a_command() {
        for cmd in [
            "stripe charges create --amount 500 --currency usd",
            "/usr/local/bin/stripe listen",
            "paypal.exe pay",
            "curl -s https://api.stripe.com/v1/charges -d amount=100",
            "python3 -c \"import requests; requests.post('https://api-m.paypal.com/v2/checkout/orders')\"",
            "cd /tmp && lncli payinvoice lnbc1",
            "cast send 0xabc --value 1ether",
            "solana transfer ADDR 1",
            "sudo -n npx stripe refunds create",
            "FOO=1 bash -c \"cd x && stripe charges create\"",
        ] {
            assert!(command_moves_money(cmd), "{cmd}");
        }
        // A word in a message, a file name or a package list is not a program.
        for cmd in [
            "ls -la",
            "npm install stripe",
            "git commit -m 'add stripe integration'",
            "cast call 0xabc balanceOf",
            "curl https://example.com/pricing",
            "echo payment done | tee out.txt",
        ] {
            assert!(!command_moves_money(cmd), "{cmd}");
        }
    }

    #[test]
    fn quoting_escapes_and_wrapper_options_do_not_hide_a_payment_program() {
        for cmd in [
            "\"stripe\" charges create",
            "'stripe' charges create",
            "\\stripe charges create",
            "s\\tripe charges create",
            "str\"\"ipe charges create",
            "$'stripe' charges create",
            "\"/usr/local/bin/stripe\" listen",
            "C:\\tools\\stripe.exe charges create",
            "sudo -u root stripe charges create",
            "sudo --user root -n stripe refunds create",
            "timeout 10 stripe listen",
            "timeout -s KILL 5s stripe listen",
            "env -C /tmp stripe charges create",
            "env -u HOME FOO=1 stripe charges create",
            "nice -n 5 stripe charges create",
            "xargs -I {} stripe charges retrieve {}",
            "nohup sudo -u root timeout 60 \"stripe\" listen &",
            "bash -c \"sudo -u root 'stripe' charges create\"",
            "env -S \"stripe charges create\"",
            "sudo -H stripe charges create",
            "SUDO -H STRIPE charges create",
            "sudo -E -H -n stripe charges create",
            "CAST SEND 0xabc --value 1ether",
            "cmd /c \"C:\\tools\\stripe.exe\" charges create",
        ] {
            assert!(command_moves_money(cmd), "{cmd}");
        }
        // The same wrappers around an ordinary program, and a payment word that is only data.
        for cmd in [
            "sudo -u root ls /var",
            "timeout 10 curl https://example.com",
            "env -C /tmp make build",
            "env FOO=1 node app.js",
            "nice -n 5 cargo build",
            "git commit -m \"add stripe integration\"",
            "echo 'stripe charges create' > notes.txt",
            "sudo -u stripe ls",
        ] {
            assert!(!command_moves_money(cmd), "{cmd}");
        }
    }

    #[test]
    fn connector_actions_of_payment_apps_or_with_payment_verbs_are_money() {
        let none = json!({});
        assert!(connector_moves_money(
            "app:stripe/create_charge",
            Effect::Destructive,
            &none
        ));
        assert!(connector_moves_money(
            "app:shopify/place_order",
            Effect::Destructive,
            &none
        ));
        assert!(connector_moves_money(
            "app:acme/PAYMENTS_CREATE_PAYOUT",
            Effect::Destructive,
            &none
        ));
        assert!(connector_moves_money(
            "apps_run_action",
            Effect::Destructive,
            &json!({"app": "paypal", "action": "get_balance"})
        ));
        assert!(connector_moves_money(
            "mcp:stripe-prod/refund",
            Effect::Write,
            &none
        ));
        assert!(!connector_moves_money(
            "app:gmail/send_email",
            Effect::Destructive,
            &none
        ));
        assert!(!connector_moves_money(
            "app:slack/post_message",
            Effect::Destructive,
            &none
        ));
        assert!(!connector_moves_money(
            "app:hubspot/payroll_report",
            Effect::Destructive,
            &none
        ));
        assert!(!connector_moves_money(
            "app:stripe/list_charges",
            Effect::Read,
            &none
        ));
        assert!(!connector_moves_money("browser_open", Effect::Write, &none));
    }
}
