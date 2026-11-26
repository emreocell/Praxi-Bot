//! `/v1/calendar/*` against an in-memory `CalendarControl`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::{T, TOKEN, call, send, setup};
use crate::calendar::{
    CalendarAccountDto, CalendarAccountInput, CalendarControl, CalendarControlError,
    CalendarFoundDto, CalendarTestInput, CalendarTestReport,
};

const SECRET: &str = "app-pw-writeonly-xyz";

#[derive(Default)]
struct FakeCalendar {
    accounts: Mutex<Vec<(CalendarAccountDto, String)>>,
}

impl FakeCalendar {
    fn new() -> Arc<Self> {
        Arc::default()
    }
}

#[async_trait]
impl CalendarControl for FakeCalendar {
    fn list(&self) -> Vec<CalendarAccountDto> {
        self.accounts
            .lock()
            .iter()
            .map(|(a, _)| a.clone())
            .collect()
    }

    async fn create(
        &self,
        input: CalendarAccountInput,
    ) -> Result<CalendarAccountDto, CalendarControlError> {
        let mut all = self.accounts.lock();
        if all.iter().any(|(a, _)| a.name == input.name) {
            return Err(CalendarControlError::Conflict("name taken".into()));
        }
        if !input.url.starts_with("https://") {
            return Err(CalendarControlError::Invalid(
                "the url must be https".into(),
            ));
        }
        let dto = CalendarAccountDto {
            name: input.name,
            url: input.url,
            username: input.username,
            email: input.email.filter(|e| !e.is_empty()),
            password_set: true,
        };
        all.push((dto.clone(), input.password.unwrap_or_default()));
        Ok(dto)
    }

    async fn update(
        &self,
        name: &str,
        input: CalendarAccountInput,
    ) -> Result<CalendarAccountDto, CalendarControlError> {
        let mut all = self.accounts.lock();
        let (dto, pw) = all
            .iter_mut()
            .find(|(a, _)| a.name == name)
            .ok_or_else(|| CalendarControlError::NotFound(format!("calendar {name}")))?;
        dto.url = input.url;
        dto.username = input.username;
        if let Some(p) = input.password {
            *pw = p;
        }
        Ok(dto.clone())
    }

    async fn delete(&self, name: &str) -> Result<(), CalendarControlError> {
        let mut all = self.accounts.lock();
        let before = all.len();
        all.retain(|(a, _)| a.name != name);
        if all.len() == before {
            return Err(CalendarControlError::NotFound(format!("calendar {name}")));
        }
        Ok(())
    }

    async fn test(
        &self,
        input: CalendarTestInput,
    ) -> Result<CalendarTestReport, CalendarControlError> {
        if input.name.is_none() && input.url.is_none() {
            return Err(CalendarControlError::Invalid("nothing to test".into()));
        }
        let wrong = input.password.as_deref() == Some("wrong");
        Ok(CalendarTestReport {
            ok: !wrong,
            calendars: if wrong {
                Vec::new()
            } else {
                vec![CalendarFoundDto {
                    name: "Personal".into(),
                    url: "https://dav.example.com/cal/personal/".into(),
                }]
            },
            scheduling: false,
            error: wrong.then(|| "The server refused the login.".into()),
        })
    }
}

fn with_calendar() -> (T, Arc<FakeCalendar>) {
    let t = setup();
    let cal = FakeCalendar::new();
    t.state
        .set_calendar(Arc::clone(&cal) as Arc<dyn CalendarControl>);
    (t, cal)
}

fn account(name: &str) -> Value {
    json!({"name": name, "url": "https://dav.example.com/cal/", "username": "me@example.com", "password": SECRET})
}

