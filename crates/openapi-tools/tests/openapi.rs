//! Import and execution tests against a petstore-like spec and wiremock.

use std::sync::Arc;
use std::time::Duration;

use openapi_tools::{
    ApiSpec, Auth, HttpMethod, ImportOptions, OpenApiConfig, OpenApiError, effect_for,
    import_tools, register, tools,
};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Effect, Tool, ToolContext, ToolError, ToolRegistry};
use wiremock::matchers::{body_json, body_string, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PETSTORE: &str = include_str!("fixtures/petstore.yaml");
const KEY: &str = "sk-live-0123456789abcdef";

fn store() -> Arc<MemoryStore> {
    let s = Arc::new(MemoryStore::new());
    s.set("petstore.key", KEY).unwrap();
    s
}

fn cfg(auth: Auth) -> OpenApiConfig {
    let mut c = OpenApiConfig::new(store());
    c.auth = auth;
    c.allow_private_hosts = true;
    c
}

fn secret(name: &str) -> SecretRef {
    SecretRef::new(name).unwrap()
}

fn bearer() -> Auth {
    Auth::Bearer {
        secret: secret("petstore.key"),
    }
}

fn opts(server: &MockServer) -> ImportOptions {
    ImportOptions {
        base_url: Some(server.uri()),
        ..ImportOptions::default()
    }
}

fn find(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools
        .iter()
        .find(|t| t.spec().name == name)
        .unwrap_or_else(|| panic!("tool {name} missing"))
        .clone()
}

async fn run(t: &Arc<dyn Tool>, input: Value) -> Result<Value, ToolError> {
    let out = t.call(&ToolContext::default(), input).await?;
    match &out.content[0] {
        ContentPart::Json { data } => Ok(data.clone()),
        other => panic!("unexpected part {other:?}"),
    }
}

fn parse(o: &ImportOptions) -> ApiSpec {
    ApiSpec::parse(PETSTORE, o).unwrap()
}

#[test]
fn imports_operations_with_names_and_effects() {
    let api = parse(&ImportOptions::default());
    assert_eq!(api.title, "Petstore");
    assert_eq!(api.base_url, "http://placeholder.invalid/v1");
    let names: Vec<_> = api.operations.iter().map(|o| o.name.as_str()).collect();
    assert!(names.contains(&"listPets"));
    assert!(names.contains(&"createPet"));
    assert!(
        names.contains(&"put_pets_petId"),
        "fallback name: {names:?}"
    );
    assert!(names.contains(&"get_stores_id_orders"));
    // deprecated + multipart are not imported
    assert!(!names.contains(&"oldThing"));
    assert!(!names.contains(&"uploadFile"));
    assert!(
        api.skipped
            .iter()
            .any(|s| s.path == "/upload" && s.reason.contains("multipart"))
    );
    assert_eq!(api.security_schemes[0].kind, "apiKey in header X-Api-Key");

    assert_eq!(effect_for(HttpMethod::Get), Effect::Read);
    assert_eq!(effect_for(HttpMethod::Head), Effect::Read);
    assert_eq!(effect_for(HttpMethod::Post), Effect::Write);
    assert_eq!(effect_for(HttpMethod::Put), Effect::Write);
    assert_eq!(effect_for(HttpMethod::Delete), Effect::Destructive);
}

#[test]
fn tool_specs_have_effects_schema_and_no_refs() {
    let api = parse(&ImportOptions {
        name_prefix: "pets".into(),
        ..ImportOptions::default()
    });
    // placeholder.invalid is not a private host, so the SSRF guard stays on.
    let mut c = cfg(Auth::None);
    c.allow_private_hosts = false;
    let ts = tools(&api, &c).unwrap();
    let del = find(&ts, "pets_deletePet");
    assert_eq!(del.spec().effect, Effect::Destructive);
    assert!(del.spec().is_valid());
    assert_eq!(find(&ts, "pets_listPets").spec().effect, Effect::Read);
    assert_eq!(find(&ts, "pets_createPet").spec().effect, Effect::Write);

    let put = find(&ts, "pets_put_pets_petId");
    let schema = &put.spec().input_schema;
    assert_eq!(schema["required"], json!(["petId", "body"]));
    assert_eq!(schema["properties"]["petId"]["description"], "The pet id");
    // $ref resolved, example stripped, property named `example` preserved,
    // recursion cut, nullable converted.
    let body = &schema["properties"]["body"];
    assert!(!schema.to_string().contains("$ref"));
    assert!(body["properties"]["id"].get("example").is_none());
    assert_eq!(body["properties"]["example"]["type"], "string");
    let cat = &body["properties"]["category"];
    assert_eq!(cat["properties"]["name"]["type"], json!(["string", "null"]));
    assert_eq!(cat["properties"]["parent"]["type"], "object");

    let list = find(&ts, "pets_listPets");
    let props = &list.spec().input_schema["properties"];
    assert_eq!(props["limit"]["maximum"], 100);
    assert_eq!(props["limit"]["description"], "How many items to return");
    assert!(props.get("X-Trace").is_some());

    // colliding path/query `id` are disambiguated
    let orders = find(&ts, "pets_get_stores_id_orders");
    let p = &orders.spec().input_schema["properties"];
    assert!(p.get("path_id").is_some() && p.get("query_id").is_some());
}

#[test]
fn json_and_3_1_are_accepted() {
    let json_spec = r#"{"openapi":"3.1.0","info":{"title":"T","version":"1"},
        "servers":[{"url":"https://{host}/api","variables":{"host":{"default":"example.com"}}}],
        "paths":{"/a":{"get":{"summary":"A","responses":{"200":{"description":"ok"}}}}}}"#;
    let api = ApiSpec::parse(json_spec, &ImportOptions::default()).unwrap();
    assert_eq!(api.base_url, "https://example.com/api");
    assert_eq!(api.operations[0].name, "get_a");
    assert!(api.operations[0].description.contains("[GET /a]"));
}

