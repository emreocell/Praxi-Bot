//! The client-side handler of every MCP session.
//!
//! The bridge advertises one client capability: form elicitation. Other
//! server-initiated requests keep rmcp's defaults: sampling is `method not
//! found` and roots are empty. The handler also reacts to
//! `notifications/tools/list_changed`: the owning server is told to list its
//! tools again, so a tool a server adds, removes or redefines at runtime never
//! stays stale in the registry.
//!
//! **Elicitation.** A server may ask the owner for input only while one of its
//! tool calls runs: the request belongs to that call's run. With no call or
//! several calls in flight on the server it is declined (the bridge never
//! guesses the run). A form the bridge cannot show (URL mode, a multi-select
//! list, an unknown type) or must not show (over the size limits in
//! `form.rs`, or more than [`MAX_REQUESTS_PER_WINDOW`] requests from one server
//! in a minute) is declined too, and the host is told why (`Elicitor::refused`)
//! so it can audit it. The answer goes back to the server only; when the call
//! ends first, the server gets `cancel`.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use rmcp::{
    ClientHandler, ErrorData, RoleClient,
    model::{
        ClientConfig, ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationCapability,
        FormElicitationCapability,
    },
    service::{NotificationContext, RequestContext, RunningService},
};
use serde_json::Value;
use tokio::time::Instant;
use tool_registry::{ElicitationAnswer, ElicitationRequest, Elicitor};

use crate::form;

/// Where the host installs its [`Elicitor`]; shared by every server of a manager.
pub(crate) type ElicitorSlot = RwLock<Option<Arc<dyn Elicitor>>>;

/// Form requests one server may make in [`RATE_WINDOW`], whether or not a tool
/// call is in flight. The rest are declined.
pub(crate) const MAX_REQUESTS_PER_WINDOW: usize = 4;
pub(crate) const RATE_WINDOW: Duration = Duration::from_secs(60);

/// A server's request budget, shared by every session of the server.
#[derive(Default)]
struct Rate {
    recent: Mutex<VecDeque<Instant>>,
    /// The refusal of this window was already reported to the host.
    reported: Mutex<bool>,
}

/// What [`Rate::check`] decided.
#[derive(Debug, PartialEq, Eq)]
enum Budget {
    Allowed,
    /// Over budget; `report` is true for the first refusal of a window only
    /// (a flood of requests must not become a flood of audit rows).
    Limited {
        report: bool,
    },
}

impl Rate {
    fn check(&self) -> Budget {
        let now = Instant::now();
        let mut recent = self.recent.lock();
        while recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() < MAX_REQUESTS_PER_WINDOW {
            recent.push_back(now);
            *self.reported.lock() = false;
            return Budget::Allowed;
        }
        let mut reported = self.reported.lock();
        let report = !*reported;
        *reported = true;
        Budget::Limited { report }
    }
}

/// One tool call in flight on a server.
struct InFlight {
    id: u64,
    run_id: String,
    /// Closed when the call ends.
    done: tokio::sync::watch::Receiver<()>,
    /// A note for the model the host left when it parked the work item.
    note: Arc<Mutex<Option<String>>>,
}

/// Signals a session raises for the server that owns it. They outlive a
/// single session: a reconnect keeps the same instance.
#[derive(Default)]
pub(crate) struct Signals {
    /// The server announced that its tool list changed.
    pub(crate) tools_changed: tokio::sync::Notify,
    /// The server's configured id, shown on the form.
    server: String,
    elicitor: Arc<ElicitorSlot>,
    calls: Mutex<Vec<InFlight>>,
    next_call: AtomicU64,
    /// Forms open right now: the call waits for the owner, not for the server.
    eliciting: AtomicUsize,
    rate: Rate,
}

/// Marks a tool call in flight until dropped.
pub(crate) struct CallGuard {
    signals: Arc<Signals>,
    id: u64,
    note: Arc<Mutex<Option<String>>>,
    _done: tokio::sync::watch::Sender<()>,
}

impl CallGuard {
    /// What the host wants the model to read next to this call's result (the
    /// work item was parked), once.
    pub(crate) fn take_note(&self) -> Option<String> {
        self.note.lock().take()
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.signals.calls.lock().retain(|c| c.id != self.id);
    }
}

struct Eliciting<'a>(&'a AtomicUsize);

impl Drop for Eliciting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Signals {
    pub(crate) fn new(server: String, elicitor: Arc<ElicitorSlot>) -> Self {
        Self {
            server,
            elicitor,
            ..Self::default()
        }
    }