#[tokio::test]
async fn calendar_routes_need_bearer_token_and_good_origin() {
    let (t, _) = with_calendar();
    for (m, p, b) in [
        (Method::GET, "/v1/calendar/accounts", None),
        (Method::POST, "/v1/calendar/accounts", Some(account("home"))),
        (
            Method::PUT,
            "/v1/calendar/accounts/home",
            Some(account("home")),
        ),
        (Method::DELETE, "/v1/calendar/accounts/home", None),
        (
            Method::POST,
            "/v1/calendar/test",
            Some(json!({"name": "home"})),
        ),
    ] {
        let r = send(&t.app, m.clone(), p, b.clone(), &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        let auth = format!("Bearer {TOKEN}");
        let r = send(
            &t.app,
            m.clone(),
            p,
            b.clone(),
            &[("origin", "https://evil.com"), ("authorization", &auth)],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {p}");
    }
}

#[tokio::test]
async fn without_calendar_control_routes_return_503() {
    let t = setup();
    for (m, p, b) in [
        (Method::GET, "/v1/calendar/accounts", None),
        (Method::POST, "/v1/calendar/accounts", Some(account("home"))),
        (
            Method::PUT,
            "/v1/calendar/accounts/home",
            Some(account("home")),
        ),
        (Method::DELETE, "/v1/calendar/accounts/home", None),
        (
            Method::POST,
            "/v1/calendar/test",
            Some(json!({"name": "home"})),
        ),
    ] {
        let r = call(&t, m, p, b).await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{p}");
    }
}

#[tokio::test]
async fn accounts_are_added_listed_changed_and_removed_and_the_password_is_never_echoed() {
    let (t, cal) = with_calendar();
    let r = call(
        &t,
        Method::POST,
        "/v1/calendar/accounts",
        Some(account("home")),
    )
    .await;
    let text = String::from_utf8_lossy(&r.body).into_owned();
    assert_eq!(r.status, StatusCode::CREATED, "{text}");
    assert!(!text.contains(SECRET), "password leaked: {text}");
    let v: Value = r.json();
    assert_eq!(v["name"], "home");
    assert_eq!(v["password_set"], true);
    assert_eq!(cal.accounts.lock()[0].1, SECRET);

    let g = call(&t, Method::GET, "/v1/calendar/accounts", None).await;
    assert_eq!(g.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&g.body).contains(SECRET));
    assert_eq!(g.json().as_array().unwrap().len(), 1);

    // A second account with the same name is a conflict.
    let r = call(
        &t,
        Method::POST,
        "/v1/calendar/accounts",
        Some(account("home")),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // Changing it without a password keeps the stored one.
    let r = call(
        &t,
        Method::PUT,
        "/v1/calendar/accounts/home",
        Some(json!({"url": "https://other.example.com/cal/", "username": "me2"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["username"], "me2");
    assert_eq!(cal.accounts.lock()[0].1, SECRET);

    let r = call(
        &t,
        Method::PUT,
        "/v1/calendar/accounts/nope",
        Some(account("nope")),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&t, Method::DELETE, "/v1/calendar/accounts/home", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&t, Method::DELETE, "/v1/calendar/accounts/home", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(cal.accounts.lock().is_empty());
}

#[tokio::test]
async fn the_password_is_checked_and_a_bad_config_is_a_422() {
    let (t, _) = with_calendar();
    let mut no_pw = account("home");
    no_pw.as_object_mut().unwrap().remove("password");
    let r = call(&t, Method::POST, "/v1/calendar/accounts", Some(no_pw)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    for bad in ["", "a\nb", &"x".repeat(1025)] {
        let mut a = account("home");
        a["password"] = json!(bad);
        let r = call(&t, Method::POST, "/v1/calendar/accounts", Some(a.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "create {bad:?}");
        let r = call(
            &t,
            Method::POST,
            "/v1/calendar/test",
            Some(json!({"url": "https://x.example.com/", "password": bad})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "test {bad:?}");
    }
    let mut a = account("home");
    a["url"] = json!("http://dav.example.com/cal/");
    let r = call(&t, Method::POST, "/v1/calendar/accounts", Some(a)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["error"]["code"], "bad_calendar_config");
}

#[tokio::test]
async fn the_connection_test_lists_calendars_and_reports_a_refused_login_as_200() {
    let (t, _) = with_calendar();
    let r = call(
        &t,
        Method::POST,
        "/v1/calendar/test",
        Some(json!({"url": "https://dav.example.com/", "username": "me", "password": SECRET})),
    )
    .await;
    let text = String::from_utf8_lossy(&r.body).into_owned();
    assert_eq!(r.status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET));
    let v: Value = r.json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["calendars"][0]["name"], "Personal");

    let r = call(
        &t,
        Method::POST,
        "/v1/calendar/test",
        Some(json!({"url": "https://dav.example.com/", "username": "me", "password": "wrong"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let v: Value = r.json();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"], "The server refused the login.");

    let r = call(&t, Method::POST, "/v1/calendar/test", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}
