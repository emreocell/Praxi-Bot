//! Regression tests for the security review of the `OpenAPI` tools: redirects,
//! untrusted `servers` and `$ref`s, URL / header injection, the money rule,
//! expansion limits and the query-string API key.

use std::fmt::Write as _;
use std::sync::Arc;

use openapi_tools::{ApiSpec, Auth, ImportOptions, OpenApiConfig, OpenApiError, import_tools};
use secret_store::{MemoryStore, SecretRef, SecretStore};
use serde_json::{Value, json};
use tool_registry::{ContentPart, Effect, PermissionCategory, Tool, ToolContext, ToolError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "sk-live-9f8e7d6c5b4a/+=&";

fn store() -> Arc<MemoryStore> {
    let s = Arc::new(MemoryStore::new());
    s.set("api-key", KEY).unwrap();
    s
}

fn cfg(auth: Auth) -> OpenApiConfig {
    let mut c = OpenApiConfig::new(store());
    c.allow_private_hosts = true; // wiremock listens on loopback
    c.auth = auth;
    c
}

fn key() -> SecretRef {
    SecretRef::new("api-key").unwrap()
}

fn opts(base: &str) -> ImportOptions {
    ImportOptions {
        base_url: Some(base.into()),
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

fn spec_with(paths: &str) -> String {
    format!("openapi: 3.0.0\ninfo: {{title: T, version: '1'}}\npaths:\n{paths}")
}

// 1. Redirects ---------------------------------------------------------------

#[tokio::test]
async fn a_redirect_never_carries_the_credential_to_another_host_or_anywhere() {
    let api = MockServer::start().await;
    let evil = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/a"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", format!("{}/steal", evil.uri())),
        )
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/b"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/api/after"))
        .mount(&api)
        .await;
    let spec = spec_with(
        "  /a: {get: {operationId: a, responses: {}}}\n  /b: {get: {operationId: b, responses: {}}}\n",
    );
    for auth in [
        Auth::Bearer { secret: key() },
        Auth::ApiKeyHeader {
            header: "X-Api-Key".into(),
            secret: key(),
        },
        Auth::ApiKeyQuery {
            param: "k".into(),
            secret: key(),
        },
    ] {
        let (_, ts) =
            import_tools(&spec, &opts(&format!("{}/api", api.uri())), &cfg(auth)).unwrap();
        let cross = run(&find(&ts, "a"), json!({})).await.unwrap();
        assert_eq!(cross["status"], 307);
        let same = run(&find(&ts, "b"), json!({})).await.unwrap();
        assert_eq!(same["status"], 302);
    }
    assert!(evil.received_requests().await.unwrap().is_empty());
    // Same-host redirects are not followed either: the target was never asked.
    for req in api.received_requests().await.unwrap() {
        assert_ne!(req.url.path(), "/api/after");
    }
}

// 2. Untrusted `servers` and `$ref`s -------------------------------------------

const SERVERS_SPEC: &str = "openapi: 3.0.0
info: {title: T, version: '1'}
servers: [{url: 'https://api.example.com/v1'}]
paths:
  /a: {get: {operationId: a, responses: {}}}
";

#[test]
fn document_servers_are_used_only_when_on_the_host_it_came_from() {
    let parse = |o: &ImportOptions| ApiSpec::parse(SERVERS_SPEC, o);
    // Pasted text, no owner URL: nothing to trust.
    assert!(matches!(
        parse(&ImportOptions::default()),
        Err(OpenApiError::NoServer(_))
    ));
    // Downloaded from another host: the document may not redirect the API.
    let elsewhere = ImportOptions {
        server_host: Some("docs.example.org".into()),
        ..ImportOptions::default()
    };
    assert!(matches!(parse(&elsewhere), Err(OpenApiError::NoServer(_))));
    // The metadata address is no better.
    let metadata = SERVERS_SPEC.replace("https://api.example.com/v1", "http://169.254.169.254/");
    assert!(
        ApiSpec::parse(
            &metadata,
            &ImportOptions {
                server_host: Some("api.example.com".into()),
                ..ImportOptions::default()
            }
        )
        .is_err()
    );
    // Same host: used.
    let same = ImportOptions {
        server_host: Some("API.example.com".into()),
        ..ImportOptions::default()
    };
    assert_eq!(parse(&same).unwrap().base_url, "https://api.example.com/v1");
    // The owner's URL always wins and `servers` are not even looked at.
    let owner = ImportOptions {
        base_url: Some("https://mine.example.net".into()),
        server_host: Some("docs.example.org".into()),
        ..ImportOptions::default()
    };
    assert_eq!(parse(&owner).unwrap().base_url, "https://mine.example.net");
}

