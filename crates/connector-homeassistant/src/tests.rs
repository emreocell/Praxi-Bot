//! Offline tests against an in-process fake hub.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tool_registry::{ContentPart, ToolContext, ToolError, ToolRegistry};

use crate::{
    HomeAssistantClient, HomeAssistantConfig, HomeAssistantConnector, TOOL_NAMES, hub_policy,
    register,
};

/// `(request line, authorization header, body)` of each request seen.
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

async fn fake_hub() -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    if let Some((h, b)) = text.split_once("\r\n\r\n") {
                        let len = h
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if b.len() >= len {
                            break (h.to_owned(), b.to_owned());
                        }
                    }
                };
                let request = head.lines().next().unwrap_or("").to_owned();
                let auth = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_owned();
                log.lock().unwrap().push((request.clone(), auth, body));
                let payload = if request.starts_with("GET /api/states/light.kitchen") {
                    json!({"entity_id":"light.kitchen","state":"off","attributes":{"friendly_name":"Kitchen"}})
                } else if request.starts_with("GET /api/states ") {
                    json!([
                        {"entity_id":"light.kitchen","state":"off","attributes":{"friendly_name":"Kitchen"}},
                        {"entity_id":"sensor.temp","state":"21","attributes":{}}
                    ])
                } else if request.starts_with("POST /api/services/") {
                    json!([{"entity_id":"light.kitchen","state":"on"}])
                } else {
                    json!({})
                }
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen)
}

fn cfg_for(port: u16, allowed: &[&str]) -> HomeAssistantConfig {
    HomeAssistantConfig {
        url: format!("http://127.0.0.1:{port}"),
        token: SecretRef::new("homeassistant/token").unwrap(),
        allowed_domains: allowed.iter().map(|s| (*s).to_owned()).collect(),
        timeout_secs: 5,
        trusted_services: Vec::new(),
        bots: None,
    }
}

fn store() -> Arc<dyn SecretStore> {
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    store.set("homeassistant/token", "ha-secret-token").unwrap();
    store
}

fn connector_for(cfg: HomeAssistantConfig) -> Arc<HomeAssistantConnector> {
    let conn = Arc::new(HomeAssistantConnector::new(hub_policy(&[])));
    assert_eq!(conn.replace(Some(cfg), &store()), None);
    conn
}

fn registry_with(conn: &Arc<HomeAssistantConnector>) -> ToolRegistry {
    let reg = ToolRegistry::new();
    assert_eq!(register(&reg, conn), 3);
    reg
}

fn registry_for(port: u16, allowed: &[&str]) -> ToolRegistry {
    registry_with(&connector_for(cfg_for(port, allowed)))
}

async fn call(reg: &ToolRegistry, name: &str, input: Value) -> Result<Value, ToolError> {
    let out = reg
        .get(name)
        .unwrap()
        .call(&ToolContext::default(), input)
        .await?;
    match out.content.into_iter().next() {
        Some(ContentPart::Json { data }) => Ok(data),
        other => panic!("unexpected output {other:?}"),
    }
}

#[tokio::test]
async fn reads_and_service_call_use_the_stored_token() {
    let (port, seen) = fake_hub().await;
    let reg = registry_for(port, &[]);

    let list = call(&reg, "ha_list_entities", json!({"domain":"light"}))
        .await
        .unwrap();
    assert_eq!(list["untrusted_external_content"], true);
    assert_eq!(list["returned"], 1);
    assert_eq!(list["entities"][0]["name"], "Kitchen");

    let st = call(&reg, "ha_get_state", json!({"entity_id":"light.kitchen"}))
        .await
        .unwrap();
    assert_eq!(st["state"]["state"], "off");

    call(
        &reg,
        "ha_call_service",
        json!({"domain":"light","service":"turn_on","entity_id":"light.kitchen","data":{"brightness_pct":40}}),
    )
    .await
    .unwrap();
    let log = seen.lock().unwrap();
    let (line, auth, body) = log.last().unwrap();
    assert!(
        line.starts_with("POST /api/services/light/turn_on"),
        "{line}"
    );
    assert!(auth.ends_with("Bearer ha-secret-token"), "{auth}");
    let body: Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["entity_id"], "light.kitchen");
    assert_eq!(body["brightness_pct"], 40);
}