#[test]
fn rejects_bad_specs() {
    let o = ImportOptions::default();
    assert!(matches!(
        ApiSpec::parse("swagger: '2.0'", &o),
        Err(OpenApiError::UnsupportedVersion(_))
    ));
    assert!(matches!(
        ApiSpec::parse("{ not json", &o),
        Err(OpenApiError::Parse(_))
    ));
    let rel = "openapi: 3.0.0\ninfo: {title: t, version: '1'}\nservers: [{url: /v1}]\npaths: {}";
    assert!(matches!(
        ApiSpec::parse(rel, &o),
        Err(OpenApiError::NoServer(_))
    ));
    let small = ImportOptions {
        max_spec_bytes: 10,
        ..ImportOptions::default()
    };
    assert!(matches!(
        ApiSpec::parse(PETSTORE, &small),
        Err(OpenApiError::SpecTooLarge { .. })
    ));
}

#[test]
fn filters_and_limits() {
    let names = |f: &dyn Fn(&mut ImportOptions)| {
        let mut o = ImportOptions::default();
        f(&mut o);
        parse(&o)
            .operations
            .iter()
            .map(|op| op.name.clone())
            .collect::<Vec<_>>()
    };
    let store_only = names(&|o| o.include_tags = vec!["store".into()]);
    assert_eq!(store_only, vec!["get_stores_id_orders"]);
    let no_admin = names(&|o| o.exclude_tags = vec!["admin".into(), "store".into()]);
    assert!(!no_admin.contains(&"deletePet".to_string()));
    let ids = names(&|o| o.include_operation_ids = vec!["listPets".into(), "deletePet".into()]);
    assert_eq!(ids.len(), 2);
    let ro = names(&|o| o.read_only = true);
    assert!(ro.iter().all(|n| n != "createPet" && n != "deletePet"));
    let dep = names(&|o| o.include_deprecated = true);
    assert!(dep.contains(&"oldThing".to_string()));

    let capped = ImportOptions {
        max_operations: 2,
        ..ImportOptions::default()
    };
    assert!(matches!(
        ApiSpec::parse(PETSTORE, &capped),
        Err(OpenApiError::TooManyOperations { limit: 2, .. })
    ));
    let counts = parse(&ImportOptions::default()).tag_counts();
    assert_eq!(counts["pets"], 5);
}