#[tokio::test]
async fn remote_refs_are_never_fetched_and_the_final_base_url_is_still_guarded() {
    let spec = "openapi: 3.0.0
info: {title: T, version: '1'}
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema: {$ref: 'http://169.254.169.254/latest/meta-data/#/x'}
      responses: {}
";
    let api = ApiSpec::parse(spec, &opts("https://api.example.com")).unwrap();
    assert!(api.warnings.iter().any(|w| w.contains("not followed")));
    // A private final base URL is refused when the tools are built.
    let guarded = OpenApiConfig::new(store());
    let err = import_tools(spec, &opts("http://169.254.169.254"), &guarded)
        .err()
        .unwrap();
    assert!(matches!(err, OpenApiError::PrivateHost(_)));
}

// 3. URL and header injection ----------------------------------------------------

const INJECT: &str = "openapi: 3.0.0
info: {title: T, version: '1'}
paths:
  /items/{id}:
    get:
      operationId: item
      parameters:
        - {name: id, in: path, required: true, schema: {type: string}}
        - {name: q, in: query, schema: {type: string}}
        - {name: X-Trace, in: header, schema: {type: string}}
      responses: {}
  /../admin:
    get: {operationId: up, responses: {}}
  /x/./y:
    get: {operationId: dot, responses: {}}
  /search?admin=1:
    get: {operationId: sneaky, responses: {}}
  /a%2e%2e/b:
    get: {operationId: encoded_ok, responses: {}}
";

