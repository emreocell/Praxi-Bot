//! A bot that lives on THIS computer but uses a paired server as ITS computer
//! (`[runtime] computer = "server:<key>"`): the model is still called here, the
//! browser tools go to the server over the paired link, approvals stay here, and
//! nothing else changes places.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use builtin_tools::browser::BrowserDriver;
use builtin_tools::pc::{PcCaller, PcError, PcLink, RPC_BROWSER, ServerLinks};
use parking_lot::Mutex;
use permission_engine::Policy;
use protocol::Event;
use serde_json::{Value, json};
use task_runtime::RunState;
use tool_registry::Tool;

use crate::ApprovalScope;
use crate::tests::{
    Fake, MACHINE_A, MACHINE_B, calls, cfg, make_loop, profile, seed_desktop_grant, setup, text,
    tool_messages, until,
};

/// The server's end of the link: records what it was asked, or refuses.
#[derive(Default)]
struct ServerLink {
    calls: Mutex<Vec<(String, Value)>>,
    refuse: Option<&'static str>,
}

#[async_trait]
impl PcLink for ServerLink {
    async fn call(&self, method: &str, params: Value, _: Duration) -> Result<Value, PcError> {
        self.calls.lock().push((method.to_owned(), params.clone()));
        if let Some(why) = self.refuse {
            return Err(PcError::Message(why.into()));
        }
        Ok(match params["op"].as_str() {
            Some("current_url") => json!({"url": null}),
            Some("open") => json!({"text": "opened on the server"}),
            _ => json!({"text": "ok"}),
        })
    }
    fn desktop(&self, _: &PcCaller) -> Result<Arc<dyn desktop_api::DesktopBackend>, PcError> {
        Err(PcError::Message("no desktop".into()))
    }
}

/// Knows the servers it was given, by key.
struct Links(Vec<(&'static str, Arc<ServerLink>)>);

impl ServerLinks for Links {
    fn link(&self, id: &str) -> Option<(String, Arc<dyn PcLink>)> {
        self.0
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, l)| ("Oracle server".to_owned(), Arc::clone(l) as Arc<dyn PcLink>))
    }
}

/// This computer's own browser: must never be used by such a bot.
#[derive(Default)]
struct LocalBrowser {
    used: AtomicUsize,
}

#[async_trait]
impl BrowserDriver for LocalBrowser {
    async fn open(&self, _: &str, _: Option<&str>) -> Result<String, String> {
        self.used.fetch_add(1, Ordering::SeqCst);
        Ok("opened HERE".into())
    }
    async fn snapshot(&self, _: bool) -> Result<String, String> {
        self.used.fetch_add(1, Ordering::SeqCst);
        Ok("here".into())
    }
    async fn click(&self, _: &str) -> Result<String, String> {
        self.used.fetch_add(1, Ordering::SeqCst);
        Ok("c".into())
    }
    async fn fill(&self, _: &str, _: &str) -> Result<String, String> {
        self.used.fetch_add(1, Ordering::SeqCst);
        Ok("f".into())
    }
    async fn select(&self, _: &str, _: &str) -> Result<String, String> {
        Ok("s".into())
    }
    async fn press(&self, _: &str) -> Result<String, String> {
        Ok("p".into())
    }
    async fn scroll(&self, _: Option<&str>, _: f64, _: f64) -> Result<String, String> {
        Ok("s".into())
    }
    async fn wait(&self, _: builtin_tools::Wait, _: Option<Duration>) -> Result<String, String> {
        Ok("w".into())
    }
    async fn back(&self) -> Result<String, String> {
        Ok("b".into())
    }
    async fn tabs(&self, _: Option<usize>) -> Result<String, String> {
        Ok("t".into())
    }
    async fn screenshot(
        &self,
        _: bool,
        _: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Ok((vec![1], "png"))
    }
    async fn eval(&self, _: &str) -> Result<Value, String> {
        self.used.fetch_add(1, Ordering::SeqCst);
        Ok(json!("ran HERE"))
    }
}

