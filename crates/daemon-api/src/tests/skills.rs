//! Tests for `/v1/skills`, `/v1/bots/{id}/skills`, and peer role guards.

mod hub;
mod management;
mod market;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{PROFILE, T, call, setup_with};
use crate::{AppState, PeerIdentity, PeerRole, SkillAssignments, peer_router};

fn write_skill(dir: &Path, name: &str, desc: &str, body: &str, when: Option<&str>) {
    let root = dir.join(name);
    fs::create_dir_all(&root).unwrap();
    let when_block = when.unwrap_or("");
    fs::write(
        root.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {desc}\nversion: 1.2.3\n{when_block}---\n{body}\n"
        ),
    )
    .unwrap();
}

fn setup_skills(prepare: impl FnOnce(&Path)) -> T {
    let t = setup_with(|dir| {
        fs::create_dir_all(dir.join("skills")).unwrap();
        prepare(dir);
    });
    let assignments = Arc::new(SkillAssignments::new(t.state.storage_arc()));
    let index = ::skills::SkillIndex::load_roots(::skills::SkillRoots::from_data_dir(
        t.state.storage().data_dir(),
    ))
    .with_assignments(Arc::clone(&assignments) as Arc<dyn ::skills::AssignmentSource>);
    t.state.set_skills(index).unwrap();
    t
}

async fn create_bot(t: &T) -> String {
    let r = call(
        t,
        Method::POST,
        "/v1/bots",
        Some(json!({ "profile_toml": PROFILE })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    r.json()["id"].as_str().unwrap().to_owned()
}

fn json_contains_key(val: &Value, needle: &str) -> bool {
    match val {
        Value::Object(map) => {
            map.contains_key(needle) || map.values().any(|v| json_contains_key(v, needle))
        }
        Value::Array(arr) => arr.iter().any(|v| json_contains_key(v, needle)),
        _ => false,
    }
}

#[tokio::test]
async fn catalog_returns_metadata_without_body_and_surfaces_issues_and_hub() {
    let t = setup_skills(|dir| {
        write_skill(
            &dir.join("skills"),
            "alpha-skill",
            "Alpha description",
            "SECRET_BODY_DO_NOT_LEAK",
            Some("when:\n  domains: [example.com]\n  keywords: [alpha]\n"),
        );
        // Broken skill triggers an issue without failing the catalog.
        let broken = dir.join("skills").join("broken-skill");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("SKILL.md"), "not valid frontmatter").unwrap();

        // Install a local skill via SkillHub so lockfile metadata is populated.
        let src = dir.join("hub-src");
        write_skill(&src, "hub-skill", "From hub", "HUB_BODY", None);
        let hub = ::skills::hub::SkillHub::new(::skills::hub::HubPaths::from_data_dir(dir));
        let preview = hub
            .preview(&::skills::hub::InstallSource::Local(src.join("hub-skill")))
            .unwrap();
        hub.install(
            &preview,
            &::skills::hub::InstallOptions {
                accept: Some(preview.report.fingerprint.clone()),
                replace: false,
                trust_local: false,
            },
        )
        .unwrap();
    });

    let resp = call(&t, Method::GET, "/v1/skills", None).await;
    assert_eq!(resp.status, StatusCode::OK);
    let body_text = String::from_utf8_lossy(&resp.body);
    assert!(!body_text.contains("SECRET_BODY_DO_NOT_LEAK"));
    assert!(!body_text.contains("HUB_BODY"));
    let json = resp.json();
    assert!(!json_contains_key(&json, "body"), "{json}");

    let skills = json["skills"].as_array().unwrap();
    let alpha = skills.iter().find(|s| s["name"] == "alpha-skill").unwrap();
    assert_eq!(alpha["scope"], "shared");
    assert_eq!(alpha["description"], "Alpha description");
    assert_eq!(alpha["version"], "1.2.3");
    // The catalog carries a body fingerprint (never the body): a client can tell its file from an older one.
    let digest = {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(b"SECRET_BODY_DO_NOT_LEAK"))
    };
    assert_eq!(alpha["body_sha256"], digest);
    assert_eq!(alpha["requires"]["domains"], json!(["example.com"]));
    assert!(alpha["hub"].is_null());

    let hub_entry = skills.iter().find(|s| s["name"] == "hub-skill").unwrap();
    assert_eq!(hub_entry["scope"], "shared");
    assert_eq!(hub_entry["hub"]["locked"], true);
    assert!(
        hub_entry["hub"]["source"]
            .as_str()
            .unwrap()
            .starts_with("local:")
    );

    let bundled = skills.iter().find(|s| s["scope"] == "bundled").unwrap();
    assert!(bundled["hub"].is_null());

    let issues = json["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["scope"] == "shared" && i["path"].as_str().unwrap().contains("broken-skill")),
        "{issues:?}"
    );
}

