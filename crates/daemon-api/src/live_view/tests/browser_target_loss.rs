//! Exercise daemon recovery against the real browser adapter over hermetic CDP.
use super::*;
use browser_cdp::{BrowserViewConfig, BrowserViewTarget, CdpClient, Page};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn browser_target_loss_reset_retry_and_local_recover_clear_the_latch() {
    // Page drop may leave the tab alive, or the session may actually be gone.
    // Both must terminate; the live session must receive the owed Shift up.
    for (session_gone, retry_first) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let commands = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorded = commands.clone();
        let gone = Arc::new(AtomicBool::new(false));
        let server_gone = gone.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let command: Value = serde_json::from_str(&text).unwrap();
                recorded.lock().push(command.clone());
                let reply = if server_gone.load(Ordering::SeqCst)
                    && command["method"] == "Input.dispatchKeyEvent"
                {
                    json!({"id":command["id"],"error":{"code":-32001,"message":"Session with given id not found"}})
                } else {
                    json!({"id":command["id"],"result":{"sessionId":"original-session"}})
                };
                if ws.send(Message::text(reply.to_string())).await.is_err() {
                    break;
                }
            }
        });
        let client = CdpClient::connect(&format!("ws://{addr}")).await.unwrap();
        let page = Page::attach(client.clone(), "original-target")
            .await
            .unwrap();
        let t = Test::new();
        let (_id, owned, _body) = t.active(Some("alice")).await;
        // Replace only the synthetic backend, keeping the active daemon takeover
        // and its target identity. Recovery now runs the real adapter methods.
        let target = BrowserViewTarget::new_idle(BrowserViewConfig {
            target_id: owned.target_id,
            ..BrowserViewConfig::default()
        });
        target.attach_page(&page).unwrap();
        t.state.live_view().registry.register(target.clone());
        let sink = target.input_sink().unwrap();
        sink.set_takeover_active(true).await.unwrap();
        sink.inject(&InputEvent::Key {
            phase: KeyPhase::Down,
            code: "ShiftLeft".into(),
            key: "Shift".into(),
            location: 1,
            repeat: false,
            modifiers: vec![Modifier::Shift],
        })
        .await
        .unwrap();
        commands.lock().clear();
        gone.store(session_gone, Ordering::SeqCst);
        drop(page);
        assert!(!client.is_closed());
        assert!(!target.info().capabilities.input);
        assert!(t.state.hub().interrupt(owned.id, Reason::Unavailable));
        t.state.hub().mark_reset_pending();
        if retry_first {
            tokio::time::timeout(Duration::from_secs(2), retry_owed_reset(&t.state))
                .await
                .expect("heartbeat reset retry must terminate");
            assert!(!t.state.hub().override_pending().0, "reset debt cleared");
        }
        assert!(
            t.state.hub().latched().is_some(),
            "only explicit recovery clears the latch"
        );
        let (status, value) = tokio::time::timeout(
            Duration::from_secs(2),
            t.call(None, Method::POST, "/v1/control/recover", None),
        )
        .await
        .expect("local recovery must terminate");
        assert_eq!(status, StatusCode::OK, "{value}");
        assert!(t.state.hub().takeover().is_none());
        assert!(t.state.hub().latched().is_none());
        assert!(!target.status().takeover_active);
        let ups: Vec<_> = commands
            .lock()
            .iter()
            .filter(|c| c["method"] == "Input.dispatchKeyEvent")
            .cloned()
            .collect();
        assert_eq!(ups.len(), 1, "one owed up; no retained dead-session debt");
        assert_eq!(ups[0]["sessionId"], "original-session");
        assert_eq!(ups[0]["params"]["type"], "keyUp");
        assert_eq!(ups[0]["params"]["code"], "ShiftLeft");
        server.abort();
    }
}