/// A loop for a bot whose computer is `server:<key>`, with the real browser
/// tools registered over a local browser that must stay untouched.
fn server_loop(
    env: &crate::tests::Env,
    fake: &Arc<Fake>,
    key: &str,
    links: Links,
    policy: Policy,
) -> (crate::AgentLoop, Arc<LocalBrowser>) {
    let lp = make_loop(
        &env.storage,
        env.bot,
        fake,
        &[],
        profile(&format!("[runtime]\ncomputer = \"server:{key}\"\n")),
        cfg(policy),
    );
    lp.set_server_links(Arc::new(links));
    let local = Arc::new(LocalBrowser::default());
    let control: Arc<dyn builtin_tools::DesktopControlGate> =
        Arc::new(crate::control::LoopControl {
            sh: Arc::downgrade(&lp.sh),
        });
    let dir = tempfile::tempdir().unwrap();
    for tool in builtin_tools::browser::tools_with_secrets(
        Arc::clone(&local) as Arc<dyn BrowserDriver>,
        dir.path(),
        Some(control),
        None,
    ) {
        assert!(lp.sh.tools.register(tool as Arc<dyn Tool>));
    }
    (lp, local)
}

fn open_call() -> crate::tests::Reply {
    calls(&[("c1", "browser_open", json!({"url": "https://example.com/"}))])
}

/// `browser_open` of a bot whose computer is the server goes to the server; this
/// computer's browser is not touched; the model is called here; the page text
/// comes back through the normal result path.
#[tokio::test]
async fn a_local_bot_with_the_server_as_its_computer_browses_on_the_server() {
    let env = setup();
    let fake = Fake::new(vec![open_call(), text("done")]);
    let server = Arc::new(ServerLink::default());
    let (lp, local) = server_loop(
        &env,
        &fake,
        MACHINE_B,
        Links(vec![(MACHINE_B, Arc::clone(&server))]),
        Policy::full(),
    );
    let run = lp.run_turn(env.chat, "open example.com").unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let second = tool_messages(&fake.request(1)).join(" ");
    assert!(second.contains("opened on the server"), "{second}");
    let seen = server.calls.lock().clone();
    assert!(
        seen.iter().any(|(m, p)| m == RPC_BROWSER
            && p["op"] == "open"
            && p["url"] == "https://example.com/"
            && p["bot"]["id"].is_string()),
        "{seen:?}"
    );
    assert_eq!(local.used.load(Ordering::SeqCst), 0);
}

/// An unpaired / unknown server leaves the run with no computer and a clear
/// reason: it fails closed, never browses on this computer, never calls a server.
#[tokio::test]
async fn an_unpaired_server_fails_closed_and_never_uses_this_computers_browser() {
    let env = setup();
    let fake = Fake::new(vec![open_call(), text("done")]);
    let other = Arc::new(ServerLink::default());
    // The bot names MACHINE_B; only some other server is paired.
    let (lp, local) = server_loop(
        &env,
        &fake,
        MACHINE_B,
        Links(vec![(MACHINE_A, Arc::clone(&other))]),
        Policy::full(),
    );
    let run = lp.run_turn(env.chat, "open example.com").unwrap();
    let state = lp.wait(run).await.unwrap();
    assert_ne!(
        state,
        RunState::Succeeded,
        "no computer: the run must not go on"
    );
    assert_eq!(local.used.load(Ordering::SeqCst), 0);
    assert!(
        other.calls.lock().is_empty(),
        "another server was not used instead"
    );
}

/// The server's own refusal comes back as the tool's error: no retry on this
/// computer's browser, whatever the permission level here (Full included).
#[tokio::test]
async fn a_refusal_by_the_server_is_final_even_at_full_level() {
    let env = setup();
    let fake = Fake::new(vec![open_call(), text("done")]);
    let server = Arc::new(ServerLink {
        refuse: Some("only the paired administrator desktop may use this server as a computer"),
        ..ServerLink::default()
    });
    let (lp, local) = server_loop(
        &env,
        &fake,
        MACHINE_B,
        Links(vec![(MACHINE_B, Arc::clone(&server))]),
        Policy::full(),
    );
    let run = lp.run_turn(env.chat, "open example.com").unwrap();
    lp.wait(run).await.unwrap();
    let second = tool_messages(&fake.request(1)).join(" ");
    assert!(second.contains("administrator desktop"), "{second}");
    assert!(!second.contains("opened"), "{second}");
    assert_eq!(server.calls.lock().len(), 1, "no second try");
    assert_eq!(local.used.load(Ordering::SeqCst), 0);
}

/// The model cannot run page scripts on the server, and the attempt does not
/// reach this computer's browser either.
#[tokio::test]
async fn page_scripts_are_not_served_on_the_server() {
    let env = setup();
    let fake = Fake::new(vec![
        calls(&[("c1", "browser_eval", json!({"js": "document.cookie"}))]),
        text("done"),
    ]);
    let server = Arc::new(ServerLink::default());
    let (lp, local) = server_loop(
        &env,
        &fake,
        MACHINE_B,
        Links(vec![(MACHINE_B, Arc::clone(&server))]),
        Policy::full(),
    );
    let run = lp.run_turn(env.chat, "read cookies").unwrap();
    lp.wait(run).await.unwrap();
    assert_eq!(local.used.load(Ordering::SeqCst), 0);
    assert!(
        server.calls.lock().iter().all(|(_, p)| p["op"] != "eval"),
        "no eval op crosses the link"
    );
}