#[test]
fn duplicate_operation_ids_get_unique_names() {
    let spec = "openapi: 3.0.0
info: {title: t, version: '1'}
servers: [{url: 'https://x.test'}]
paths:
  /a: {get: {operationId: do-it, responses: {}}}
  /b: {get: {operationId: do_it, responses: {}}}
";
    let api = ApiSpec::parse(spec, &ImportOptions::default()).unwrap();
    let mut names: Vec<_> = api.operations.iter().map(|o| o.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["do_it", "do_it_2"]);
}

#[test]
fn private_hosts_are_blocked_by_default() {
    let c = OpenApiConfig::new(store());
    for url in [
        "http://169.254.169.254/latest",
        "http://localhost:8080",
        "http://[::1]/x",
        "http://10.0.0.5",
        "http://[::ffff:127.0.0.1]",
    ] {
        let s = ApiSpec::parse(
            PETSTORE,
            &ImportOptions {
                base_url: Some(url.into()),
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert!(
            matches!(tools(&s, &c), Err(OpenApiError::PrivateHost(_))),
            "{url}"
        );
    }
}

#[test]
fn invalid_secret_name_is_a_config_error() {
    let spec = parse(&ImportOptions::default());
    let c = cfg(Auth::Bearer {
        secret: serde_json::from_value(json!("__reserved")).unwrap(),
    });
    assert!(matches!(tools(&spec, &c), Err(OpenApiError::Config(_))));
}

#[test]
fn registers_into_registry() {
    let spec = parse(&ImportOptions::default());
    let reg = ToolRegistry::new();
    let n = register(&reg, &spec, &cfg(Auth::None)).unwrap();
    assert_eq!(n, spec.operations.len());
    assert_eq!(register(&reg, &spec, &cfg(Auth::None)).unwrap(), 0);
    assert!(reg.get("listPets").is_some());
}

#[tokio::test]
async fn get_with_query_header_and_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets"))
        .and(query_param("limit", "5"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(header("x-trace", "t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": 1}])))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(bearer())).unwrap();
    let out = run(&find(&ts, "listPets"), json!({"limit": 5, "X-Trace": "t1"}))
        .await
        .unwrap();
    assert_eq!(out["status"], 200);
    assert_eq!(out["ok"], true);
    assert_eq!(out["body"], json!([{"id": 1}]));
}

#[tokio::test]
async fn path_param_is_encoded_and_cannot_escape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets/a%2F..%2Fb%20c"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 2})))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(Auth::None)).unwrap();
    let out = run(&find(&ts, "showPetById"), json!({"petId": "a/../b c"}))
        .await
        .unwrap();
    assert_eq!(out["status"], 200);
}

#[tokio::test]
async fn array_query_is_exploded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets"))
        .and(query_param("status", "a"))
        .and(query_param("status", "b"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(Auth::None)).unwrap();
    let out = run(&find(&ts, "listPets"), json!({"status": ["a", "b"]}))
        .await
        .unwrap();
    assert_eq!(out["body"], "ok");
}

#[tokio::test]
async fn post_json_body_and_non_2xx_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/pets"))
        .and(body_json(json!({"name": "Rex"})))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({"error": "bad"})))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(Auth::None)).unwrap();
    let out = run(&find(&ts, "createPet"), json!({"body": {"name": "Rex"}}))
        .await
        .unwrap();
    assert_eq!(out["status"], 422);
    assert_eq!(out["ok"], false);
    assert_eq!(out["body"]["error"], "bad");
}

