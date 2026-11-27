//! Money movement: the one category the Full level still asks about.
//!
//! The Full level allows every call without a card except the ones that spend money. A tool
//! that declares [`PermissionCategory::Money`] is a floor in the resolver already. The rest of
//! the surfaces cannot declare it, so they are recognised here from facts the daemon holds:
//!
//! * a shell command that starts a known payment program or contacts a known payment API, however
//!   it is nested (a command substitution, an interpreter's code argument, `ssh host ...`,
//!   `docker exec ...`, a PowerShell cmdlet);
//! * a page script (`browser_eval`) that names a known payment API;
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
    "stripe-cli",
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

/// Programs that run the text they are given as a command line: a payment program after one of
/// them is a payment (`bash -lc "..."`, `bash <<< "..."`, `cmd /c ...`, `eval ...`).
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "csh",
    "tcsh",
    "busybox",
    "cmd",
    "powershell",
    "pwsh",
    "eval",
];

/// Programs that run another command given among their arguments, at a position this scanner
/// does not model (after options with values, a host, a container name): `ssh host stripe ...`,
/// `docker exec c stripe ...`, `kubectl exec pod -- stripe ...`, `Start-Process stripe`,
/// `find . -exec stripe {} \;`. A payment program anywhere after one of them is a payment.
const CARRIERS: &[&str] = &[
    "ssh",
    "sshpass",
    "mosh",
    "docker",
    "podman",
    "nerdctl",
    "kubectl",
    "oc",
    "lxc",
    "incus",
    "nsenter",
    "chroot",
    "su",
    "runuser",
    "setpriv",
    "doas",
    "pkexec",
    "watch",
    "find",
    "parallel",
    "flatpak",
    "distrobox",
    "toolbox",
    "wsl",
    "screen",
    "tmux",
    "systemd-run",
    "script",
    "strace",
    "ltrace",
    "gdb",
    "valgrind",
    "at",
    "batch",
    // PowerShell cmdlets and their aliases that start a program or run a string as code.
    "invoke-expression",
    "iex",
    "start-process",
    "saps",
    "invoke-command",
    "icm",
    "invoke-item",
    "ii",
    "start",
    "call",
];

/// Language runtimes that run the code in an argument (`python -c`, `node -e`, `perl -e`).
const INTERPRETERS: &[&str] = &[
    "python",
    "py",
    "pypy",
    "node",
    "nodejs",
    "bun",
    "deno",
    "ruby",
    "perl",
    "php",
    "lua",
    "rscript",
    "osascript",
];

/// A program name without a version suffix (`python3.12` is `python`, `perl5` is `perl`).
fn interpreter_name(prog: &str) -> &str {
    prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.')
}

fn is_interpreter(prog: &str) -> bool {
    INTERPRETERS.contains(&interpreter_name(prog))
}

/// Words that are the name of a person or a thing as often as a program: only a program at the
/// start of a command counts, never a word somewhere in a script's text.
const AMBIGUOUS_WORDS: &[&str] = &["wise"];

/// Whether a command starting at `prog` (with the words after it) is a payment program, or a
/// program with the subcommand that sends funds.
fn is_payment_start(prog: &str, rest: &[&str]) -> bool {
    PAYMENT_PROGRAMS.contains(&prog)
        || PAYMENT_SUBCOMMANDS.iter().any(|(p, sub)| {
            prog == *p
                && rest
                    .iter()
                    .take(3)
                    .any(|next| next.eq_ignore_ascii_case(sub))
        })
}

/// Whether the text of a script (the code argument of an interpreter) names a payment program
/// or a payment subcommand as a word: `subprocess.run(['stripe', ...])`, `require('stripe')`,
/// `import stripe`, `os.system("lncli payinvoice ...")`.
fn code_moves_money(code: &str) -> bool {
    let words: Vec<String> = code
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    words
        .iter()
        .any(|w| PAYMENT_PROGRAMS.contains(&w.as_str()) && !AMBIGUOUS_WORDS.contains(&w.as_str()))
        || words
            .windows(2)
            .any(|p| PAYMENT_SUBCOMMANDS.contains(&(p[0].as_str(), p[1].as_str())))
}