    /// Record a tool call of run `run_id` until the guard is dropped.
    pub(crate) fn begin_call(self: &Arc<Self>, run_id: &str) -> CallGuard {
        let id = self.next_call.fetch_add(1, Ordering::Relaxed);
        let (tx, done) = tokio::sync::watch::channel(());
        let note = Arc::new(Mutex::new(None));
        self.calls.lock().push(InFlight {
            id,
            run_id: run_id.to_owned(),
            done,
            note: Arc::clone(&note),
        });
        CallGuard {
            signals: Arc::clone(self),
            id,
            note,
            _done: tx,
        }
    }

    /// A form of this server is waiting for the owner.
    pub(crate) fn is_eliciting(&self) -> bool {
        self.eliciting.load(Ordering::Acquire) > 0
    }

    async fn elicit(&self, request: ElicitRequestParams) -> ElicitResult {
        let decline = || ElicitResult::new(ElicitationAction::Decline);
        // Every request counts against the budget, answered or not.
        let budget = self.rate.check();
        let Some(elicitor) = self.elicitor.read().clone() else {
            return decline();
        };
        let (run_id, mut done, note) = match self.calls.lock().as_slice() {
            [call] => (
                call.run_id.clone(),
                call.done.clone(),
                Arc::clone(&call.note),
            ),
            _ => return decline(),
        };
        match budget {
            Budget::Allowed => {}
            Budget::Limited { report } => {
                if report {
                    elicitor.refused(
                        &run_id,
                        &self.server,
                        "too many form requests from this server (limit: 4 per minute)",
                    );
                }
                return decline();
            }
        }
        let form = match form::project(request) {
            Ok(form) => form,
            Err(reason) => {
                elicitor.refused(&run_id, &self.server, reason);
                return decline();
            }
        };
        self.eliciting.fetch_add(1, Ordering::AcqRel);
        let _open = Eliciting(&self.eliciting);
        let request = ElicitationRequest {
            server: self.server.clone(),
            message: form.message,
            fields: form.fields,
        };
        let answer = tokio::select! {
            answer = elicitor.elicit(&run_id, request) => answer,
            // The call ended (timeout, Stop): nobody waits for the answer.
            _ = done.changed() => ElicitationAnswer::Cancel,
        };
        match answer {
            ElicitationAnswer::Accept(content) => {
                ElicitResult::new(ElicitationAction::Accept).with_content(Value::Object(content))
            }
            ElicitationAnswer::Decline => decline(),
            ElicitationAnswer::Cancel => ElicitResult::new(ElicitationAction::Cancel),
            ElicitationAnswer::Parked(text) => {
                *note.lock() = Some(text);
                ElicitResult::new(ElicitationAction::Cancel)
            }
        }
    }
}

/// Handler installed on every session.
#[derive(Clone, Default)]
pub(crate) struct BridgeClient {
    pub(crate) signals: Arc<Signals>,
}

/// A running client session with the bridge's handler.
pub(crate) type Running = RunningService<RoleClient, BridgeClient>;

