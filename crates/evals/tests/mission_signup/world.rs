//! The daemon under test and its fake outside world.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::{AgentLoop, BotProfile, LoopConfig, Providers, SharedRedactor};
use builtin_tools::browser::BrowserDriver;
use builtin_tools::connector_email::{
    AccountConfig, AccountKind, AgentMailEndpoint, EmailConnector, IdentitySettings, ImapSettings,
    SmtpSettings,
};
use daemon_api::{AppState, DaemonConfig};
use missions::supervisor::{SupervisorConfig, SystemClock};
use missions::{Item, MissionStore};
use parking_lot::Mutex;
use protocol::{BotId, ChatId, InitiatorKind, RunId, UserId};
use secret_store::{FillSecrets, MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use storage::{Bot, Chat, Storage, User, now_ms};
use task_runtime::RetryPolicy;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::browser::SiteBrowser;
use crate::mailbox::FakeAgentMail;
use crate::model::SignupModel;
use crate::sites::Sites;

const TOKEN: &str = "eval-token-0123456789abcdef0123456789abcdef0123456789abcdef";
const OWNER_KEY: &str = "ownerkey0123456789abcdef0123456789abcdef0123456789abcdef";
const INBOX: &str = "ada@agentmail.to";
pub const MAIL_API_KEY: &str = "am_eval_KEY_5c1d9e7a3b2f4086";

const PROFILE: &str = "[bot]\nid = \"ada\"\nname = \"Ada\"\npersona = \"You are Ada, a careful assistant.\"\n\
    [models]\nexecutor = \"fake/m\"\n[permissions]\npreset = \"full\"\n";

pub struct World {
    pub dir: tempfile::TempDir,
    pub storage: Arc<Storage>,
    pub model: Arc<SignupModel>,
    pub sites: Sites,
    pub mail: Arc<FakeAgentMail>,
    pub browser: Arc<SiteBrowser>,
    pub fill: Arc<FillSecrets>,
    pub events: Arc<Mutex<Vec<Value>>>,
    pub user: UserId,
    pub bot: BotId,
    pub chat: ChatId,
    http: reqwest::Client,
    base: String,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<std::io::Result<Vec<RunId>>>>,
    collector: JoinHandle<()>,
}

impl World {
    #[allow(clippy::too_many_lines)] // one ordered assembly, like the daemon's own
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(dir.path()).unwrap());
        let (user, bot, chat) = (UserId::new(), BotId::new(), ChatId::new());
        storage
            .insert_user(&User {
                id: user,
                name: "owner".into(),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .insert_bot(&Bot {
                id: bot,
                user_id: user,
                name: "Ada".into(),
                // The scripted model answers every request: no page reader call.
                config: json!({"browser": {"reader": "off"}}),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .insert_chat(&Chat {
                id: chat,
                user_id: user,
                bot_id: bot,
                title: "Ada".into(),
                created_at_ms: now_ms(),
            })
            .unwrap();
        storage
            .set_bot_spend_limits(user, bot, "{}", &json!({}))
            .unwrap();

        // The outside world.
        let mail = Arc::new(FakeAgentMail::start(INBOX).await);
        let sites = Sites::start(Arc::clone(&mail)).await;
        let browser = Arc::new(SiteBrowser::new(sites.ports.clone()));
        let model = SignupModel::new();

        // The vault, its redactor, and the bot's own mailbox.
        let store = Arc::new(MemoryStore::new());
        let redactor = SharedRedactor::default();
        redactor.register(MAIL_API_KEY);
        let hook = redactor.clone();
        let fill = Arc::new(
            FillSecrets::in_memory(Arc::clone(&store) as Arc<dyn SecretStore>)
                .with_value_hook(Arc::new(move |v: &str| hook.register(v))),
        );
        let account = AccountConfig {
            name: "own".into(),
            address: INBOX.into(),
            username: String::new(),
            password: SecretRef::new("email/own/api_key").unwrap(),
            imap: ImapSettings::default(),
            smtp: SmtpSettings::default(),
            drafts_folder: "Drafts".into(),
            extra_root_pem: None,
            timeout_secs: 30,
            bot: Some(bot.to_string()),
            allowed_senders: Vec::new(),
            identity: IdentitySettings::default(),
            kind: AccountKind::AgentMail,
            inbox_id: Some(INBOX.into()),
        };
        store.set(account.password.name(), MAIL_API_KEY).unwrap();
        let connector = Arc::new(EmailConnector::reloadable_with(
            Arc::clone(&store) as Arc<dyn SecretStore>,
            AgentMailEndpoint::loopback_mock_for_tests(&mail.uri()).unwrap(),
        ));
        connector.replace_accounts(vec![account]).unwrap();

        // The agent loop with the built-in tools, as `bot serve` assembles it.
        let gate = builtin_tools::ResolverGate::new(
            permission_engine::PermissionResolver::new(),
            permission_engine::Policy::approve_destructive(),
            None,
            InitiatorKind::Human,
        );
        let mut tools = builtin_tools::BuiltinConfig::new(dir.path().to_path_buf(), Arc::new(gate));
        tools.browser = Some(Arc::clone(&browser) as Arc<dyn BrowserDriver>);
        tools.email = Some(Arc::clone(&connector));
        tools.fill_secrets = Some(Arc::clone(&fill));
        let agent = AgentLoop::new(
            Arc::clone(&storage),
            tool_registry::ToolRegistry::new(),
            Providers::single(Arc::clone(&model) as Arc<dyn provider_catalog::ChatProvider>),
            LoopConfig {
                redactor: Some(redactor),
                // Every tool is offered directly: tool discovery has its own tests, and a
                // scripted model has no use for a search step.
                legacy_tool_surface: true,
                discovery_threshold: 1_000,
                model_retry: RetryPolicy {
                    max_attempts: 1,
                    initial_backoff_ms: 1,
                    multiplier: 1,
                    max_backoff_ms: 1,
                },
                ..LoopConfig::default()
            },
        )
        .with_builtin_tools(tools);
        agent.add_profile(bot, BotProfile::from_toml_str(PROFILE).unwrap());

        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let collector = {
            let mut rx = agent.subscribe();
            let sink = Arc::clone(&events);
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(item) => sink
                            .lock()
                            .push(serde_json::to_value(&item.envelope).unwrap_or(Value::Null)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
            })
        };

        // The daemon: the real state, routes, supervisor, wake loop and owner notices.
        let state = AppState::new(
            agent.clone(),
            Arc::clone(&storage),
            DaemonConfig::new(TOKEN),
        )
        .unwrap();
        state.set_owner_key(OWNER_KEY.into());
        state.set_fill_secrets(Arc::clone(&fill));
        state.set_mission_timing(
            Arc::new(SystemClock),
            SupervisorConfig {
                session_gap: Duration::ZERO,
                poll: Duration::from_millis(10),
                wake_poll: Duration::from_millis(10),
            },
        );
        state.enable_missions().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(daemon_api::serve(listener, state, async move {
            let _ = rx.await;
        }));
        let w = Self {
            dir,
            storage,
            model,
            sites,
            mail,
            browser,
            fill,
            events,
            user,
            bot,
            chat,
            http: reqwest::Client::new(),
            base: format!("http://127.0.0.1:{port}"),
            stop: Some(stop),
            server: Some(server),
            collector,
        };
        let t0 = Instant::now();
        loop {
            let (_, health) = w.api("GET", "/v1/health", None).await;
            if health["startup"] == "ready" {
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "the daemon never got ready: {health}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        w
    }

    /// The notices the daemon PUSHES to Telegram and Discord, as their bridges read them: the
    /// replay of `GET /v1/owner-notices/stream` from the start, read until it goes quiet.
    pub async fn pushed_notices(&self) -> Vec<channel_api::notice::OwnerNotice> {
        let mut resp = self
            .http
            .get(format!("{}/v1/owner-notices/stream?after=0", self.base))
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("x-praxi-owner-key", OWNER_KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let mut text = String::new();
        while let Ok(Ok(Some(chunk))) =
            tokio::time::timeout(Duration::from_millis(400), resp.chunk()).await
        {
            text.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(
            text.contains("event: hello"),
            "the stream greets first: {text}"
        );
        text.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect()
    }

    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            let ended = tokio::time::timeout(Duration::from_secs(20), server).await;
            assert!(ended.is_ok(), "the daemon shut down in time");
        }
        self.collector.abort();
    }

    // ---------------------------------------------------------------- HTTP

    /// One owner-key request.
    pub async fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("x-praxi-owner-key", OWNER_KEY)
            .header("x-praxi-message-source", "local_ui");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    // ---------------------------------------------------------------- the story

    /// The owner types in their own chat (the app's request: owner key and local UI source).
    pub async fn owner_says(&self, text: &str) -> Value {
        let (status, body) = self
            .api(
                "POST",
                &format!("/v1/chats/{}/messages", self.chat.0),
                Some(json!({"text": text, "mode": "steer"})),
            )
            .await;
        assert_eq!(status, 202, "{body}");
        body
    }

    /// The one mission the chat request started.
    pub async fn wait_for_mission(&self) -> Value {
        let t0 = Instant::now();
        loop {
            let (_, list) = self.api("GET", "/v1/missions", None).await;
            if let Some(m) = list.as_array().and_then(|l| l.first()) {
                return m.clone();
            }
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "the owner's request never started a mission"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The mission (`GET /v1/missions/{id}`: mission, items, digest) once it is `done`, `failed`,
    /// `blocked` or `cancelled`.
    pub async fn wait_mission_end(&self, id: &str) -> Value {
        let t0 = Instant::now();
        loop {
            let (_, m) = self.api("GET", &format!("/v1/missions/{id}"), None).await;
            if matches!(
                m["mission"]["state"].as_str(),
                Some("done" | "failed" | "blocked" | "cancelled")
            ) {
                return m;
            }
            assert!(
                self.model.failure().is_none(),
                "the scripted model could not read a result: {:?}",
                self.model.failure()
            );
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "the mission never ended: {}\n{}",
                m["mission"]["state"],
                self.diagnose()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// What happened so far, for a failure message: the tool calls, every card and the audit rows.
    pub fn diagnose(&self) -> String {
        let mut out = String::new();
        for e in self.events.lock().iter() {
            let ev = &e["event"];
            match ev["type"].as_str() {
                Some("tool_call_requested") => {
                    writeln!(
                        out,
                        "  call {} {}",
                        ev["tool_name"],
                        ev["input"]
                            .to_string()
                            .chars()
                            .take(160)
                            .collect::<String>()
                    )
                    .unwrap();
                }
                Some("tool_call_completed") => {
                    writeln!(
                        out,
                        "    -> {}",
                        ev.to_string().chars().take(300).collect::<String>()
                    )
                    .unwrap();
                }
                Some(
                    t @ ("approval_needed" | "question_asked" | "secret_requested"
                    | "handover_requested" | "error"),
                ) => {
                    writeln!(out, "  !! {t}: {ev}").unwrap();
                }
                _ => {}
            }
        }
        for a in self.storage.list_audit(self.user).unwrap_or_default() {
            writeln!(
                out,
                "  audit {} {} {}",
                a.actor,
                a.action,
                a.detail.to_string().chars().take(200).collect::<String>()
            )
            .unwrap();
        }
        out
    }

    /// The ledger, read from the mission database the daemon wrote.
    pub fn items(&self, id: &str) -> Vec<Item> {
        MissionStore::open(self.dir.path().join(missions::DB_FILE))
            .unwrap()
            .items(id, None)
            .unwrap()
    }
}