/// Whether `word` is the flag that hands an interpreter its code (`-c`, `-e`, `-E`, `-p`, `-r`,
/// `-pe`, `--eval`, `--eval=...`, `deno eval`). `Some(inline)` is the code written in the same
/// word (`--eval=...`), empty otherwise.
fn code_flag<'a>(interpreter: &str, word: &'a str) -> Option<&'a str> {
    if let Some(long) = word.strip_prefix("--") {
        let (name, value) = long.split_once('=').unwrap_or((long, ""));
        return matches!(name, "eval" | "print" | "command").then_some(value);
    }
    if interpreter == "deno" && word == "eval" {
        return Some("");
    }
    word.strip_prefix('-')
        .filter(|flags| {
            !flags.is_empty()
                && flags.chars().all(|c| c.is_ascii_alphabetic())
                && flags.ends_with(['c', 'e', 'E', 'p', 'r'])
        })
        .map(|_| "")
}

/// The code an interpreter is told to run, as far as the words (quotes already gone) show it:
/// everything from its code flag on.
fn interpreter_code(prog: &str, rest: &[&str]) -> Option<String> {
    let name = interpreter_name(prog);
    let at = rest.iter().position(|w| code_flag(name, w).is_some())?;
    Some(rest[at..].join(" "))
}

/// The command's words with the shell's quoting applied: a quoted span is part of one word, and
/// its quotes are gone. The code of `python -c "a; b"` is one word here, where the flattened
/// reading above would have cut it at the `;`.
fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let (mut cur, mut in_word, mut quote) = (String::new(), false, None::<char>);
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (Some(q), c) if c == q => quote = None,
            (None | Some('"'), '\\') if matches!(chars.clone().next(), Some('"' | '\'' | '\\')) => {
                cur.extend(chars.next());
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (_, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

/// An interpreter whose code argument (read with its quoting intact) names a payment program.
fn quoted_code_moves_money(command: &str) -> bool {
    let words = shell_words(command);
    words.iter().enumerate().any(|(at, w)| {
        let prog = program_of(w).to_ascii_lowercase();
        let name = interpreter_name(&prog);
        is_interpreter(&prog)
            && words[at + 1..].iter().enumerate().any(|(i, word)| {
                code_flag(name, word).is_some_and(|inline| {
                    code_moves_money(inline)
                        || words
                            .get(at + 2 + i)
                            .is_some_and(|code| code_moves_money(code))
                })
            })
    })
}

/// The base64 payload of a PowerShell `-EncodedCommand` (or a prefix of it: `-e`, `-enc`), as the
/// text it decodes to. `Err` when the payload cannot be read: an opaque command is not trusted.
fn powershell_encoded(rest: &[&str]) -> Option<Result<String, ()>> {
    use base64::Engine as _;
    let at = rest.iter().position(|w| {
        let w = w.to_ascii_lowercase();
        w.strip_prefix('-')
            .is_some_and(|f| f.starts_with('e') && "encodedcommand".starts_with(f))
    })?;
    let decoded = rest
        .get(at + 1)
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .map(|bytes| {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        });
    Some(decoded.ok_or(()))
}

/// The program a command segment starts, after wrappers (and their options) and `VAR=value`
/// words, with the words that follow it.
fn segment_program<'w, 'a>(words: &'w [&'a str]) -> Option<(String, &'w [&'a str])> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let prog = program_of(w).to_ascii_lowercase();
        if is_assignment(w) || (w.starts_with('-') && i > 0) {
            i += 1;
        } else if WRAPPERS.contains(&prog.as_str()) {
            i = skip_wrapper_arguments(&prog, words, i + 1);
        } else {
            return Some((prog, &words[i + 1..]));
        }
    }
    None
}