#[tokio::test]
async fn path_query_and_header_values_cannot_inject() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let (api, ts) = import_tools(
        INJECT,
        &opts(&format!("{}/api/v1", server.uri())),
        &cfg(Auth::None),
    )
    .unwrap();
    let skipped: Vec<&str> = api.skipped.iter().map(|s| s.path.as_str()).collect();
    for p in ["/../admin", "/x/./y", "/search?admin=1"] {
        assert!(skipped.contains(&p), "{p} should be left out: {skipped:?}");
    }
    let item = find(&ts, "item");
    for id in [
        "../../admin",
        "..%2f..%2fadmin",
        "a/b",
        "x?admin=1",
        "x#frag",
        "x\\y",
        "%2e%2e",
        "x\r\nHost: evil",
    ] {
        let _ = run(&item, json!({"id": id})).await;
    }
    assert!(!server.received_requests().await.unwrap().is_empty());
    let _ = run(&item, json!({"id": "ok", "q": "a&admin=1#x"}))
        .await
        .unwrap();
    for req in server.received_requests().await.unwrap() {
        let u = &req.url;
        assert!(u.path().starts_with("/api/v1/items/"), "{u}");
        assert_eq!(u.path().matches('/').count(), 4, "one segment only: {u}");
        assert_eq!(u.fragment(), None);
        // The only query ever sent is the model's own, encoded.
        assert!(
            u.query().is_none() || u.query() == Some("q=a%26admin%3D1%23x"),
            "{u}"
        );
        assert!(
            req.headers
                .get("host")
                .is_some_and(|h| !h.to_str().unwrap().contains("evil"))
        );
    }
    // CR/LF in a header value never reaches the wire.
    let err = run(&item, json!({"id": "1", "X-Trace": "a\r\nX-Evil: 1"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(m) if m.contains("X-Trace")));
    let err = run(&item, json!({"id": "1", "X-Trace": "a\nb"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

// 4. The money rule ---------------------------------------------------------------

#[test]
fn money_hides_in_tags_text_and_generic_endpoints() {
    let spec = spec_with(
        "  /v1/things:
    post: {operationId: make, tags: [Payments], summary: Make a thing, responses: {}}
    get: {operationId: list, tags: [Things], summary: List things, responses: {}}
  /v1/stuff:
    post: {operationId: stuff, summary: Sends money to the customer, responses: {}}
  /graphql:
    post: {operationId: gql, summary: Run a query, responses: {}}
  /rpc:
    post: {operationId: rpc, responses: {}}
  /v1/charges:
    post: {operationId: benign, responses: {}}
",
    );
    let (_, ts) = import_tools(&spec, &opts("https://api.example.com"), &cfg(Auth::None)).unwrap();
    let ask = |n: &str| find(&ts, n).always_ask();
    for n in ["make", "stuff", "gql", "rpc", "benign"] {
        assert!(ask(n), "{n}");
        assert_eq!(
            find(&ts, n).spec().category,
            PermissionCategory::Money,
            "{n}"
        );
        assert_eq!(find(&ts, n).spec().effect, Effect::Write, "{n}");
    }
    assert!(!ask("list"));
}

// 5. Expansion limits ---------------------------------------------------------------

fn schema_chain(depth: usize) -> String {
    let mut s = String::from(
        "openapi: 3.0.0\ninfo: {title: T, version: '1'}\npaths:\n  /a:\n    post:\n      operationId: a\n      requestBody:\n        content:\n          application/json:\n            schema: {$ref: '#/components/schemas/S0'}\n      responses: {}\n  /ok:\n    get: {operationId: ok, responses: {}}\ncomponents:\n  schemas:\n",
    );
    for i in 0..depth {
        writeln!(
            s,
            "    S{i}: {{type: object, properties: {{n: {{$ref: '#/components/schemas/S{}'}}}}}}",
            i + 1
        )
        .unwrap();
    }
    writeln!(s, "    S{depth}: {{type: string}}").unwrap();
    s
}

#[test]
fn ref_nesting_is_bounded_and_the_rest_of_the_document_still_imports() {
    let o = opts("https://api.example.com");
    let shallow = ApiSpec::parse(&schema_chain(8), &o).unwrap();
    assert_eq!(shallow.operations.len(), 2);
    let deep = ApiSpec::parse(&schema_chain(40), &o).unwrap();
    assert_eq!(deep.operations.len(), 1);
    assert_eq!(deep.operations[0].name, "ok");
    assert!(
        deep.skipped[0].reason.contains("deeper than 10"),
        "{:?}",
        deep.skipped
    );
    let relaxed = ImportOptions {
        max_ref_depth: 60,
        ..o
    };
    assert_eq!(
        ApiSpec::parse(&schema_chain(40), &relaxed)
            .unwrap()
            .operations
            .len(),
        2
    );
}

#[test]
fn circular_refs_are_cut_or_refused_never_followed_forever() {
    let spec = "openapi: 3.0.0
info: {title: T, version: '1'}
paths:
  /tree:
    post:
      operationId: tree
      requestBody:
        content: {application/json: {schema: {$ref: '#/components/schemas/Node'}}}
      responses: {}
  /loop:
    post:
      operationId: looped
      parameters: [{$ref: '#/components/parameters/A'}]
      responses: {}
components:
  parameters:
    A: {$ref: '#/components/parameters/B'}
    B: {$ref: '#/components/parameters/A'}
  schemas:
    Node:
      type: object
      properties:
        child: {$ref: '#/components/schemas/Node'}
";
    let api = ApiSpec::parse(spec, &opts("https://api.example.com")).unwrap();
    // A recursive model is imported, cut where it repeats and reported.
    assert!(api.operations.iter().any(|o| o.name == "tree"));
    assert!(api.warnings.iter().any(|w| w.contains("recursive")));
    // A reference that only points at itself is refused.
    assert!(api.skipped.iter().any(|s| s.path == "/loop"));
    assert!(api.warnings.iter().any(|w| w.contains("circular")));
}

#[test]
fn an_operation_whose_expansion_is_too_big_is_left_out() {
    // Every level references the next one twice: 2^12 copies once inlined.
    let mut s = String::from(
        "openapi: 3.0.0\ninfo: {title: T, version: '1'}\npaths:\n  /big:\n    post:\n      operationId: big\n      requestBody:\n        content: {application/json: {schema: {$ref: '#/components/schemas/L0'}}}\n      responses: {}\n  /ok:\n    get: {operationId: ok, responses: {}}\ncomponents:\n  schemas:\n",
    );
    for i in 0..9 {
        writeln!(s, "    L{i}: {{type: object, properties: {{a: {{$ref: '#/components/schemas/L{n}'}}, b: {{$ref: '#/components/schemas/L{n}'}}, c: {{$ref: '#/components/schemas/L{n}'}}, d: {{$ref: '#/components/schemas/L{n}'}}}}}}",
            n = i + 1
        )
        .unwrap();
    }
    s.push_str("    L9: {type: string, description: leaf}\n");
    let tight = ImportOptions {
        max_ref_depth: 20,
        max_expanded_bytes: 20_000,
        ..opts("https://api.example.com")
    };
    let api = ApiSpec::parse(&s, &tight).unwrap();
    assert_eq!(
        api.operations
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["ok"]
    );
    assert!(
        api.skipped[0].reason.contains("more than 20000 bytes"),
        "{:?}",
        api.skipped
    );
}

#[test]
fn a_yaml_alias_bomb_is_refused_by_the_parser() {
    let mut y = String::from(
        "openapi: 3.0.0\ninfo: {title: t, version: '1'}\nx: &a0 [lol, lol, lol, lol, lol, lol, lol, lol, lol]\n",
    );
    for i in 1..9 {
        let p = i - 1;
        writeln!(y, "x{i}: &a{i} [{}]", vec![format!("*a{p}"); 9].join(",")).unwrap();
    }
    y.push_str("paths: {}\n");
    assert!(matches!(
        ApiSpec::parse(&y, &opts("https://a.test")),
        Err(OpenApiError::Parse(_))
    ));
}

// 6. The key in the query string ----------------------------------------------------

#[tokio::test]
async fn a_query_string_key_is_masked_in_output_errors_and_egress() {
    let server = MockServer::start().await;
    // The API echoes the full request URL, raw and percent-encoded, in the
    // body and in a header.
    Mock::given(method("GET"))
        .and(path("/echo"))
        .respond_with(|req: &wiremock::Request| {
            let url = req.url.to_string();
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("x-seen", url.as_str())
                .set_body_json(json!({"url": url, "raw": KEY}))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/moved"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("/echo?k={KEY}")),
        )
        .mount(&server)
        .await;
    let spec = spec_with(
        "  /echo: {get: {operationId: echo, responses: {}}}\n  /moved: {get: {operationId: moved, responses: {}}}\n  /dead: {get: {operationId: dead, responses: {}}}\n",
    );
    let auth = Auth::ApiKeyQuery {
        param: "k".into(),
        secret: key(),
    };
    let (_, ts) = import_tools(&spec, &opts(&server.uri()), &cfg(auth.clone())).unwrap();
    let encoded = utf8(KEY);

    let out = run(&find(&ts, "echo"), json!({}))
        .await
        .unwrap()
        .to_string();
    assert!(!out.contains(KEY) && !out.contains(&encoded), "{out}");
    assert!(out.contains("k="), "{out}");
    let out = run(&find(&ts, "moved"), json!({}))
        .await
        .unwrap()
        .to_string();
    assert!(!out.contains(KEY) && !out.contains(&encoded), "{out}");

    // Egress shows the host only, never a URL with the key.
    let egress = find(&ts, "echo").egress_host(&json!({})).unwrap();
    assert!(!egress.contains("k=") && !egress.contains('?'), "{egress}");

    // A failing connection reports no URL either.
    let dead = ImportOptions {
        base_url: Some("http://127.0.0.1:1".into()),
        ..ImportOptions::default()
    };
    let (_, ts) = import_tools(&spec, &dead, &cfg(auth)).unwrap();
    let err = run(&find(&ts, "dead"), json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        !err.contains(KEY) && !err.contains(&encoded) && !err.contains("k="),
        "{err}"
    );
}

fn utf8(s: &str) -> String {
    s.replace('/', "%2F")
        .replace('+', "%2B")
        .replace('=', "%3D")
        .replace('&', "%26")
}