#[tokio::test]
async fn delete_with_basic_auth() {
    let server = MockServer::start().await;
    // base64("user:sk-live-0123456789abcdef")
    Mock::given(method("DELETE"))
        .and(path("/pets/9"))
        .and(header(
            "authorization",
            "Basic dXNlcjpzay1saXZlLTAxMjM0NTY3ODlhYmNkZWY=",
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(
        PETSTORE,
        &opts(&server),
        &cfg(Auth::Basic {
            username: "user".into(),
            password_secret: secret("petstore.key"),
        }),
    )
    .unwrap();
    let out = run(&find(&ts, "deletePet"), json!({"petId": "9"}))
        .await
        .unwrap();
    assert_eq!(out["status"], 204);
    assert_eq!(out["body"], Value::Null);
}

#[tokio::test]
async fn api_key_header_hidden_from_schema_and_not_overridable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(
        PETSTORE,
        &opts(&server),
        &cfg(Auth::ApiKeyHeader {
            header: "X-Api-Key".into(),
            secret: secret("petstore.key"),
        }),
    )
    .unwrap();
    let t = find(&ts, "listPets");
    assert!(
        t.spec().input_schema["properties"]
            .get("X-Api-Key")
            .is_none()
    );
    let err = run(&t, json!({"X-Api-Key": "attacker"})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
    run(&t, json!({})).await.unwrap();
}

#[tokio::test]
async fn api_key_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets/1"))
        .and(query_param("api_key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(
        PETSTORE,
        &opts(&server),
        &cfg(Auth::ApiKeyQuery {
            param: "api_key".into(),
            secret: secret("petstore.key"),
        }),
    )
    .unwrap();
    run(&find(&ts, "showPetById"), json!({"petId": "1"}))
        .await
        .unwrap();
}

#[tokio::test]
async fn secrets_are_redacted_from_responses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets/1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"echo": format!("Bearer {KEY}")})),
        )
        .mount(&server)
        .await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(bearer())).unwrap();
    let out = run(&find(&ts, "showPetById"), json!({"petId": "1"}))
        .await
        .unwrap();
    assert!(!out.to_string().contains("0123456789abcdef"), "{out}");
}

#[tokio::test]
async fn missing_secret_is_a_clean_error() {
    let server = MockServer::start().await;
    let (_, ts) = import_tools(
        PETSTORE,
        &opts(&server),
        &cfg(Auth::Bearer {
            secret: secret("nope"),
        }),
    )
    .unwrap();
    let err = run(&find(&ts, "listPets"), json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::Execution(m) if m.contains("nope")));
}

#[tokio::test]
async fn input_validation() {
    let server = MockServer::start().await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(Auth::None)).unwrap();
    let show = find(&ts, "showPetById");
    assert!(matches!(
        run(&show, json!({})).await,
        Err(ToolError::InvalidInput(m)) if m.contains("petId")
    ));
    assert!(matches!(
        run(&show, json!({"petId": "1", "bogus": 1})).await,
        Err(ToolError::InvalidInput(m)) if m.contains("bogus")
    ));
    assert!(matches!(
        run(&find(&ts, "createPet"), json!({})).await,
        Err(ToolError::InvalidInput(m)) if m.contains("body")
    ));
    assert!(matches!(
        run(&show, json!([1])).await,
        Err(ToolError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn response_is_capped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("x".repeat(10_000)),
        )
        .mount(&server)
        .await;
    let mut c = cfg(Auth::None);
    c.max_response_bytes = 100;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &c).unwrap();
    let out = run(&find(&ts, "listPets"), json!({})).await.unwrap();
    assert_eq!(out["truncated"], true);
    assert_eq!(out["body"].as_str().unwrap().len(), 100);
}

#[tokio::test]
async fn timeout_is_enforced() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let mut c = cfg(Auth::None);
    c.timeout = Duration::from_millis(200);
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &c).unwrap();
    let err = run(&find(&ts, "listPets"), json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::Execution(m) if m.contains("timed out")));
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "http://evil.test/"))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &cfg(bearer())).unwrap();
    let out = run(&find(&ts, "listPets"), json!({})).await.unwrap();
    assert_eq!(out["status"], 302);
    assert_eq!(out["location"], "http://evil.test/");
}

#[tokio::test]
async fn form_body_is_url_encoded() {
    let spec = "openapi: 3.0.0
info: {title: t, version: '1'}
servers: [{url: 'https://x.test'}]
paths:
  /login:
    post:
      operationId: login
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema: {type: object, properties: {user: {type: string}}}
      responses: {}
";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string("user=a+b"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .expect(1)
        .mount(&server)
        .await;
    let (_, ts) = import_tools(spec, &opts(&server), &cfg(Auth::None)).unwrap();
    let out = run(&find(&ts, "login"), json!({"body": {"user": "a b"}}))
        .await
        .unwrap();
    assert_eq!(out["status"], 200);
}