#[tokio::test]
async fn code_running_and_unlisted_services_never_reach_the_hub() {
    let (port, seen) = fake_hub().await;
    let reg = registry_for(port, &["light"]);
    for (domain, service) in [
        ("shell_command", "x"),
        ("homeassistant", "restart"),
        ("lock", "unlock"),
    ] {
        let err = call(
            &reg,
            "ha_call_service",
            json!({"domain":domain,"service":service}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidInput(_)),
            "{domain}.{service}: {err}"
        );
    }
    assert!(seen.lock().unwrap().is_empty());
}

/// A hub that answers every request with a fixed status line (and a redirect target).
async fn canned_hub(status: &'static str, location: Option<String>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let location = location.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 4096];
                let _ = sock.read(&mut chunk).await;
                let loc = location.map_or_else(String::new, |l| format!("Location: {l}\r\n"));
                let resp = format!(
                    "HTTP/1.1 {status}\r\n{loc}Content-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

fn client_for(cfg: HomeAssistantConfig) -> HomeAssistantClient {
    HomeAssistantClient::new(cfg, store()).unwrap()
}

#[test]
fn public_plain_http_is_refused_and_the_effects_are_declared() {
    let conn = Arc::new(HomeAssistantConnector::new(hub_policy(&[])));
    let mut cfg = cfg_for(8123, &[]);
    cfg.url = "http://ha.example.com:8123".into();
    let problem = conn.replace(Some(cfg), &store()).unwrap();
    assert!(problem.contains("must be https"), "{problem}");
    assert!(!conn.is_configured());

    let reg = registry_with(&conn);
    let specs = reg.list();
    let effect = |n: &str| specs.iter().find(|s| s.name == n).unwrap().effect;
    // Calling a service is a write (Full: no card); only the named services always ask.
    assert_eq!(effect("ha_call_service"), tool_registry::Effect::Write);
    assert_eq!(effect("ha_get_state"), tool_registry::Effect::Read);
    assert_eq!(effect("ha_list_entities"), tool_registry::Effect::Read);
    // No usable hub: nothing is offered to a bot.
    assert!(!reg.get("ha_get_state").unwrap().visible_to(None));
}

#[tokio::test]
async fn the_hub_is_replaced_and_removed_while_the_tools_stay_registered() {
    let (port, _) = fake_hub().await;
    let conn = Arc::new(HomeAssistantConnector::new(hub_policy(&[])));
    let reg = registry_with(&conn);
    let tool = reg.get("ha_list_entities").unwrap();
    assert!(!tool.visible_to(Some("bot-a")));
    let err = call(&reg, "ha_list_entities", json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err}");

    assert_eq!(conn.replace(Some(cfg_for(port, &[])), &store()), None);
    assert!(tool.visible_to(Some("bot-a")));
    let list = call(&reg, "ha_list_entities", json!({})).await.unwrap();
    assert_eq!(list["returned"], 2);

    assert_eq!(conn.replace(None, &store()), None);
    assert!(!tool.visible_to(Some("bot-a")));
    assert!(call(&reg, "ha_list_entities", json!({})).await.is_err());
}

#[tokio::test]
async fn only_the_listed_bots_see_and_use_the_hub() {
    let (port, seen) = fake_hub().await;
    let mut cfg = cfg_for(port, &[]);
    cfg.bots = Some(vec!["bot-a".into()]);
    let conn = connector_for(cfg);
    let reg = registry_with(&conn);
    for name in TOOL_NAMES {
        let tool = reg.get(name).unwrap();
        assert!(tool.visible_to(Some("bot-a")), "{name}");
        assert!(!tool.visible_to(Some("bot-b")), "{name}");
        assert!(!tool.visible_to(None), "{name}");
    }
    // Hiding is not the boundary: a call by another bot is refused too.
    let mut ctx = ToolContext {
        bot_id: Some("bot-b".into()),
        ..ToolContext::default()
    };
    let err = reg
        .get("ha_list_entities")
        .unwrap()
        .call(&ctx, json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err}");
    assert!(seen.lock().unwrap().is_empty());
    ctx.bot_id = Some("bot-a".into());
    reg.get("ha_list_entities")
        .unwrap()
        .call(&ctx, json!({}))
        .await
        .unwrap();

    // An empty list is nobody; no list is everybody.
    let mut cfg = cfg_for(port, &[]);
    cfg.bots = Some(vec![]);
    assert_eq!(conn.replace(Some(cfg), &store()), None);
    assert!(!reg.get("ha_get_state").unwrap().visible_to(Some("bot-a")));
    assert_eq!(conn.replace(Some(cfg_for(port, &[])), &store()), None);
    assert!(reg.get("ha_get_state").unwrap().visible_to(None));
}

fn asks(trusted: &[&str], input: &Value) -> bool {
    let mut cfg = cfg_for(1, &[]);
    cfg.trusted_services = trusted.iter().map(|s| (*s).to_owned()).collect();
    client_for(cfg).needs_confirmation(input)
}

#[test]
fn opening_unlocking_and_disarming_ask_and_ordinary_calls_do_not() {
    let call = |domain: &str, service: &str, entity: Option<&str>| json!({"domain": domain, "service": service, "entity_id": entity});
    for (domain, service, entity) in [
        ("lock", "unlock", Some("lock.front_door")),
        ("lock", "lock", Some("lock.front_door")),
        ("lock", "open", None),
        (
            "alarm_control_panel",
            "alarm_disarm",
            Some("alarm_control_panel.home"),
        ),
        ("alarm_control_panel", "alarm_arm_away", None),
        ("cover", "open_cover", Some("cover.garage")),
        ("cover", "close_cover", Some("cover.garage")),
        ("cover", "set_cover_position", Some("cover.blind")),
        // by the name of the service, in any domain
        ("valve", "open_valve", Some("valve.water")),
        ("anything", "unlock_thing", None),
        // by what it acts on: a garage door is often a plain switch or button
        ("switch", "turn_on", Some("switch.garage_door")),
        ("button", "press", Some("button.main_gate")),
        ("light", "turn_on", Some("light.kitchen, lock.front_door")),
        // other actions that could do any of the above
        ("script", "turn_on", Some("script.goodnight")),
        ("automation", "trigger", Some("automation.x")),
        ("scene", "turn_on", Some("scene.movie")),
        // a domain nobody listed as a plain device asks: raw protocols and helpers can reach a
        // lock or a garage door without naming one
        ("mqtt", "publish", None),
        ("zwave_js", "set_value", Some("zwave_js.x")),
        ("input_button", "press", Some("input_button.x")),
        ("input_boolean", "turn_on", Some("input_boolean.x")),
        ("button", "press", Some("button.x")),
        ("siren", "turn_on", Some("siren.x")),
        ("valve", "close_valve", Some("valve.water")),
        // a switch aimed at nothing in particular, or at everything
        ("switch", "turn_on", None),
        ("switch", "turn_off", Some("all")),
        // spelled to slip past a filter
        ("LOCK", "UNLOCK", None),
        (" Cover ", " Open_Cover ", None),
        ("light", "turn_on", Some("\u{1f}cover.main")),
        ("light", "turn_on", Some("loc\u{212A}.deadbolt")),
        ("light", "turn_on", Some("Light.Kitchen")),
        ("light", "turn_on", Some("light.kitchen;lock.front")),
    ] {
        assert!(
            asks(&[], &call(domain, service, entity)),
            "{domain}.{service} {entity:?} must ask"
        );
    }
    // not stated clearly: ask
    assert!(asks(&[], &json!({"domain": "light"})));
    assert!(asks(&[], &json!({"service": "turn_on"})));
    assert!(asks(&[], &json!({"domain": 5, "service": "turn_on"})));

    for (domain, service, entity) in [
        ("light", "turn_on", Some("light.kitchen")),
        ("light", "turn_off", Some("light.kitchen")),
        ("climate", "set_temperature", Some("climate.living_room")),
        ("switch", "turn_off", Some("switch.fan")),
        ("media_player", "media_play", Some("media_player.tv")),
        ("homeassistant", "turn_on", Some("light.kitchen")),
        ("homeassistant", "toggle", Some("light.a,light.b")),
        ("fan", "turn_on", Some("fan.bedroom")),
        ("vacuum", "start", Some("vacuum.robot")),
        ("number", "set_value", Some("number.volume")),
        ("notify", "mobile_app_phone", None),
        // plain devices may be aimed by area or all
        ("light", "turn_off", Some("all")),
        ("light", "turn_off", None),
        ("media_player", "media_pause", Some("all")),
    ] {
        assert!(
            !asks(&[], &call(domain, service, entity)),
            "{domain}.{service} {entity:?} must not ask"
        );
    }
}

#[test]
fn an_area_device_or_all_target_asks_outside_the_area_friendly_domains() {
    let with = |domain: &str, service: &str, data: Value| json!({"domain": domain, "service": service, "data": data});
    // A switch, a button or an input helper aimed at an area may be the garage door.
    for (domain, service) in [
        ("switch", "turn_on"),
        ("button", "press"),
        ("input_boolean", "turn_on"),
        ("siren", "turn_on"),
    ] {
        for data in [
            json!({"area_id": "garage"}),
            json!({"device_id": "d1"}),
            json!({"floor_id": "f"}),
            json!({"label_id": "doors"}),
            json!({"entity_id": "all"}),
        ] {
            assert!(
                asks(&[], &with(domain, service, data.clone())),
                "{domain} {data}"
            );
        }
    }
    // Lights, fans, climate and media players are plain: the area is fine.
    for domain in ["light", "fan", "climate", "media_player"] {
        assert!(
            !asks(
                &[],
                &with(domain, "turn_off", json!({"area_id": "kitchen"}))
            ),
            "{domain}"
        );
    }
    // ... but an entity that is a door, named in any way, still asks.
    assert!(asks(
        &[],
        &with(
            "light",
            "turn_on",
            json!({"entity_id": ["light.kitchen", "lock.front"]})
        )
    ));
    assert!(asks(
        &[],
        &json!({"domain":"light","service":"turn_on","entity_id":{"a":1}})
    ));
}

#[test]
fn the_generic_homeassistant_service_asks_unless_it_names_plain_entities() {
    // These fan out to entities only the hub can resolve (area, device, all) or hide a lock.
    for input in [
        json!({"domain":"homeassistant","service":"turn_off","entity_id":"all"}),
        json!({"domain":"homeassistant","service":"turn_off"}),
        json!({"domain":"homeassistant","service":"turn_on","data":{"area_id":"hall"}}),
        json!({"domain":"homeassistant","service":"turn_on","entity_id":"light.a","data":{"device_id":"d"}}),
        json!({"domain":"homeassistant","service":"turn_on","data":{"entity_id":["light.a","lock.b"]}}),
        json!({"domain":"homeassistant","service":"turn_on","entity_id":"cover.blind"}),
    ] {
        assert!(asks(&[], &input), "{input}");
        // The owner cannot waive the whole generic service.
        assert!(
            asks(&["homeassistant.turn_on", "homeassistant.turn_off"], &input),
            "{input}"
        );
    }
}

#[test]
fn a_risky_entity_in_a_target_object_asks_like_one_in_entity_id() {
    // The same risky entity, written in every place a target can sit.
    let risky = [
        json!({"domain":"light","service":"turn_on","target":{"entity_id":"lock.front_door"}}),
        json!({"domain":"light","service":"turn_on","target":{"entity_id":["light.a","cover.garage"]}}),
        json!({"domain":"light","service":"turn_on","data":{"target":{"entity_id":"lock.front_door"}}}),
        json!({"domain":"light","service":"turn_on","entity_id":"light.a","target":{"entity_id":"lock.b"}}),
        json!({"domain":"light","service":"turn_on","entity_id":"light.a","data":{"target":{"entity_id":"switch.garage_door"}}}),
        json!({"domain":"homeassistant","service":"turn_on","target":{"entity_id":"lock.front_door"}}),
        json!({"domain":"homeassistant","service":"turn_on","entity_id":"light.a","target":{"entity_id":"lock.b"}}),
        json!({"domain":"homeassistant","service":"turn_on","data":{"target":{"entity_id":"lock.b"}}}),
        // not readable as a target at all
        json!({"domain":"light","service":"turn_on","target":"lock.front_door"}),
        json!({"domain":"light","service":"turn_on","data":{"target":["lock.front_door"]}}),
        json!({"domain":"light","service":"turn_on","target":{"entity_id":"Lock.Front"}}),
        json!({"domain":"light","service":"turn_on","target":{"entity_id":{"x":1}}}),
    ];
    for input in &risky {
        assert!(asks(&[], input), "{input}");
        // The generic service can never be waived by the owner's list.
        if input["domain"] == "homeassistant" {
            assert!(asks(&["homeassistant.turn_on"], input), "{input}");
        }
    }
    // An area, device or `all` in a target object reaches things nobody can see, like the flat form.
    for target in [
        json!({"area_id":"garage"}),
        json!({"device_id":"d1"}),
        json!({"floor_id":"f"}),
        json!({"label_id":"doors"}),
        json!({"entity_id":"all"}),
    ] {
        for place in ["target", "data.target"] {
            let input = if place == "target" {
                json!({"domain":"switch","service":"turn_on","entity_id":"switch.lamp","target":target})
            } else {
                json!({"domain":"switch","service":"turn_on","entity_id":"switch.lamp","data":{"target":target}})
            };
            assert!(asks(&[], &input), "{input}");
            let generic = json!({"domain":"homeassistant","service":"turn_on","target":target});
            assert!(asks(&[], &generic), "{generic}");
        }
    }
    // Plain targets in a target object do not ask, exactly like the flat form.
    for input in [
        json!({"domain":"light","service":"turn_on","target":{"entity_id":"light.kitchen"}}),
        json!({"domain":"light","service":"turn_on","data":{"target":{"area_id":"kitchen"}}}),
        json!({"domain":"homeassistant","service":"turn_on","target":{"entity_id":["light.a","switch.lamp"]}}),
    ] {
        assert!(!asks(&[], &input), "{input}");
    }
}

#[tokio::test]
async fn a_target_object_with_odd_ids_never_reaches_the_hub() {
    let (port, seen) = fake_hub().await;
    let reg = registry_for(port, &[]);
    for input in [
        json!({"domain":"light","service":"turn_on","data":{"target":{"entity_id":"Light.Kitchen"}}}),
        json!({"domain":"light","service":"turn_on","data":{"target":{"entity_id":"light.a;lock.b"}}}),
        json!({"domain":"light","service":"turn_on","data":{"target":{"entity_id":["light.a", 5]}}}),
        json!({"domain":"light","service":"turn_on","data":{"target":"lock.front_door"}}),
    ] {
        let err = call(&reg, "ha_call_service", input.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{input}: {err}");
    }
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn the_owners_list_stops_the_card_for_exactly_those_services() {
    let open = json!({"domain":"cover","service":"open_cover","entity_id":"cover.garage_door"});
    let close = json!({"domain":"cover","service":"close_cover","entity_id":"cover.garage_door"});
    let trusted = ["cover.open_cover", " Switch.Turn_On "];
    assert!(asks(&[], &open));
    assert!(!asks(&trusted, &open));
    assert!(asks(&trusted, &close), "only the listed service");
    // A hand-written domain-wide entry is not a service name and trusts nothing.
    assert!(asks(&["cover"], &open));
    assert!(asks(&["cover.*"], &open));
    // The same for a name-based trigger (a garage switch).
    let garage = json!({"domain":"switch","service":"turn_on","entity_id":"switch.garage_door"});
    assert!(!asks(&trusted, &garage));
    assert!(asks(&[], &garage));
}

#[tokio::test]
async fn the_tool_asks_for_what_the_client_says_and_not_for_reads() {
    let (port, _) = fake_hub().await;
    let reg = registry_for(port, &[]);
    let ask = |name: &str, input: &Value| reg.get(name).unwrap().always_ask_for(input);
    assert!(ask(
        "ha_call_service",
        &json!({"domain":"lock","service":"unlock"})
    ));
    assert!(!ask(
        "ha_call_service",
        &json!({"domain":"light","service":"turn_on","entity_id":"light.kitchen"})
    ));
    // The Full level, which runs every write without a card, still asks for the same calls.
    let full = |input: &Value| {
        reg.get("ha_call_service")
            .unwrap()
            .full_access_asks_for(input)
    };
    assert!(full(&json!({"domain":"lock","service":"unlock"})));
    assert!(!full(
        &json!({"domain":"light","service":"turn_on","entity_id":"light.kitchen"})
    ));
    // Reads never carry the rule, whatever they name.
    assert!(!ask(
        "ha_get_state",
        &json!({"entity_id":"lock.front_door"})
    ));
    assert!(!ask("ha_list_entities", &json!({"domain":"lock"})));
}

#[tokio::test]
async fn a_test_counts_the_entities_and_never_shows_the_token() {
    let (port, seen) = fake_hub().await;
    let probe = client_for(cfg_for(port, &[])).probe().await.unwrap();
    assert_eq!(probe.entities, 2);
    {
        let log = seen.lock().unwrap();
        assert!(log[0].0.starts_with("GET /api/ "), "{:?}", log[0].0);
        assert!(log[1].0.starts_with("GET /api/states "), "{:?}", log[1].0);
    }

    for (status, words) in [
        ("401 Unauthorized", "refused the access token"),
        ("403 Forbidden", "refused the access token"),
        ("404 Not Found", "not found"),
        ("500 Internal Server Error", "HTTP 500"),
    ] {
        let port = canned_hub(status, None).await;
        let err = client_for(cfg_for(port, &[])).probe().await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains(words), "{status}: {text}");
        assert!(!text.contains("ha-secret-token"), "{text}");
    }
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    let (elsewhere, seen) = fake_hub().await;
    let port = canned_hub(
        "302 Found",
        Some(format!("http://127.0.0.1:{elsewhere}/api/states")),
    )
    .await;
    let err = client_for(cfg_for(port, &[])).probe().await.unwrap_err();
    assert!(err.to_string().contains("redirect"), "{err}");
    // The token went to the hub the owner named, and to nobody else.
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn only_the_owners_address_is_allowed() {
    let policy = |protected: &[SocketAddr]| hub_policy(protected);
    let make = |url: &str, protected: &[SocketAddr]| {
        let mut cfg = cfg_for(1, &[]);
        cfg.url = url.into();
        HomeAssistantClient::with_policy(cfg, store(), &policy(protected)).map(|_| ())
    };
    let daemon = [
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
    ];
    // The daemon's own API, the cloud metadata address, link-local space, credentials in the
    // address and plain http to a public host never get a client.
    for url in ["http://127.0.0.1:8123/", "http://[::1]:8123/"] {
        assert!(make(url, &[]).is_ok(), "{url}");
        assert!(make(url, &daemon).is_err(), "{url}");
    }
    for url in [
        "http://169.254.169.254/",
        "https://169.254.169.254/",
        "http://[fe80::1]:8123/",
        "https://user:pw@ha.example.com/",
        "http://ha.example.com:8123/",
        "file:///etc/passwd",
        "ftp://ha.local/",
        "not a url",
    ] {
        assert!(make(url, &[]).is_err(), "{url}");
    }
    // A home network, loopback, Tailscale and a certificate-bearing name are fine.
    for url in [
        "http://192.168.1.10:8123",
        "http://10.0.0.5:8123/",
        "http://100.64.0.5:8123/",
        "http://homeassistant.local:8123/",
        "http://homeassistant:8123/",
        "https://ha.example.com/",
        "https://ha.example.com/sub/path",
    ] {
        assert!(make(url, &[]).is_ok(), "{url}: {:?}", make(url, &[]));
    }
}

#[test]
fn a_name_that_leads_to_the_daemons_own_port_is_refused() {
    // Pretend the fake hub is the daemon: every port of loopback is protected.
    let protected = [
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
    ];
    let mut cfg = cfg_for(8123, &[]);
    cfg.url = "http://localhost:8123".into();
    let err = HomeAssistantClient::with_policy(cfg, store(), &hub_policy(&protected))
        .map(|_| ())
        .unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
}

#[test]
fn the_config_file_round_trips_without_a_bots_key_meaning_every_bot() {
    let text = "url = \"http://192.168.1.10:8123\"\ntoken = \"homeassistant/token\"\n";
    let cfg: HomeAssistantConfig = toml::from_str(text).unwrap();
    assert_eq!(cfg.bots, None);
    assert_eq!(cfg.trusted_services, Vec::<String>::new());
    let back = toml::to_string(&cfg).unwrap();
    assert!(!back.contains("bots"), "{back}");
    let some: HomeAssistantConfig = toml::from_str(&format!("{text}bots = [\"a\"]\n")).unwrap();
    assert_eq!(some.bots, Some(vec!["a".to_owned()]));
}

#[tokio::test]
async fn odd_entity_ids_never_reach_the_hub() {
    let (port, seen) = fake_hub().await;
    let reg = registry_for(port, &[]);
    for input in [
        json!({"domain":"light","service":"turn_on","entity_id":"Light.Kitchen"}),
        json!({"domain":"light","service":"turn_on","entity_id":"\u{1f}cover.main"}),
        json!({"domain":"light","service":"turn_on","entity_id":"loc\u{212A}.deadbolt"}),
        json!({"domain":"light","service":"turn_on","entity_id":"light.a;lock.b"}),
        json!({"domain":"light","service":"turn_on","data":{"entity_id":["light.a", 5]}}),
        json!({"domain":"light","service":"turn_on","data":{"entity_id":{"x":"light.a"}}}),
    ] {
        let err = call(&reg, "ha_call_service", input.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{input}: {err}");
    }
    assert!(seen.lock().unwrap().is_empty());
    // Ordinary spellings go through: a list, a comma list, all.
    for input in [
        json!({"domain":"light","service":"turn_on","entity_id":"light.a, light.b"}),
        json!({"domain":"light","service":"turn_off","data":{"entity_id":["light.a","light.b"]}}),
        json!({"domain":"light","service":"turn_off","entity_id":"all"}),
    ] {
        call(&reg, "ha_call_service", input).await.unwrap();
    }
}

#[tokio::test]
async fn an_unreachable_hub_error_does_not_name_the_address() {
    let port = {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let err = client_for(cfg_for(port, &[])).probe().await.unwrap_err();
    let text = err.to_string();
    assert!(
        !text.contains("127.0.0.1") && !text.contains(&port.to_string()),
        "{text}"
    );
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused_while_it_is_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Announces no length and keeps sending: the client must stop on its own.
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut chunk = [0u8; 4096];
        let _ = sock.read(&mut chunk).await;
        let _ = sock
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
            )
            .await;
        let block = vec![b' '; 64 * 1024];
        for _ in 0..200 {
            if sock.write_all(&block).await.is_err() {
                return;
            }
        }
    });
    let err = client_for(cfg_for(port, &[])).probe().await.unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
}