/// One command of a command line, and whether it reads what the one before it printed (`a | b`).
struct Segment {
    text: String,
    piped: bool,
}

/// The command line cut at everything that starts another command: a new line, `;`, `&`, `&&`,
/// `||`, `|`, a subshell or command substitution (`(`, `)`, `$(`, a backtick).
fn segments(command: &str) -> Vec<Segment> {
    fn end(out: &mut Vec<Segment>, cur: &mut String, piped: &mut bool) {
        let text = cur.trim().trim_matches(['(', ')', '{', '}', '`']).trim();
        if !text.is_empty() {
            out.push(Segment {
                text: text.to_owned(),
                piped: *piped,
            });
            *piped = false;
        }
        cur.clear();
    }
    let chars: Vec<char> = command.chars().collect();
    let (mut out, mut cur, mut piped) = (Vec::new(), String::new(), false);
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '|' => {
                end(&mut out, &mut cur, &mut piped);
                let or = chars.get(i + 1) == Some(&'|');
                piped = !or;
                i += if or || chars.get(i + 1) == Some(&'&') {
                    2
                } else {
                    1
                };
            }
            '\n' | ';' | '&' | '(' | ')' | '`' => {
                end(&mut out, &mut cur, &mut piped);
                i += 1;
            }
            c => {
                cur.push(c);
                i += 1;
            }
        }
    }
    end(&mut out, &mut cur, &mut piped);
    out
}

/// A segment that runs what it is fed on its standard input as code: a shell or an interpreter
/// with no code argument or script of its own, a remote shell, a container's shell.
fn reads_code_from_stdin(prog: &str, rest: &[&str]) -> bool {
    let runs_code = |p: &str| SHELLS.contains(&p) || is_interpreter(p);
    (runs_code(prog) || CARRIERS.contains(&prog))
        && (matches!(prog, "ssh" | "sshpass" | "mosh" | "at" | "batch")
            || rest.iter().all(|w| w.starts_with(['-', '<']))
            || rest
                .iter()
                .any(|w| runs_code(&program_of(w).to_ascii_lowercase())))
}

/// How deep a PowerShell `-EncodedCommand` may hide another one.
const MAX_ENCODED_DEPTH: u8 = 3;