/// "Always allow browser actions" given while the bot's computer is server B
/// covers server B only: not server C, and (by the scope's own prefix) not a
/// local `browser:` computer either. Approval stays here, in this loop.
#[tokio::test]
async fn a_grant_for_one_server_never_covers_another_computer() {
    let env = setup();
    let fake = Fake::new(vec![
        open_call(),
        text("on B"),
        calls(&[(
            "c2",
            "browser_open",
            json!({"url": "https://example.com/b"}),
        )]),
        text("on B again"),
    ]);
    let b = Arc::new(ServerLink::default());
    let c = Arc::new(ServerLink::default());
    let links = || {
        Links(vec![
            (MACHINE_B, Arc::clone(&b)),
            (MACHINE_A, Arc::clone(&c)),
        ])
    };
    let (lp, _local) = server_loop(
        &env,
        &fake,
        MACHINE_B,
        links(),
        Policy::approve_destructive(),
    );
    let mut rx = lp.subscribe();
    let run = lp.run_turn(env.chat, "open").unwrap();
    let Event::ApprovalNeeded(a) = until(&mut rx, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        unreachable!()
    };
    lp.approve(a.call_id, ApprovalScope::AlwaysCategory, env.user)
        .unwrap();
    assert_eq!(lp.wait(run).await.unwrap(), RunState::Succeeded);
    let scope = format!("server:{MACHINE_B}");
    assert_eq!(
        lp.list_grants(env.bot)[0].computer.as_deref(),
        Some(scope.as_str())
    );
    // The same bot now using server A as its computer must ask again.
    let fake2 = Fake::new(vec![open_call(), text("on A")]);
    let (lp2, _local2) = server_loop(
        &env,
        &fake2,
        MACHINE_A,
        links(),
        Policy::approve_destructive(),
    );
    let mut rx2 = lp2.subscribe();
    let run2 = lp2.run_turn(env.chat, "open on A").unwrap();
    let Event::ApprovalNeeded(a2) =
        until(&mut rx2, |e| matches!(e, Event::ApprovalNeeded(_))).await
    else {
        panic!("a grant given for server B covered server A")
    };
    lp2.deny(a2.call_id, env.user, None).unwrap();
    let _ = lp2.wait(run2).await;
    assert!(c.calls.lock().iter().all(|(_, p)| p["op"] != "open"));
}

/// A real `server:<key>` grant and a paused server run survive the boot
/// reconcile while that server is paired; an unknown/unpaired key fails closed.
#[tokio::test]
async fn server_grants_and_paused_runs_survive_a_restart_while_the_server_is_paired() {
    let env = setup();
    let fake = Fake::new(vec![]);
    let server = Arc::new(ServerLink::default());
    seed_desktop_grant(&env, Some(&format!("server:{MACHINE_B}")));
    seed_desktop_grant(&env, Some("server:unpaired-key"));
    let (lp, _local) = server_loop(
        &env,
        &fake,
        MACHINE_B,
        Links(vec![(MACHINE_B, server)]),
        Policy::full(),
    );
    let kept = lp
        .rt
        .create_run(env.user, env.bot, Some(env.chat), "turn")
        .unwrap();
    let lost = lp
        .rt
        .create_run(env.user, env.bot, Some(env.chat), "turn")
        .unwrap();
    for (run, node) in [(kept, MACHINE_B), (lost, "unpaired-key")] {
        env.storage
            .bind_run_computer(run, "server", Some(node), Some("Oracle server"))
            .unwrap();
        lp.rt.pause_queued(run, Some("waiting")).unwrap();
    }
    lp.reconcile_computer_identity(MACHINE_A, Some(&[])).unwrap();
    for g in lp.list_grants(env.bot) {
        let paired = g.computer.as_deref() == Some(format!("server:{MACHINE_B}").as_str());
        assert_eq!(g.inert, !paired, "{:?}", g.computer);
    }
    assert_eq!(env.storage.get_run(kept).unwrap().unwrap().state, "paused");
    assert_eq!(env.storage.get_run(lost).unwrap().unwrap().state, "failed");
}
