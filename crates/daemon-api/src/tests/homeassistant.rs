//! `/v1/homeassistant*` against an in-memory `HomeAssistantControl`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::{T, TOKEN, call, call_token_only, send, setup};
use crate::homeassistant::{
    HomeAssistantControl, HomeAssistantControlError, HomeAssistantDto, HomeAssistantInput,
    HomeAssistantSaved, HomeAssistantTestInput, HomeAssistantTestReport,
};

const SECRET: &str = "eyJ.long-lived.token-writeonly";

#[derive(Default)]
struct Fake {
    saved: Mutex<Option<(HomeAssistantDto, String)>>,
}

#[async_trait]
impl HomeAssistantControl for Fake {
    fn get(&self) -> HomeAssistantDto {
        self.saved.lock().as_ref().map_or(
            HomeAssistantDto {
                configured: false,
                url: None,
                token_set: false,
                trusted_services: Vec::new(),
                bots: None,
            },
            |(d, _)| d.clone(),
        )
    }

    async fn save(
        &self,
        input: HomeAssistantInput,
    ) -> Result<HomeAssistantSaved, HomeAssistantControlError> {
        if !input.url.starts_with("http://192.168.") {
            return Err(HomeAssistantControlError::Invalid(
                "not a home address".into(),
            ));
        }
        let mut saved = self.saved.lock();
        let token = match (&input.token, saved.as_ref()) {
            (Some(t), _) | (None, Some((_, t))) => t.clone(),
            (None, None) => {
                return Err(HomeAssistantControlError::Invalid(
                    "a token is needed".into(),
                ));
            }
        };
        let bots = saved.as_ref().and_then(|(d, _)| d.bots.clone());
        let dto = HomeAssistantDto {
            configured: true,
            url: Some(input.url),
            token_set: true,
            trusted_services: input.trusted_services,
            bots,
        };
        *saved = Some((dto.clone(), token));
        Ok(HomeAssistantSaved {
            settings: dto,
            entities: 12,
        })
    }

    async fn set_bots(
        &self,
        bots: Option<Vec<String>>,
    ) -> Result<HomeAssistantDto, HomeAssistantControlError> {
        let mut saved = self.saved.lock();
        let (dto, _) = saved
            .as_mut()
            .ok_or_else(|| HomeAssistantControlError::NotFound("Home Assistant".into()))?;
        dto.bots = bots;
        Ok(dto.clone())
    }

    async fn remove(&self) -> Result<(), HomeAssistantControlError> {
        self.saved
            .lock()
            .take()
            .map(|_| ())
            .ok_or_else(|| HomeAssistantControlError::NotFound("Home Assistant".into()))
    }

    async fn test(
        &self,
        input: HomeAssistantTestInput,
    ) -> Result<HomeAssistantTestReport, HomeAssistantControlError> {
        match (&input.url, &input.token) {
            (None, None) if self.saved.lock().is_none() => {
                Err(HomeAssistantControlError::NotFound("Home Assistant".into()))
            }
            (None, None) => Ok(HomeAssistantTestReport {
                ok: true,
                entities: Some(12),
                error: None,
            }),
            (Some(_), Some(t)) if t == "wrong" => Ok(HomeAssistantTestReport {
                ok: false,
                entities: None,
                error: Some("The hub refused the access token.".into()),
            }),
            (Some(_), Some(_)) => Ok(HomeAssistantTestReport {
                ok: true,
                entities: Some(3),
                error: None,
            }),
            _ => Err(HomeAssistantControlError::Invalid(
                "an address and a token go together".into(),
            )),
        }
    }
}

fn with_ha() -> (T, Arc<Fake>) {
    let t = setup();
    let ha = Arc::new(Fake::default());
    t.state
        .set_homeassistant(Arc::clone(&ha) as Arc<dyn HomeAssistantControl>);
    (t, ha)
}

fn body() -> Value {
    json!({"url": "http://192.168.1.10:8123", "token": SECRET})
}

fn every_route() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (Method::GET, "/v1/homeassistant", None),
        (Method::PUT, "/v1/homeassistant", Some(body())),
        (
            Method::PUT,
            "/v1/homeassistant/bots",
            Some(json!({"bots": null})),
        ),
        (Method::DELETE, "/v1/homeassistant", None),
        (Method::POST, "/v1/homeassistant/test", Some(json!({}))),
    ]
}

