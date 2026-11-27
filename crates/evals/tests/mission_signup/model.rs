//! The scripted model: a deterministic stand-in for a careful assistant. It never invents what a
//! tool will say. It reads the mission brief and the tool results of its own session out of the
//! conversation (refs from the page snapshot, the secret name from `account_register`, the link
//! from the mail) and picks the next call, so the order of sessions, step cuts and wakes can
//! change without breaking the script.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use provider_catalog::{
    ChatChunk, ChatProvider, ChatRequest, FinishReason, MessageRole, ProviderError, ToolCall, Usage,
};
use regex::Regex;
use serde_json::{Value, json};

use super::sites::{CAPTCHA, PLAIN, TWOFA, VERIFY};

pub struct SignupModel {
    /// The script hit something it could not read: the first such message.
    failure: Mutex<Option<String>>,
    requests: Mutex<Vec<ChatRequest>>,
    calls: AtomicU32,
}

/// One tool call of the current session and what it returned.
struct Call {
    name: String,
    args: Value,
    result: String,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn call(name: &str, args: &Value) -> Vec<ChatChunk> {
    vec![ChatChunk::ToolCall(ToolCall {
        id: format!("call-{}", uuid_like()),
        name: name.to_owned(),
        arguments: args.to_string(),
        provider_extra: None,
    })]
}

fn uuid_like() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!("{:06}", N.fetch_add(1, Ordering::SeqCst))
}

fn say(t: &str) -> Vec<ChatChunk> {
    vec![ChatChunk::TextDelta(t.to_owned())]
}

fn find<'a>(pat: &'static str, hay: &'a str) -> Option<regex::Captures<'a>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<&'static str, Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let re = cache
        .lock()
        .entry(pat)
        .or_insert_with(|| Regex::new(pat).unwrap())
        .clone();
    // `captures` borrows `hay`, not the regex.
    re.captures(hay)
}

/// The ref of the element named `label` in a page snapshot.
fn ref_of(page: &str, label: &str) -> String {
    let want = format!("\"{label}\"");
    page.lines()
        .find(|l| l.starts_with("[e") && l.contains(&want))
        .and_then(|l| l[1..].split_once(']').map(|(r, _)| r.to_owned()))
        .unwrap_or_else(|| panic!("no element \"{label}\" in the page:\n{page}"))
}

/// The ref of the first element with `role` in a page snapshot (a masked key has no name worth
/// asking for: the page shows it as `[REDACTED 2FA setup key]`).
fn ref_of_role(page: &str, role: &str) -> String {
    page.lines()
        .find(|l| {
            l.starts_with("[e") && l.split_once("] ").is_some_and(|(_, r)| r.starts_with(role))
        })
        .and_then(|l| l[1..].split_once(']').map(|(r, _)| r.to_owned()))
        .unwrap_or_else(|| panic!("no {role} in the page:\n{page}"))
}

/// Heading and first paragraph of a page: what an honest ledger entry says.
fn evidence(page: &str) -> String {
    let heading = page
        .lines()
        .find_map(|l| l.strip_prefix("heading \"")?.strip_suffix('"'))
        .unwrap_or("");
    let first = page
        .lines()
        .find_map(|l| l.strip_prefix("text \"")?.strip_suffix('"'))
        .unwrap_or("");
    format!("{heading}. {first}")
}

fn open_keys(brief: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut in_open = false;
    for line in brief.lines() {
        if line.starts_with("NEXT ITEMS") || line.starts_with("ANSWERED BY THE OWNER") {
            in_open = true;
            continue;
        }
        if line.trim().is_empty() {
            in_open = false;
        }
        if in_open
            && let Some(rest) = line.strip_prefix("- \"")
            && let Some(end) = rest.find('"')
        {
            keys.push(rest[..end].to_owned());
        }
    }
    keys
}