const MINI: &str = r#"
openapi: 3.0.3
info: {title: Mini, version: "1"}
paths:
  /v1/users/{id}/profile:
    get:
      operationId: profile
      parameters:
        - {name: id, in: path, required: true, schema: {type: string}}
      responses: {"200": {description: ok}}
  /search:
    get:
      operationId: search
      parameters:
        - name: filter
          in: query
          schema: {type: object, additionalProperties: {type: string}}
      responses: {"200": {description: ok}}
"#;

fn mini_tools(server: &MockServer, auth: Auth, base_suffix: &str) -> Vec<Arc<dyn Tool>> {
    let o = ImportOptions {
        base_url: Some(format!("{}{base_suffix}", server.uri())),
        ..ImportOptions::default()
    };
    import_tools(MINI, &o, &cfg(auth)).unwrap().1
}

#[tokio::test]
async fn dot_segment_path_params_are_rejected() {
    let server = MockServer::start().await;
    let ts = mini_tools(&server, Auth::None, "/api");
    let t = find(&ts, "profile");
    for bad in ["..", ".", "%2e%2E", "%2E", " .. "] {
        let err = run(&t, json!({"id": bad})).await.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidInput(_)),
            "{bad:?}: {err:?}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    // Dots inside a longer value stay legal and literal.
    Mock::given(method("GET"))
        .and(path("/api/v1/users/a..b/profile"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    assert_eq!(run(&t, json!({"id": "a..b"})).await.unwrap()["status"], 200);
}

#[tokio::test]
async fn truncated_multibyte_text_is_still_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pets"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("ğğğ", "text/plain"))
        .mount(&server)
        .await;
    let mut c = cfg(Auth::None);
    c.max_response_bytes = 5;
    let (_, ts) = import_tools(PETSTORE, &opts(&server), &c).unwrap();
    let out = run(&find(&ts, "listPets"), json!({})).await.unwrap();
    assert_eq!(out["truncated"], true);
    assert_eq!(out["body"], "ğğ");
}

#[tokio::test]
async fn api_key_query_cannot_be_shadowed_by_exploded_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("api_key", KEY))
        .and(query_param("q", "x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;
    let auth = Auth::ApiKeyQuery {
        param: "api_key".into(),
        secret: secret("petstore.key"),
    };
    let ts = mini_tools(&server, auth, "");
    let out = run(
        &find(&ts, "search"),
        json!({"filter": {"api_key": "attacker", "q": "x"}}),
    )
    .await
    .unwrap();
    assert_eq!(out["status"], 200);
    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs[0].url.as_str().contains("attacker"));
}

#[test]
fn tool_keys_are_capped_and_unique() {
    let long = "a".repeat(100);
    let spec = format!(
        r#"
openapi: 3.0.3
info: {{title: K, version: "1"}}
servers: [{{url: "http://example.com"}}]
paths:
  /x/{{id}}:
    get:
      operationId: k
      parameters:
        - {{name: id, in: path, required: true, schema: {{type: string}}}}
        - {{name: id, in: query, schema: {{type: string}}}}
        - {{name: path_id, in: query, schema: {{type: string}}}}
        - {{name: {long}, in: query, schema: {{type: string}}}}
        - {{name: {long}, in: header, schema: {{type: string}}}}
      responses: {{"200": {{description: ok}}}}
"#
    );
    let api = ApiSpec::parse(&spec, &ImportOptions::default()).unwrap();
    let keys: Vec<&str> = api.operations[0]
        .params
        .iter()
        .map(|p| p.key.as_str())
        .collect();
    let uniq: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(uniq.len(), keys.len(), "{keys:?}");
    assert!(keys.iter().all(|k| k.len() <= 64), "{keys:?}");
    assert!(
        keys.contains(&"path_id"),
        "real param keeps its key: {keys:?}"
    );
}