fn segments_move_money(command: &str, depth: u8) -> bool {
    let segments = segments(command);
    let mut feeds_code = false;
    for seg in &segments {
        let words: Vec<&str> = seg.text.split_whitespace().collect();
        let Some((prog, rest)) = segment_program(&words) else {
            continue;
        };
        if is_payment_start(&prog, rest) {
            return true;
        }
        if SHELLS.contains(&prog.as_str()) || CARRIERS.contains(&prog.as_str()) {
            // Any word after it may be the command it runs.
            let from_here = (0..rest.len()).any(|at| {
                segment_program(&rest[at..]).is_some_and(|(inner, after)| {
                    is_payment_start(&inner, after)
                        || (is_interpreter(&inner)
                            && interpreter_code(&inner, after)
                                .is_some_and(|code| code_moves_money(&code)))
                })
            });
            if from_here {
                return true;
            }
        }
        if matches!(prog.as_str(), "powershell" | "pwsh")
            && let Some(encoded) = powershell_encoded(rest)
        {
            match encoded {
                Ok(text) if depth < MAX_ENCODED_DEPTH => {
                    if command_moves_money_at(&text, depth + 1) {
                        return true;
                    }
                }
                // Unreadable, or too deeply nested to read: an opaque command is asked about.
                _ => return true,
            }
        }
        if is_interpreter(&prog)
            && interpreter_code(&prog, rest).is_some_and(|code| code_moves_money(&code))
        {
            return true;
        }
        // A pipe or a here-document / here-string hands it its code.
        feeds_code |= (seg.piped || seg.text.contains("<<")) && reads_code_from_stdin(&prog, rest);
    }
    // `echo stripe charges create | sh`: the text on the left is the code on the right.
    feeds_code && code_moves_money(command)
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

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The longest `${...}` body that is read as a variable name and its default.
const MAX_EXPANSION_NAME: usize = 256;

/// What a `$` at `chars[at]` introduces.
enum Expansion {
    /// `$name` / `${name}`: the body and how many characters the expansion spans.
    Named(String, usize),
    /// A `${` whose `}` is not within [`MAX_EXPANSION_NAME`] characters: what it expands to
    /// cannot be told, and leaving it literal could hide a program name (`s${<300 a>}tripe`).
    Opaque,
    /// Something else (`$(`, `$'`, a lone `$`).
    Other,
}

fn expansion_at(chars: &[char], at: usize) -> Expansion {
    match chars.get(at + 1) {
        Some('{') => {
            let close = chars[at + 2..]
                .iter()
                .take(MAX_EXPANSION_NAME)
                .position(|&c| c == '}');
            match close {
                Some(close) => {
                    Expansion::Named(chars[at + 2..at + 2 + close].iter().collect(), close + 3)
                }
                None => Expansion::Opaque,
            }
        }
        Some(c) if is_name_char(*c) => {
            let len = chars[at + 1..]
                .iter()
                .take_while(|c| is_name_char(**c))
                .count();
            Expansion::Named(chars[at + 1..at + 1 + len].iter().collect(), len + 1)
        }
        _ => Expansion::Other,
    }
}

/// Whether the text holds a `${` whose name is opaque or unterminated.
fn has_opaque_expansion(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len())
        .any(|i| chars[i] == '$' && matches!(expansion_at(&chars, i), Expansion::Opaque))
}

/// `NAME=value` words of the command line, in order.
fn assignments(text: &str) -> Vec<(String, String)> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .filter_map(|w| w.split_once('='))
        .filter(|(name, _)| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(is_name_char)
        })
        .map(|(n, v)| (n.to_owned(), v.to_owned()))
        .collect()
}