impl SignupModel {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            failure: Mutex::default(),
            requests: Mutex::default(),
            calls: AtomicU32::new(0),
        })
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().clone()
    }

    /// Why the script gave up, if it did.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().clone()
    }

    pub fn model_calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    fn decide(&self, req: &ChatRequest) -> Vec<ChatChunk> {
        // The closing call after a safety limit has no tools.
        if req.tools.is_empty() {
            return say("Stopped by a safety limit; the progress is in the ledger.");
        }
        let brief_at = req.messages.iter().rposition(|m| {
            m.role == MessageRole::User
                && m.content
                    .as_str()
                    .is_some_and(|t| t.starts_with("MISSION BRIEF"))
        });
        let Some(at) = brief_at else {
            return Self::owner_turn(req);
        };
        if std::env::var_os("EVAL_DEBUG").is_some() {
            let names: Vec<_> = req
                .tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect();
            eprintln!("TOOLS OFFERED: {names:?}");
        }
        let brief = text_of(&req.messages[at].content);
        // The tool calls of this session, with their results.
        let mut calls: Vec<Call> = Vec::new();
        for m in &req.messages[at + 1..] {
            if m.role == MessageRole::Assistant {
                for c in &m.tool_calls {
                    calls.push(Call {
                        name: c.name.clone(),
                        args: serde_json::from_str(&c.arguments).unwrap_or_default(),
                        result: format!("<pending {}>", c.id),
                    });
                }
            } else if m.role == MessageRole::Tool
                && let Some(id) = &m.tool_call_id
                && let Some(c) = calls
                    .iter_mut()
                    .find(|c| c.result == format!("<pending {id}>"))
            {
                c.result = text_of(&m.content);
            }
        }
        Self::mission_turn(&brief, &calls)
    }

    /// The owner's own chat: start a mission, once.
    fn owner_turn(req: &ChatRequest) -> Vec<ChatChunk> {
        let offered = req
            .tools
            .iter()
            .any(|t| t["function"]["name"] == "mission_create");
        let started = req.messages.iter().any(|m| {
            m.role == MessageRole::Assistant
                && m.tool_calls.iter().any(|c| c.name == "mission_create")
        });
        if started {
            return say(
                "I started a mission for the four sites. The report will come when it is done.",
            );
        }
        if offered {
            return call(
                "mission_create",
                &json!({"title": "Register on four sites", "target_done": 3}),
            );
        }
        say("I cannot start a mission here.")
    }

    #[allow(clippy::too_many_lines)] // one flat decision table, read top to bottom
    fn mission_turn(brief: &str, calls: &[Call]) -> Vec<ChatChunk> {
        let mut handled: HashSet<String> = HashSet::new();
        let mut current: Option<(String, usize)> = None;
        let (mut planned, mut completed) = (false, false);
        let mut plan_keys: Vec<String> = Vec::new();
        for (i, c) in calls.iter().enumerate() {
            match c.name.as_str() {
                "mission_plan" => {
                    planned = true;
                    plan_keys = c.args["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|it| it["key"].as_str().map(str::to_owned))
                        .collect();
                }
                "mission_complete" => completed = true,
                "mission_wait" => {
                    handled.insert(c.args["item"].as_str().unwrap_or_default().to_owned());
                    current = None;
                }
                // A CAPTCHA hand-over ends the item by code: the item is given up.
                "browser_handover" => {
                    if let Some((k, _)) = current.take() {
                        handled.insert(k);
                    }
                }
                "mission_item" => {
                    let key = c.args["key"].as_str().unwrap_or_default().to_owned();
                    match c.args["status"].as_str() {
                        Some("doing") => current = Some((key, i)),
                        Some("done" | "failed" | "blocked" | "skipped") => {
                            handled.insert(key);
                            current = None;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let Some((key, at)) = current else {
            if brief.contains("THE LEDGER IS EMPTY") && !planned {
                let mut seen = Vec::new();
                for h in [PLAIN, VERIFY, CAPTCHA, TWOFA] {
                    if brief.contains(h) && !seen.contains(&h) {
                        seen.push(h);
                    }
                }
                let items: Vec<Value> = seen
                    .iter()
                    .map(|h| {
                        json!({"key": h, "title": format!("Sign up on {h}"), "kind": "site",
                               "facts": {"url": format!("https://{h}")}})
                    })
                    .collect();
                return call("mission_plan", &json!({"items": items}));
            }
            let mut open = open_keys(brief);
            if open.is_empty() {
                open = plan_keys;
            }
            if let Some(next) = open.into_iter().find(|k| !handled.contains(k)) {
                return call("mission_item", &json!({"key": next, "status": "doing"}));
            }
            if brief.contains("NO OPEN ITEM IS LEFT")
                && !brief.contains("WAITING FOR A WAKE")
                && !completed
            {
                return call(
                    "mission_complete",
                    &json!({"summary": "Signed up on the sites that allow it; one was given up (CAPTCHA)."}),
                );
            }
            return say("Done for now: everything I could reach is in the ledger.");
        };

        let site = format!("https://{key}");
        let woken = brief
            .lines()
            .any(|l| l.contains(&format!("- \"{key}\"")) && l.contains("a mail that matches"));
        let since = &calls[at + 1..];
        if woken {
            // The wake brief says how: open the site, then ask the inbox once for that site.
            return match since.len() {
                0 => call("browser_open", &json!({"url": site})),
                1 => call(
                    "email_wait_for",
                    &json!({"site": site, "from_domain": key, "subject_contains": "Confirm", "max_age_minutes": 60, "block_seconds": 5}),
                ),
                2 => {
                    let link = find(
                        r"https://[^\s\x22\\<>]+/confirm\?token=[\w-]+",
                        &since[1].result,
                    )
                    .map(|c| c[0].to_owned())
                    .unwrap_or_else(|| panic!("no followable link in: {}", since[1].result));
                    call("browser_open", &json!({"url": link}))
                }
                _ => call(
                    "mission_item",
                    &json!({"key": key, "status": "done", "result": evidence(&since[2].result)}),
                ),
            };
        }
        match since.len() {
            0 => call("account_register", &json!({"site": site})),
            1 => call("browser_open", &json!({"url": format!("{site}/signup")})),
            2 => {
                let login = find(r"[\w.+-]+@[\w.-]+", &since[0].result).map_or_else(
                    || panic!("no login in: {}", since[0].result),
                    |c| c[0].to_owned(),
                );
                call(
                    "browser_fill",
                    &json!({"ref": ref_of(&since[1].result, "Email"), "text": login}),
                )
            }
            3 => {
                let secret = find(r"login/[\w.-]+", &since[0].result)
                    .map(|c| c[0].to_owned())
                    .unwrap_or_else(|| panic!("no secret name in: {}", since[0].result));
                call(
                    "browser_fill",
                    &json!({"ref": ref_of(&since[1].result, "Password"), "secret": secret}),
                )
            }
            4 => {
                let page = &since[1].result;
                if page.contains("not a robot") || page.contains("verify you are human") {
                    // A CAPTCHA: the form could be filled, but nobody may solve the check.
                    call("browser_handover", &json!({"kind": "captcha"}))
                } else {
                    call(
                        "browser_click",
                        &json!({"ref": ref_of(page, "Create account")}),
                    )
                }
            }
            5 => {
                let page = &since[4].result;
                if page.contains("Welcome aboard") {
                    call(
                        "mission_item",
                        &json!({"key": key, "status": "done", "result": evidence(page)}),
                    )
                } else if page.contains("Check your email") {
                    call(
                        "mission_wait",
                        &json!({
                            "item": key,
                            "reason": "waiting for the confirmation mail",
                            "mail": {"from_domain": key, "subject_contains": "Confirm", "site": site},
                            "timeout_minutes": 30
                        }),
                    )
                } else if page.contains("Set up two-factor") {
                    let name = find(r"login/[\w.-]+", &since[0].result).unwrap()[0].to_owned();
                    call(
                        "account_totp_save",
                        &json!({"name": name, "ref": ref_of_role(page, "code")}),
                    )
                } else {
                    call(
                        "mission_item",
                        &json!({"key": key, "status": "failed", "error": format!("unexpected page: {}", evidence(page))}),
                    )
                }
            }
            6 => {
                let name = find(r"login/[\w.-]+", &since[0].result).unwrap()[0].to_owned();
                call(
                    "browser_fill",
                    &json!({"ref": ref_of(&since[4].result, "Authentication code"),
                            "secret": format!("totp:{name}")}),
                )
            }
            7 => call(
                "browser_click",
                &json!({"ref": ref_of(&since[4].result, "Verify")}),
            ),
            _ => call(
                "mission_item",
                &json!({"key": key, "status": "done",
                        "result": format!("{} Two-factor key kept in the vault.", evidence(&since[7].result))}),
            ),
        }
    }
}

#[async_trait]
impl ChatProvider for SignupModel {
    fn id(&self) -> &'static str {
        "fake"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ProviderError>>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().push(req.clone());
        let mut chunks =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.decide(&req))) {
                Ok(chunks) => chunks,
                Err(e) => {
                    let why = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    self.failure.lock().get_or_insert(why);
                    say("The script cannot go on.")
                }
            };
        let finish = if chunks.iter().any(|c| matches!(c, ChatChunk::ToolCall(_))) {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };
        chunks.push(ChatChunk::Usage(Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            cached_tokens: None,
            cache_write_tokens: None,
        }));
        chunks.push(ChatChunk::Done {
            finish_reason: finish,
        });
        Ok(futures::stream::iter(chunks.into_iter().map(Ok)).boxed())
    }
}