#[tokio::test]
async fn the_routes_need_the_bearer_token_and_a_good_origin() {
    let (t, _) = with_ha();
    for (m, p, b) in every_route() {
        let r = send(&t.app, m.clone(), p, b.clone(), &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        let auth = format!("Bearer {TOKEN}");
        let r = send(
            &t.app,
            m.clone(),
            p,
            b,
            &[("origin", "https://evil.com"), ("authorization", &auth)],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {p}");
    }
}

#[tokio::test]
async fn every_route_is_owner_only_reads_included_and_a_token_holder_changes_nothing() {
    let (t, ha) = with_ha();
    for (m, p, b) in every_route() {
        let r = call_token_only(&t, m.clone(), p, b).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {p}");
        assert_eq!(r.json()["error"]["code"], "owner_key_required", "{m} {p}");
    }
    // The refused calls reached nothing.
    assert!(ha.saved.lock().is_none());
}

#[tokio::test]
async fn without_a_manager_the_routes_return_503() {
    let t = setup();
    for (m, p, b) in every_route() {
        let r = call(&t, m, p, b).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{p}");
    }
}

#[tokio::test]
async fn setup_is_saved_read_limited_to_bots_and_removed_and_the_token_is_never_echoed() {
    let (t, ha) = with_ha();
    let g = call(&t, Method::GET, "/v1/homeassistant", None).await;
    assert_eq!(g.json()["configured"], false);

    let r = call(&t, Method::PUT, "/v1/homeassistant", Some(body())).await;
    let text = String::from_utf8_lossy(&r.body).into_owned();
    assert_eq!(r.status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET), "token leaked: {text}");
    let v: Value = r.json();
    assert_eq!(v["entities"], 12);
    assert_eq!(v["settings"]["token_set"], true);
    assert_eq!(v["settings"]["url"], "http://192.168.1.10:8123");
    assert_eq!(ha.saved.lock().as_ref().unwrap().1, SECRET);

    // Saved again without the token and with a list of allowed services: the token is kept.
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant",
        Some(json!({"url": "http://192.168.1.10:8123", "trusted_services": ["cover.open_cover"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json()["settings"]["trusted_services"][0],
        "cover.open_cover"
    );
    assert_eq!(ha.saved.lock().as_ref().unwrap().1, SECRET);

    let g = call(&t, Method::GET, "/v1/homeassistant", None).await;
    assert!(!String::from_utf8_lossy(&g.body).contains(SECRET));
    assert_eq!(g.json()["bots"], Value::Null);

    // Bots: a list, an empty list (nobody) and null (everybody).
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant/bots",
        Some(json!({"bots": ["b1", "b2"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["bots"], json!(["b1", "b2"]));
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant/bots",
        Some(json!({"bots": []})),
    )
    .await;
    assert_eq!(r.json()["bots"], json!([]));
    // The field must be there: an empty body never turns into "every bot".
    for bad in [json!({}), json!({"bot": ["b1"]})] {
        let r = call(&t, Method::PUT, "/v1/homeassistant/bots", Some(bad)).await;
        assert!(r.status.is_client_error(), "{}", r.status);
    }
    assert_eq!(ha.saved.lock().as_ref().unwrap().0.bots, Some(vec![]));
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant/bots",
        Some(json!({"bots": null})),
    )
    .await;
    assert_eq!(r.json()["bots"], Value::Null);

    let r = call(&t, Method::DELETE, "/v1/homeassistant", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&t, Method::DELETE, "/v1/homeassistant", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant/bots",
        Some(json!({"bots": null})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_bad_token_is_a_400_and_a_bad_hub_a_422() {
    let (t, _) = with_ha();
    for bad in ["", "a b", "a\nb", &"x".repeat(4097)] {
        let r = call(
            &t,
            Method::PUT,
            "/v1/homeassistant",
            Some(json!({"url": "http://192.168.1.10:8123", "token": bad})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "save {bad:?}");
        let r = call(
            &t,
            Method::POST,
            "/v1/homeassistant/test",
            Some(json!({"url": "http://192.168.1.10:8123", "token": bad})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "test {bad:?}");
    }
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant",
        Some(json!({"url": "https://ha.example.com", "token": SECRET})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "bad_homeassistant_config");
    // The first save needs a token.
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant",
        Some(json!({"url": "http://192.168.1.10:8123"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let long: Vec<String> = (0..65).map(|i| format!("cover.s{i}")).collect();
    let r = call(
        &t,
        Method::PUT,
        "/v1/homeassistant",
        Some(json!({"url": "http://192.168.1.10:8123", "token": SECRET, "trusted_services": long})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_connection_test_counts_entities_and_a_refused_token_is_a_200() {
    let (t, _) = with_ha();
    let r = call(&t, Method::POST, "/v1/homeassistant/test", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "nothing saved yet");

    let r = call(
        &t,
        Method::POST,
        "/v1/homeassistant/test",
        Some(json!({"url": "http://192.168.1.10:8123", "token": SECRET})),
    )
    .await;
    let text = String::from_utf8_lossy(&r.body).into_owned();
    assert_eq!(r.status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET));
    let v: Value = r.json();
    assert_eq!(
        (v["ok"].as_bool(), v["entities"].as_u64()),
        (Some(true), Some(3))
    );

    let r = call(
        &t,
        Method::POST,
        "/v1/homeassistant/test",
        Some(json!({"url": "http://192.168.1.10:8123", "token": "wrong"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["ok"], false);

    // An address without a token (the stored one must not go to it) is refused.
    let r = call(
        &t,
        Method::POST,
        "/v1/homeassistant/test",
        Some(json!({"url": "http://192.168.1.10:8123"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn changes_are_audited_without_the_address_or_the_token() {
    let (t, _) = with_ha();
    call(&t, Method::PUT, "/v1/homeassistant", Some(body())).await;
    call(
        &t,
        Method::PUT,
        "/v1/homeassistant/bots",
        Some(json!({"bots": ["b1"]})),
    )
    .await;
    call(&t, Method::DELETE, "/v1/homeassistant", None).await;
    let rows: Vec<_> = t
        .state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .into_iter()
        .filter(|a| a.action.starts_with("homeassistant."))
        .collect();
    let actions: Vec<&str> = rows.iter().map(|a| a.action.as_str()).collect();
    for want in [
        "homeassistant.save",
        "homeassistant.bots",
        "homeassistant.remove",
    ] {
        assert!(actions.contains(&want), "{actions:?}");
    }
    let all = serde_json::to_string(&rows.iter().map(|a| &a.detail).collect::<Vec<_>>()).unwrap();
    assert!(!all.contains(SECRET) && !all.contains("192.168"), "{all}");
}