/// The text with every `$name` / `${name}` replaced: by the value the same command line
/// assigned to it (`p=stripe; $p charges create`), or, with `known` off, by nothing (an unset
/// variable: `s${x}tripe`).
fn expand_variables(text: &str, vars: &[(String, String)], known: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$'
            && let Expansion::Named(body, span) = expansion_at(&chars, i)
        {
            // `${NAME:-default}`: the default is what an unset variable becomes.
            let (name, default) = body.split_once(":-").unwrap_or((body.as_str(), ""));
            let value = known
                .then(|| vars.iter().rev().find(|(n, _)| n == name))
                .flatten()
                .map_or(default, |(_, v)| v.as_str());
            out.push_str(value);
            i += span;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The text with every `+` closed up with the white space around it: `"stri" + "pe"` is
/// `stripe` once the quotes are gone.
fn join_plus(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skip_space = false;
    for c in text.chars() {
        if c == '+' {
            out.truncate(out.trim_end().len());
            skip_space = true;
        } else if !(skip_space && c.is_whitespace()) {
            skip_space = false;
            out.push(c);
        }
    }
    out
}

/// Every spelling of the command a shell could read it as: unquoted two ways, `${IFS}` as the
/// space it stands for, the command line's own assignments filled in, variables read as empty
/// (`s${x}tripe`) and `+` joins closed.
fn spellings(command: &str) -> Vec<String> {
    let [without_quotes, escapes_removed] = unquoted(command);
    let mut out = vec![command.to_owned()];
    for base in [without_quotes, escapes_removed] {
        let spaced = base.replace("${IFS}", " ").replace("$IFS", " ");
        let vars = assignments(&spaced);
        let filled = expand_variables(&expand_variables(&spaced, &vars, true), &vars, true);
        out.push(expand_variables(&spaced, &vars, false));
        out.push(filled);
        // PowerShell and JavaScript join a program name out of pieces with `+`.
        out.push(join_plus(&spaced));
        out.push(spaced);
        out.push(base);
    }
    out
}

fn command_moves_money_at(command: &str, depth: u8) -> bool {
    // A `${` that cannot be read could be spelling anything: an opaque command is asked about.
    has_opaque_expansion(command)
        || quoted_code_moves_money(command)
        || spellings(command).iter().any(|s| {
            tool_registry::payment::text_names_payment_host(s) || segments_move_money(s, depth)
        })
}

/// A shell command that starts a payment program or names a payment API.
#[must_use]
pub(crate) fn command_moves_money(command: &str) -> bool {
    command_moves_money_at(command, 0)
}

/// A page script (`browser_eval`, also under the owner's computer's `pc_` prefix) that names a
/// payment API.
#[must_use]
pub(crate) fn script_moves_money(name: &str, input: &Value) -> bool {
    name.strip_prefix("pc_").unwrap_or(name) == "browser_eval"
        && input
            .get("js")
            .and_then(Value::as_str)
            .is_some_and(tool_registry::payment::script_names_payment_host)
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
        || script_moves_money(name, input)
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
    fn an_unreadable_expansion_is_asked_about_not_left_literal() {
        let long = "a".repeat(MAX_EXPANSION_NAME + 44);
        for cmd in [
            // Too long to read as a name: could be empty, could join `stripe`.
            format!("s${{{long}}}tripe charges create"),
            // Never closed.
            "s${x tripe charges create".to_owned(),
            format!("echo ${{{long}"),
        ] {
            assert!(command_moves_money(&cmd), "{cmd}");
        }
        // A short, closed expansion is still read, and one that is no payment stays free.
        assert!(command_moves_money("s${x}tripe charges create"));
        assert!(!command_moves_money("echo ${HOME} ${x:-fallback}"));
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

    /// Second review, finding 2: a payment program nested in a command substitution, a variable,
    /// an interpreter's code argument, a PowerShell cmdlet, a remote or container command or a
    /// pipe into a shell is still a payment.
    #[test]
    fn nested_payment_programs_are_recognised() {
        use base64::Engine as _;
        let utf16: Vec<u8> = "stripe charges create"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let encoded = format!(
            "powershell -NoProfile -EncodedCommand {}",
            base64::engine::general_purpose::STANDARD.encode(utf16)
        );
        for cmd in [
            // command substitution, backticks, parameter expansion, variables
            "echo $(stripe charges create)",
            "echo \"done: $(stripe charges create --amount 5)\"",
            "echo `stripe charges create`",
            "x=$(stripe charges create); echo $x",
            "cat <(stripe charges list)",
            "echo ${P:-stripe} && ${P:-stripe} charges create",
            "s${x}tripe charges create",
            "p=stripe; $p charges create",
            "a=stri; b=pe; $a$b charges create",
            "export P=stripe && $P refunds create",
            "stripe${IFS}charges${IFS}create",
            // interpreters
            "python3 -c \"import subprocess; subprocess.run(['stripe','charges','create'])\"",
            "python3.12 -c 'import stripe; stripe.Charge.create(amount=5)'",
            "python -c 'import os; os.system(\"lncli payinvoice lnbc1\")'",
            "env -i /usr/bin/python3 -c \"import stripe\"",
            "node -e \"require('stripe')('sk').charges.create({})\"",
            "node --eval=\"require('stripe')\"",
            "bun -e \"await Bun.spawn(['stripe','charges','create'])\"",
            "deno eval \"new Deno.Command('stripe')\"",
            "ruby -e 'system(\"stripe charges create\")'",
            "perl -e 'exec \"stripe\", \"charges\"'",
            "perl -pe 's/x/y/' -e 'system(q(stripe))'",
            "php -r \"passthru('stripe charges create');\"",
            "python3 -c \"import subprocess; subprocess.run(['cast','send','0xabc'])\"",
            // shells and PowerShell
            "pwsh -c \"stripe charges create\"",
            "powershell -NoProfile -Command stripe charges create",
            encoded.as_str(),
            "powershell -enc not-base64!",
            "Invoke-Expression 'stripe charges create'",
            "iex \"stripe charges create\"",
            "iex (\"stri\" + \"pe charges create\")",
            "Start-Process stripe -ArgumentList 'charges create'",
            "Start-Process -FilePath \"C:\\tools\\stripe.exe\"",
            "bash <<< \"stripe charges create\"",
            "eval \"stripe charges create\"",
            "bash -lc 'stripe charges create'",
            "ksh -c 'stripe charges create'",
            // remote and container commands
            "ssh host stripe charges create",
            "ssh -p 2222 -i key user@host 'cd /srv && stripe charges create'",
            "docker exec -it web stripe charges create",
            "docker run --rm stripe/stripe-cli charges create",
            "kubectl exec pod -c app -- stripe refunds create",
            "tmux new -d 'stripe charges create'",
            "watch -n 5 stripe balance",
            "find . -name x -exec stripe charges create {} \\;",
            "timeout 5 ssh host stripe x",
            // a pipe into something that runs what it reads
            "echo 'stripe charges create' | sh",
            "printf stripe charges create | bash -s",
            "echo 'stripe charges create' | ssh host",
            "echo 'stripe charges create' | python3",
            "echo stripe charges create | docker exec -i web sh",
        ] {
            assert!(command_moves_money(cmd), "{cmd}");
        }
        // The same constructs around ordinary commands, and payment words that are only data.
        for cmd in [
            "echo $(date)",
            "echo $(git rev-parse HEAD) > rev.txt",
            "x=$(ls); echo $x",
            "echo `whoami`",
            "echo ${HOME}",
            "python3 manage.py migrate",
            "python -m pip install stripe",
            "python3 -c \"print('be wise')\"",
            "python3 -c \"print(1 + 1)\"",
            "node app.js --port 3000 --print-config",
            "node -r dotenv/config app.js",
            "perl -pi -e 's/a/b/' file.txt",
            "ssh host ls -la /srv",
            "ssh-keygen -t ed25519",
            "docker build -t shop .",
            "docker run --rm node:20 node build.js",
            "kubectl get pods -n stripe-prod",
            "tmux new -d 'npm run dev'",
            "find . -name '*.rs' -exec grep -l todo {} \\;",
            "cat notes.txt | sh",
            "curl -fsSL https://get.docker.com | sh",
            "echo hello | tee out.txt | wc -l",
            "name=stripe; npm install $name",
            "powershell -Command Get-ChildItem",
            "Start-Process notepad",
        ] {
            assert!(!command_moves_money(cmd), "{cmd}");
        }
    }

    /// Second review, finding 1: a page script that names a payment API is money on any page.
    #[test]
    fn a_page_script_that_names_a_payment_api_is_money() {
        let call =
            |name: &str, js: &str| call_moves_money(name, Effect::Write, "", &json!({ "js": js }));
        for js in [
            "fetch('https://api.stripe.com/v1/charges', {method:'POST', body:'amount=500'})",
            "const x = new XMLHttpRequest(); x.open('POST', 'https://api-m.paypal.com/v2/checkout/orders');",
            "navigator.sendBeacon('https://api.revolut.com/api/1.0/transfer', body)",
            "fetch('https://api.' + 'stripe' + '.com/v1/charges')",
        ] {
            assert!(call("browser_eval", js), "{js}");
            assert!(call("pc_browser_eval", js), "{js}");
        }
        assert!(!call("browser_eval", "document.title"));
        assert!(!call(
            "browser_eval",
            "fetch('https://example.com/api/charges')"
        ));
        // Only a script tool carries a script: the same text typed into a field is data.
        assert!(!call(
            "browser_fill",
            "fetch('https://api.stripe.com/v1/charges')"
        ));
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