async fn assert_put_validation(t: &T, bot_id: &str) {
    let unknown = uuid::Uuid::now_v7();
    let r = call(
        t,
        Method::PUT,
        &format!("/v1/bots/{unknown}/skills"),
        Some(json!({"mode": "selected", "names": ["alpha"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    for bad in ["../escape", "UpperCase", "", "a..b"] {
        let r = call(
            t,
            Method::PUT,
            &format!("/v1/bots/{bot_id}/skills"),
            Some(json!({"mode": "selected", "names": [bad]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "bad={bad}");
    }

    let too_many: Vec<String> = (0..201).map(|i| format!("skill-{i}")).collect();
    let r = call(
        t,
        Method::PUT,
        &format!("/v1/bots/{bot_id}/skills"),
        Some(json!({"mode": "selected", "names": too_many})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

fn assert_audit_trail(t: &T) {
    let audits: Vec<_> = t
        .state
        .storage()
        .list_audit(t.state.user_id())
        .unwrap()
        .into_iter()
        .filter(|a| a.action == "bot.skills_assigned")
        .collect();
    assert_eq!(audits.len(), 3);
    assert_eq!(audits[0].detail["mode"], "selected");
    assert_eq!(audits[0].detail["added"], json!(["alpha", "ghost"]));
    assert_eq!(audits[0].detail["removed"], json!([]));
    assert_eq!(audits[1].detail["added"], json!([]));
    assert_eq!(audits[1].detail["removed"], json!([]));
    assert_eq!(audits[2].detail["mode"], "all");
    assert_eq!(audits[2].detail["removed"], json!(["alpha", "ghost"]));
    let audit_raw = serde_json::to_string(&audits[0].detail).unwrap();
    assert!(!audit_raw.contains("Alpha body"));
}

async fn assert_initial_bot_skills(t: &T, bot_id: &str) {
    let initial = call(t, Method::GET, &format!("/v1/bots/{bot_id}/skills"), None).await;
    assert_eq!(initial.status, StatusCode::OK);
    let init_json = initial.json();
    assert_eq!(init_json["mode"], "all");
    assert_eq!(init_json["assigned"], json!([]));
    assert!(
        init_json["available"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["assigned"] == true)
    );
    assert_eq!(
        init_json["own"],
        json!([{"name": "my-own", "version": "1.2.3"}])
    );
}

#[tokio::test]
async fn put_bot_skills_validates_audits_and_round_trips() {
    let t = setup_skills(|dir| {
        write_skill(
            &dir.join("skills"),
            "alpha",
            "Alpha desc",
            "Alpha body",
            None,
        );
        write_skill(&dir.join("skills"), "beta", "Beta desc", "Beta body", None);
    });
    let bot_id = create_bot(&t).await;

    let learned_dir = t
        .state
        .storage()
        .data_dir()
        .join("bots")
        .join(&bot_id)
        .join("skills");
    write_skill(&learned_dir, "my-own", "Own desc", "Own body", None);

    assert_initial_bot_skills(&t, &bot_id).await;
    assert_put_validation(&t, &bot_id).await;

    // Assign selected: `alpha` (installed) + `ghost` (missing) + duplicate `alpha`.
    let put_resp = call(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot_id}/skills"),
        Some(json!({"mode": "selected", "names": ["alpha", "ghost", "alpha"]})),
    )
    .await;
    assert_eq!(put_resp.status, StatusCode::OK, "{:?}", put_resp.json());
    let put_json = put_resp.json();
    assert_eq!(put_json["mode"], "selected");
    assert_eq!(
        put_json["assigned"],
        json!([
            {"name": "alpha", "state": "ok"},
            {"name": "ghost", "state": "missing"}
        ])
    );
    let avail = put_json["available"].as_array().unwrap();
    assert_eq!(
        avail.iter().find(|s| s["name"] == "alpha").unwrap()["assigned"],
        true
    );
    assert_eq!(
        avail.iter().find(|s| s["name"] == "beta").unwrap()["assigned"],
        false
    );

    // Live SkillIndex::for_bot reflects the assignment without restart.
    let view = t
        .state
        .skills_index()
        .unwrap()
        .for_bot(Some(&bot_id))
        .unwrap();
    assert!(view.get("alpha").is_some());
    assert!(view.get("my-own").is_some());
    assert!(view.get("beta").is_none());

    // Idempotent PUT produces empty added/removed in audit.
    let idem = call(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot_id}/skills"),
        Some(json!({"mode": "selected", "names": ["ghost", "alpha"]})),
    )
    .await;
    assert_eq!(idem.status, StatusCode::OK);

    // Mode = all deletes the DB row and restores full catalog.
    let reset = call(
        &t,
        Method::PUT,
        &format!("/v1/bots/{bot_id}/skills"),
        Some(json!({"mode": "all", "names": []})),
    )
    .await;
    assert_eq!(reset.status, StatusCode::OK);
    assert_eq!(reset.json()["mode"], "all");
    let parsed_id = protocol::BotId(bot_id.parse().unwrap());
    assert_eq!(
        t.state
            .storage()
            .get_bot_skill_assignment(parsed_id)
            .unwrap(),
        None
    );
    let view_after = t
        .state
        .skills_index()
        .unwrap()
        .for_bot(Some(&bot_id))
        .unwrap();
    assert!(view_after.get("beta").is_some());

    assert_audit_trail(&t);
}

async fn peer_status(
    state: AppState,
    role: PeerRole,
    method: Method,
    path: String,
    body: Option<Value>,
) -> StatusCode {
    let mut req = Request::builder()
        .method(method)
        .uri(&path)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    req.extensions_mut().insert(PeerIdentity {
        key: format!("peer-{role:?}"),
        role,
    });
    peer_router(state).oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn peer_router_enforces_roles_on_skill_routes() {
    let t = setup_skills(|dir| {
        write_skill(
            &dir.join("skills"),
            "alpha",
            "Alpha desc",
            "Alpha body",
            None,
        );
    });
    let bot_id = create_bot(&t).await;

    let send_peer = |role: PeerRole, method: Method, path: &str, body: Option<Value>| {
        peer_status(t.state.clone(), role, method, path.to_owned(), body)
    };

    let put_body = || Some(json!({"mode": "selected", "names": ["alpha"]}));

    // Phone: cannot mutate skill assignments (including normalized/encoded paths).
    for path in [
        format!("/v1/bots/{bot_id}/skills"),
        format!("/v1/bots/{bot_id}/skills/"),
        format!("/v1/bots//{bot_id}//skills"),
        format!("/v1/bots/{bot_id}%2Fskills"),
    ] {
        assert_eq!(
            send_peer(PeerRole::Phone, Method::PUT, &path, put_body()).await,
            StatusCode::FORBIDDEN,
            "phone PUT {path}"
        );
    }
    // Phone read-only metadata GETs remain allowed (no bodies).
    assert_eq!(
        send_peer(PeerRole::Phone, Method::GET, "/v1/skills", None).await,
        StatusCode::OK
    );
    assert_eq!(
        send_peer(
            PeerRole::Phone,
            Method::GET,
            &format!("/v1/bots/{bot_id}/skills"),
            None
        )
        .await,
        StatusCode::OK
    );

    // Node & Host: denied on all routes.
    for role in [PeerRole::Node, PeerRole::Host] {
        assert_eq!(
            send_peer(role, Method::GET, "/v1/skills", None).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_peer(
                role,
                Method::GET,
                &format!("/v1/bots/{bot_id}/skills"),
                None
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_peer(
                role,
                Method::PUT,
                &format!("/v1/bots/{bot_id}/skills"),
                put_body()
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    // Paired Desktop: allowed on GET and PUT.
    assert_eq!(
        send_peer(PeerRole::Desktop, Method::GET, "/v1/skills", None).await,
        StatusCode::OK
    );
    assert_eq!(
        send_peer(
            PeerRole::Desktop,
            Method::GET,
            &format!("/v1/bots/{bot_id}/skills"),
            None
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send_peer(
            PeerRole::Desktop,
            Method::PUT,
            &format!("/v1/bots/{bot_id}/skills"),
            put_body()
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn stale_expected_version_is_refused_with_409_and_current_succeeds() {
    let t = setup_skills(|dir| {
        write_skill(&dir.join("skills"), "alpha", "A", "a", None);
        write_skill(&dir.join("skills"), "beta", "B", "b", None);
    });
    let bot_id = create_bot(&t).await;
    let path = format!("/v1/bots/{bot_id}/skills");
    let loaded = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(loaded["version"], 0);

    // Another client saves first.
    let first = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "selected", "names": ["alpha"], "expected_version": 0})),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{:?}", first.json());
    let v1 = first.json()["version"].as_u64().unwrap();
    assert!(v1 > 0);

    // A dialog still holding version 0 must not overwrite it.
    let stale = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "selected", "names": ["beta"], "expected_version": 0})),
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.json());
    let now = call(&t, Method::GET, &path, None).await.json();
    assert_eq!(now["assigned"], json!([{"name": "alpha", "state": "ok"}]));

    let ok = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "selected", "names": ["beta"], "expected_version": v1})),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
}

#[tokio::test]
async fn a_corrupt_assignment_row_still_loads_and_can_be_repaired() {
    let t = setup_skills(|dir| write_skill(&dir.join("skills"), "alpha", "A", "a", None));
    let bot_id = create_bot(&t).await;
    let path = format!("/v1/bots/{bot_id}/skills");
    let put = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "selected", "names": ["alpha"]})),
    )
    .await;
    assert_eq!(put.status, StatusCode::OK);
    rusqlite::Connection::open(t.state.storage().data_dir().join("bot.db"))
        .unwrap()
        .execute(
            "UPDATE bot_skill_assignment SET names_json = 'not-json' WHERE bot_id = ?1",
            [&bot_id],
        )
        .unwrap();

    // Runtime fails closed: the bot sees no shared skill ...
    let view = t
        .state
        .skills_index()
        .unwrap()
        .for_bot(Some(&bot_id))
        .unwrap();
    assert!(view.get("alpha").is_none());
    // ... but the endpoint reports it instead of answering 500.
    let got = call(&t, Method::GET, &path, None).await;
    assert_eq!(got.status, StatusCode::OK, "{:?}", got.json());
    let body = got.json();
    assert_eq!(body["mode"], "selected");
    assert_eq!(body["assigned"], json!([]));
    let version = body["version"].clone();
    let fixed = call(
        &t,
        Method::PUT,
        &path,
        Some(json!({"mode": "all", "names": [], "expected_version": version})),
    )
    .await;
    assert_eq!(fixed.status, StatusCode::OK, "{:?}", fixed.json());
    let view = t
        .state
        .skills_index()
        .unwrap()
        .for_bot(Some(&bot_id))
        .unwrap();
    assert!(view.get("alpha").is_some());
}

#[tokio::test]
async fn a_write_by_another_storage_handle_invalidates_cached_views() {
    let t = setup_skills(|dir| write_skill(&dir.join("skills"), "alpha", "A", "a", None));
    let bot_id = create_bot(&t).await;
    let index = t.state.skills_index().unwrap();
    assert!(index.for_bot(Some(&bot_id)).unwrap().get("alpha").is_some());
    // Not through the API: a second connection (as a state-bundle import or a
    // restore would). No bump exists anywhere for this write.
    let other = storage::Storage::open(t.state.storage().data_dir()).unwrap();
    other
        .set_bot_skill_assignment(
            protocol::BotId(bot_id.parse().unwrap()),
            storage::SkillAssignmentMode::Selected,
            &[],
        )
        .unwrap();
    assert!(index.for_bot(Some(&bot_id)).unwrap().get("alpha").is_none());
}

#[test]
fn storage_and_skills_name_validators_agree() {
    let long = "a".repeat(64);
    let too_long = "a".repeat(65);
    let cases = [
        "a",
        "alpha-skill",
        "a_b",
        "A",
        "UPPER",
        "with space",
        "",
        "..",
        "a/b",
        r"a\b",
        "é",
        "con",
        "-lead",
        "trail-",
        "a.b",
        "ok-1",
        &long,
        &too_long,
        "a\0b",
        "a\nb",
    ];
    for name in cases {
        assert_eq!(
            storage::is_valid_skill_name(name),
            ::skills::validate_name(name).is_ok(),
            "validators disagree on {name:?}"
        );
    }
}

#[test]
fn hub_source_tags_never_carry_credentials() {
    use crate::skills::redact_source_tag as r;
    assert_eq!(
        r("git:https://user:tok@github.com/o/r.git?token=x#frag"),
        "git:https://github.com/o/r.git"
    );
    assert_eq!(r("git:https://tok@host/o/r"), "git:https://host/o/r");
    assert_eq!(r("git:https://host?x=1"), "git:https://host");
    assert_eq!(r("local:/some/path"), "local:/some/path");
}

#[tokio::test]
async fn review95_skills_writes_invalidate_profile_revision_without_changing_profile_or_audit_contract()
 {
    let t = setup_skills(|_| {});
    let bid = create_bot(&t).await;
    let path = format!("/v1/bots/{bid}");
    let before = call(&t, Method::GET, &path, None).await.json();
    let assignment = format!("{path}/skills");
    for (body, expected) in [
        (
            json!({"mode":"selected","names":[],"expected_version":0}),
            2,
        ),
        (json!({"mode":"all"}), 3),
    ] {
        let saved = call(&t, Method::PUT, &assignment, Some(body)).await;
        assert_eq!(saved.status, StatusCode::OK, "{:?}", saved.json());
        let current = call(&t, Method::GET, &path, None).await.json();
        assert_eq!(current["version"], expected);
        assert_eq!(current["config"], before["config"]);
        let stale = call(
            &t,
            Method::PATCH,
            &path,
            Some(json!({"expected_version":1,"name":"stale"})),
        )
        .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
    }
    let audits = t.state.storage().list_audit(t.state.user_id()).unwrap();
    assert_eq!(
        audits
            .iter()
            .filter(|a| a.action == "bot.skills_assigned")
            .count(),
        2
    );
}
