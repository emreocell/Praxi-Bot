//! 244-V: the new availability route and bot-scoped sessions, per peer role and
//! over non-canonical paths; target lifecycle notifications; bot retirement.
use super::*;

const ROLES: [crate::PeerRole; 4] = [
    crate::PeerRole::Desktop,
    crate::PeerRole::Phone,
    crate::PeerRole::Node,
    crate::PeerRole::Host,
];

fn role_name(role: crate::PeerRole) -> &'static str {
    match role {
        crate::PeerRole::Desktop => "desktop",
        crate::PeerRole::Phone => "phone",
        crate::PeerRole::Node => "node",
        crate::PeerRole::Host => "host",
    }
}

/// One request as a paired peer of `role` (no bearer: the transport identity is
/// all a peer has).
async fn as_peer(
    t: &Test,
    role: crate::PeerRole,
    method: &Method,
    path: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method.clone())
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    req.extensions_mut().insert(crate::PeerIdentity {
        key: role_name(role).into(),
        role,
    });
    let response = crate::peer_router(t.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn grant(t: &Test, role: crate::PeerRole) {
    t.state.live_view().privileges.lock().insert(
        format!("peer:{}", role_name(role)),
        ScreenPrivileges::default(),
    );
}

fn note(t: &Test) {
    t.state
        .live_view()
        .registry
        .note_unavailable(TargetUnavailable {
            kind: TargetKind::Desktop,
            host_id: "local".into(),
            reason: Reason::Unavailable,
            code: UnavailableCode::NoDisplay,
        });
}

/// Every view route, in canonical and odd spellings.
fn view_requests(t: &Test) -> Vec<(Method, String, Option<Value>)> {
    let target = t.target.info().target_id;
    let session = Uuid::now_v7();
    let create = json!({"target_id":target,"profile":"watch","max_width":960,"max_height":600,
        "codecs":["jpeg"],"client_id":Uuid::now_v7()});
    let mut all = Vec::new();
    for odd in [
        "/v1/view/availability",
        "/v1/view/availability/",
        "//v1/view/availability",
        "/v1//view/availability",
        "/v1/view/%61vailability",
        "/v1/view%2Favailability",
        "/v1/view/./availability",
        "/v1/view/targets",
        "/v1/view/targets/",
        "//v1/view/targets",
        "/v1/view/%74argets",
    ] {
        all.push((Method::GET, odd.to_owned(), None));
    }
    for odd in [
        "/v1/view/sessions",
        "/v1/view/sessions/",
        "//v1/view/sessions",
        "/v1/view/%73essions",
    ] {
        all.push((Method::POST, odd.to_owned(), Some(create.clone())));
    }
    for odd in [
        format!("/v1/view/sessions/{session}/frames"),
        format!("/v1/view/sessions/{session}/frames/"),
        format!("//v1/view/sessions/{session}/frames"),
        format!("/v1/view/sessions/{session}/%66rames"),
    ] {
        all.push((Method::GET, odd, None));
    }
    let takeover = json!({"session_id":session,"request_id":Uuid::now_v7()});
    for odd in [
        "/v1/control/takeover",
        "/v1/control/takeover/",
        "//v1/control/takeover",
        "/v1/control/%74akeover",
    ] {
        all.push((Method::POST, odd.to_owned(), Some(takeover.clone())));
    }
    let input = json!({"v":1,"takeover_id":Uuid::now_v7(),"control_epoch":1,"generation":1,
        "geometry_revision":1,"first_seq":1,"events":[{"type":"text","text":"x"}]});
    for odd in [
        format!("/v1/view/sessions/{session}/input"),
        format!("/v1/view/sessions/{session}/input/"),
        format!("//v1/view/sessions/{session}/input"),
        format!("/v1/view/sessions/{session}/%69nput"),
    ] {
        all.push((Method::POST, odd, Some(input.clone())));
    }
    all
}

/// Nothing may have started: no subscription, no input, no takeover pin.
fn assert_untouched(t: &Test, what: &str) {
    assert_eq!(t.target.source.subscribers(), 0, "{what}: subscribed");
    assert_eq!(
        t.target.sink.calls.load(Ordering::SeqCst),
        0,
        "{what}: input"
    );
    assert_eq!(
        t.target.sink.activations.load(Ordering::SeqCst),
        0,
        "{what}: pin"
    );
    assert!(t.state.hub().takeover().is_none(), "{what}: takeover");
}

#[tokio::test]
async fn node_and_host_peers_and_ungranted_peers_get_nothing_on_any_spelling_of_a_view_route() {
    let t = Test::new();
    note(&t);
    for role in ROLES {
        // Nobody is granted yet. Node/Host are refused by role; Desktop/Phone
        // by the missing screen:view grant. No spelling reaches a handler.
        for (method, path, body) in view_requests(&t) {
            let (status, value) = as_peer(&t, role, &method, &path, body.as_ref()).await;
            assert!(
                !status.is_success(),
                "{} {method} {path} -> {status} {value}",
                role_name(role)
            );
            let text = value.to_string();
            assert!(
                !text.contains("no_display") && !text.contains("hermetic"),
                "{} {method} {path} leaked: {text}",
                role_name(role)
            );
        }
        assert_untouched(&t, role_name(role));
    }
    // A grant never opens a node or a host: the role rule comes first.
    for role in ROLES {
        grant(&t, role);
    }
    for role in [crate::PeerRole::Node, crate::PeerRole::Host] {
        for (method, path, body) in view_requests(&t) {
            let (status, _) = as_peer(&t, role, &method, &path, body.as_ref()).await;
            assert!(
                !status.is_success(),
                "{} {method} {path} -> {status}",
                role_name(role)
            );
        }
        let (status, _) = as_peer(&t, role, &Method::GET, "/v1/view/availability", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{}", role_name(role));
    }
    assert_untouched(&t, "granted node/host");
}

#[tokio::test]
async fn granted_desktop_and_phone_read_availability_and_odd_spellings_never_do_more() {
    let t = Test::new();
    note(&t);
    for role in [crate::PeerRole::Desktop, crate::PeerRole::Phone] {
        grant(&t, role);
        let (status, body) = as_peer(&t, role, &Method::GET, "/v1/view/availability", None).await;
        assert_eq!(status, StatusCode::OK, "{}", role_name(role));
        assert_eq!(body[0]["code"], "no_display");
        // An odd spelling is either not found/refused or exactly the canonical answer.
        for odd in [
            "/v1/view/availability/",
            "//v1/view/availability",
            "/v1//view/availability",
            "/v1/view/%61vailability",
            "/v1/view%2Favailability",
        ] {
            let (status, other) = as_peer(&t, role, &Method::GET, odd, None).await;
            assert!(
                !status.is_success() || other == body,
                "{} {odd} -> {status} {other}",
                role_name(role)
            );
        }
    }
    assert_untouched(&t, "granted desktop/phone reading");
}

/// A bot-scoped panel resolves only to its own bot's browser or the shared desktop.
#[tokio::test]
async fn a_bot_scoped_session_never_resolves_to_another_bots_browser() {
    let t = Test::new();
    let (bot_a, bot_b) = (Uuid::now_v7(), Uuid::now_v7());
    let browser = |bot: Uuid| {
        let target = Arc::new(Target::new());
        target.info.lock().bot_id = Some(bot);
        t.state.live_view().registry.register(target.clone());
        target.info().target_id
    };
    let (a_browser, b_browser) = (browser(bot_a), browser(bot_b));
    let desktop = {
        let target = Arc::new(Target::new());
        {
            let mut info = target.info.lock();
            info.kind = TargetKind::Desktop;
            info.bot_id = None;
        }
        t.state.live_view().registry.register(target.clone());
        target.info().target_id
    };
    let create = |target: Uuid, bot: Option<Uuid>| {
        let mut body = json!({"target_id":target,"profile":"watch","max_width":960,"max_height":600,
            "codecs":["jpeg"],"client_id":Uuid::now_v7()});
        if let Some(bot) = bot {
            body["bot_id"] = json!(bot);
        }
        body
    };
    // Bot A's panel asks for bot B's browser: refused, no session.
    let (status, value) = t
        .call(
            None,
            Method::POST,
            "/v1/view/sessions",
            Some(create(b_browser, Some(bot_a))),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");
    assert!(t.state.live_view().sessions.lock().is_empty());
    // Its own browser and the shared desktop are fine.
    for (target, expect) in [
        (a_browser, StatusCode::CREATED),
        (desktop, StatusCode::CREATED),
    ] {
        let (status, value) = t
            .call(
                None,
                Method::POST,
                "/v1/view/sessions",
                Some(create(target, Some(bot_a))),
            )
            .await;
        assert_eq!(status, expect, "{value}");
    }
}

/// Target lifecycle (added, removed, a note changing) reaches an event subscriber
/// without any control-state change.
#[tokio::test]
async fn target_lifecycle_changes_are_published_without_a_control_event() {
    let t = Test::new();
    let mut rx = t.state.live_view().registry.subscribe_changes();
    assert!(rx.try_recv().is_err());
    let extra = Arc::new(Target::new());
    t.state.live_view().registry.register(extra.clone());
    assert!(rx.try_recv().is_ok(), "register is announced");
    note(&t);
    assert!(rx.try_recv().is_ok(), "a new reason is announced");
    note(&t);
    assert!(rx.try_recv().is_err(), "the same reason again is not");
    t.state.live_view().registry.remove(extra.info().target_id);
    assert!(rx.try_recv().is_ok(), "removal is announced");
}

/// Deleting a bot retires its view target and runs the owners' hooks.
#[tokio::test]
async fn deleting_a_bot_removes_its_view_target_and_retires_its_driver() {
    let t = Test::new();
    let (status, bot) = t
        .call(
            None,
            Method::POST,
            "/v1/bots",
            Some(json!({"profile_toml": "[bot]\nid = \"spare\"\nname = \"Spare\"\n[models]\nexecutor = \"fake/echo\"\n"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{bot}");
    let id = Uuid::parse_str(bot["id"].as_str().unwrap()).unwrap();
    let browser = Arc::new(Target::new());
    browser.info.lock().bot_id = Some(id);
    t.state.live_view().registry.register(browser.clone());
    let retired = Arc::new(Mutex::new(Vec::<Uuid>::new()));
    {
        let retired = Arc::clone(&retired);
        t.state
            .live_view()
            .registry
            .on_bot_retired(move |bot| retired.lock().push(bot));
    }
    let before = t.state.live_view().registry.targets().len();
    let (status, _) = t
        .call(None, Method::DELETE, &format!("/v1/bots/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t.state.live_view().registry.targets().len(), before - 1);
    assert!(
        t.state
            .live_view()
            .registry
            .get(browser.info().target_id)
            .is_none()
    );
    assert_eq!(*retired.lock(), vec![id]);
}