impl ClientHandler for BridgeClient {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        // `notify_one` keeps one permit, so a notification that arrives while
        // the watcher is busy listing is not lost and a burst collapses.
        self.signals.tools_changed.notify_one();
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        Ok(self.signals.elicit(request).await)
    }

    fn get_info(&self) -> ClientConfig {
        let mut info = ClientConfig::default();
        info.capabilities.elicitation =
            Some(ElicitationCapability::new().with_form(FormElicitationCapability::new()));
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ElicitationSchema;

    struct Echo;

    #[async_trait::async_trait]
    impl Elicitor for Echo {
        async fn elicit(&self, run_id: &str, request: ElicitationRequest) -> ElicitationAnswer {
            let mut content = serde_json::Map::new();
            content.insert("run".into(), Value::String(run_id.to_owned()));
            content.insert("server".into(), Value::String(request.server));
            ElicitationAnswer::Accept(content)
        }
    }

    struct Never;

    #[async_trait::async_trait]
    impl Elicitor for Never {
        async fn elicit(&self, _: &str, _: ElicitationRequest) -> ElicitationAnswer {
            std::future::pending().await
        }
    }

    /// Answers with a fixed answer and remembers every refusal it was told of.
    #[derive(Default)]
    struct Recorder {
        answer: Mutex<Option<ElicitationAnswer>>,
        refusals: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Elicitor for Recorder {
        async fn elicit(&self, _: &str, _: ElicitationRequest) -> ElicitationAnswer {
            self.answer
                .lock()
                .clone()
                .unwrap_or(ElicitationAnswer::Decline)
        }

        fn refused(&self, run_id: &str, server: &str, reason: &str) {
            self.refusals
                .lock()
                .push(format!("{run_id}|{server}|{reason}"));
        }
    }

    fn signals(elicitor: Arc<dyn Elicitor>) -> Arc<Signals> {
        Arc::new(Signals::new(
            "srv".into(),
            Arc::new(RwLock::new(Some(elicitor))),
        ))
    }

    fn form() -> ElicitRequestParams {
        ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "Which account?".into(),
            requested_schema: ElicitationSchema::builder()
                .required_email("email")
                .build()
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn exactly_one_call_in_flight_gets_the_form_for_its_run() {
        let s = signals(Arc::new(Echo));
        let _call = s.begin_call("run-1");
        let r = s.elicit(form()).await;
        assert_eq!(r.action, ElicitationAction::Accept);
        let content = r.content.unwrap();
        assert_eq!(content["run"], "run-1");
        assert_eq!(content["server"], "srv");
    }

    #[tokio::test]
    async fn no_call_in_flight_declines() {
        let s = signals(Arc::new(Echo));
        drop(s.begin_call("run-1"));
        assert_eq!(s.elicit(form()).await.action, ElicitationAction::Decline);
    }

    #[tokio::test]
    async fn several_calls_in_flight_decline() {
        let s = signals(Arc::new(Echo));
        let _a = s.begin_call("run-1");
        let _b = s.begin_call("run-2");
        assert_eq!(s.elicit(form()).await.action, ElicitationAction::Decline);
    }

    #[tokio::test]
    async fn no_elicitor_declines() {
        let s = Arc::new(Signals::default());
        let _call = s.begin_call("run-1");
        assert_eq!(s.elicit(form()).await.action, ElicitationAction::Decline);
    }

    #[tokio::test]
    async fn url_mode_declines_and_is_reported() {
        let rec = Arc::new(Recorder::default());
        let s = signals(rec.clone());
        let _call = s.begin_call("run-1");
        let url = ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: "open".into(),
            url: "https://example.com/".into(),
            elicitation_id: "x".into(),
        };
        assert_eq!(s.elicit(url).await.action, ElicitationAction::Decline);
        assert_eq!(rec.refusals.lock().len(), 1);
    }

    #[tokio::test]
    async fn the_call_ending_cancels_an_open_form() {
        let s = signals(Arc::new(Never));
        let call = s.begin_call("run-1");
        let waiter = {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.elicit(form()).await })
        };
        while !s.is_eliciting() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(call);
        let r = waiter.await.unwrap();
        assert_eq!(r.action, ElicitationAction::Cancel);
        assert!(!s.is_eliciting());
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_over_its_budget_is_declined_reported_once_and_recovers() {
        let rec = Arc::new(Recorder::default());
        let s = signals(rec.clone());
        let _call = s.begin_call("run-1");
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert_eq!(s.elicit(form()).await.action, ElicitationAction::Decline);
        }
        assert!(
            rec.refusals.lock().is_empty(),
            "within budget: nothing refused"
        );
        for _ in 0..10 {
            assert_eq!(s.elicit(form()).await.action, ElicitationAction::Decline);
        }
        assert_eq!(
            rec.refusals.lock().len(),
            1,
            "a flood is reported once per window, not once per request"
        );
        assert!(rec.refusals.lock()[0].starts_with("run-1|srv|too many"));
        tokio::time::advance(RATE_WINDOW).await;
        *rec.answer.lock() = Some(ElicitationAnswer::Cancel);
        assert_eq!(
            s.elicit(form()).await.action,
            ElicitationAction::Cancel,
            "a new window is a new budget"
        );
    }

    #[tokio::test]
    async fn a_request_over_the_size_limits_never_reaches_the_host_and_is_reported() {
        let rec = Arc::new(Recorder::default());
        *rec.answer.lock() = Some(ElicitationAnswer::Cancel);
        let s = signals(rec.clone());
        let _call = s.begin_call("run-1");
        let huge = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "m".repeat(form::MAX_MESSAGE_BYTES + 1),
            requested_schema: ElicitationSchema::builder()
                .required_email("email")
                .build()
                .unwrap(),
        };
        assert_eq!(s.elicit(huge).await.action, ElicitationAction::Decline);
        assert_eq!(rec.refusals.lock().len(), 1);
        assert!(!s.is_eliciting());
    }

    #[tokio::test]
    async fn a_parked_answer_cancels_for_the_server_and_leaves_a_note_for_the_model() {
        let rec = Arc::new(Recorder::default());
        *rec.answer.lock() = Some(ElicitationAnswer::Parked("item parked".into()));
        let s = signals(rec);
        let call = s.begin_call("run-1");
        assert_eq!(s.elicit(form()).await.action, ElicitationAction::Cancel);
        assert_eq!(call.take_note().as_deref(), Some("item parked"));
        assert_eq!(call.take_note(), None, "the note is read once");
    }
}
