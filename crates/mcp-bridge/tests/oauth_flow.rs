//! OAuth sign-in against a fake authorization server on loopback: discovery,
//! PKCE code exchange, `resource`, storage and redaction. No network.
#[path = "support/oauth_fixture.rs"]
mod fixture;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use fixture::*;
use mcp_bridge::oauth::{SecretRegistrar, SignIn, SignInOptions, credential_name};
use mcp_bridge::{Backoff, McpError, McpManager, McpPolicy, McpServerConfig, ServerStatus};
use parking_lot::Mutex;
use secret_store::SecretStore;

#[tokio::test(flavor = "multi_thread")]
async fn expiry_and_the_sixty_second_window_refresh_before_sending_a_bearer() {
    for (expiry, accepted) in [(1, true), (45, true), (1, false)] {
        let (base, seen, server) = fake_authorization_server(true).await;
        seen.lock().expires_in = Some(expiry);
        seen.lock().refresh_allowed = accepted;
        let store = Arc::new(MemoryStore::default());
        let manager = McpManager::with_policy(
            tool_registry::ToolRegistry::new(),
            McpPolicy {
                quarantine_after: 1,
                ..McpPolicy::default()
            },
        )
        .with_secret_store(store);
        assert!(matches!(
            manager
                .add_server(http_config("expiry", format!("{base}/mcp")))
                .await,
            Err(McpError::AuthRequired(_))
        ));
        finish_sign_in(
            manager.start_sign_in("expiry", options()).await.unwrap(),
            &base,
            &seen,
        )
        .await;
        if expiry == 1 {
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        // This fixture deliberately still rejects MCP; the refresh must happen
        // first, and authentication failures must never quarantine it.
        assert!(matches!(
            manager.reenable("expiry").await,
            Err(McpError::AuthRequired(_))
        ));
        let requests = seen.lock().token_requests.clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["grant_type"], "refresh_token");
        assert_eq!(requests[1]["resource"], format!("{base}/mcp"));
        let bearers = seen.lock().mcp_authorization.clone();
        if accepted {
            assert_eq!(
                bearers.last().cloned().flatten(),
                Some(format!("Bearer {REFRESHED}"))
            );
        } else {
            assert!(bearers.iter().all(Option::is_none));
        }
        let snapshot = manager.snapshot("expiry").unwrap();
        assert!(snapshot.auth_required);
        assert_eq!(snapshot.consecutive_failures, 0);
        assert_eq!(snapshot.status, ServerStatus::Disconnected);
        manager.shutdown().await;
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_unauthorized_request_reconnects_and_retries_only_once_without_failures() {
    for accepted in [true, false] {
        let (base, seen, server) = fake_authorization_server(true).await;
        seen.lock().mcp_bearer = Some(ACCESS.into());
        seen.lock().refresh_allowed = true;
        let registry = tool_registry::ToolRegistry::new();
        let manager = McpManager::with_policy(
            registry.clone(),
            McpPolicy {
                quarantine_after: 1,
                ..McpPolicy::default()
            },
        )
        .with_secret_store(Arc::new(MemoryStore::default()));
        assert!(matches!(
            manager
                .add_server(http_config("live", format!("{base}/mcp")))
                .await,
            Err(McpError::AuthRequired(_))
        ));
        finish_sign_in(
            manager.start_sign_in("live", options()).await.unwrap(),
            &base,
            &seen,
        )
        .await;
        assert_eq!(manager.reenable("live").await.unwrap(), 1);
        let echo = registry.get("mcp:live/echo").unwrap();
        let context = tool_registry::ToolContext::default();
        echo.call(&context, serde_json::json!({"text": "before"}))
            .await
            .unwrap();
        seen.lock().tool_calls.clear();
        seen.lock().mcp_bearer = Some(REFRESHED.into());
        if !accepted {
            // Reconnect succeeds, but the retried tool request is still denied.
            seen.lock().reject_tool_calls = true;
        }
        let call = echo
            .call(&context, serde_json::json!({"text": "after"}))
            .await;
        let snapshot = manager.snapshot("live").unwrap();
        assert_eq!(snapshot.consecutive_failures, 0, "{snapshot:?}");
        assert_ne!(snapshot.status, ServerStatus::Quarantined);
        let requests = seen.lock().token_requests.clone();
        assert_eq!(requests.len(), 2, "one refresh per failed request");
        assert_eq!(
            seen.lock().tool_calls,
            vec![
                Some(format!("Bearer {ACCESS}")),
                Some(format!("Bearer {REFRESHED}"))
            ]
        );
        if accepted {
            assert!(call.is_ok(), "{call:?}");
            assert_eq!(snapshot.status, ServerStatus::Connected);
        } else {
            assert!(call.unwrap_err().to_string().contains("authorization"));
            assert!(snapshot.auth_required);
        }
        manager.shutdown().await;
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_discovers_exchanges_with_pkce_and_stores_only_in_the_secret_store() {
    let (base, seen, server) = fake_authorization_server(true).await;
    seen.lock().mcp_bearer = Some(ACCESS.into());
    let store = Arc::new(MemoryStore::default());
    let registered = Arc::new(Mutex::new(Vec::<String>::new()));
    let registrar: SecretRegistrar = {
        let registered = Arc::clone(&registered);
        Arc::new(move |value: &str| registered.lock().push(value.to_owned()))
    };
    let registry = tool_registry::ToolRegistry::new();
    let manager = McpManager::new(registry.clone())
        .with_secret_store(store.clone())
        .with_secret_registrar(registrar);
    assert!(matches!(
        manager
            .add_server(http_config("e2e", format!("{base}/mcp")))
            .await,
        Err(McpError::AuthRequired(_))
    ));
    assert!(registry.get("mcp:e2e/echo").is_none());
    let sign_in = manager.start_sign_in("e2e", options()).await.unwrap();
    let url = sign_in.authorization_url().to_owned();
    let params = pairs(url.split_once('?').unwrap().1);
    assert!(url.starts_with(&format!("{base}/authorize?")), "{url}");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["client_id"], "praxi-test-client");
    assert_eq!(params["resource"], format!("{base}/mcp"));
    assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert_ne!(params["state"], "");
    seen.lock().challenge = Some(params["code_challenge"].clone());
    assert!(!format!("{sign_in:?}").contains(&params["state"]));

    let finishing = tokio::spawn(sign_in.finish(Duration::from_secs(20)));
    let page = redirect(
        &url,
        &format!("code=code-1&state={}&iss={base}", params["state"]),
    )
    .await
    .unwrap();
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    finishing.await.unwrap().unwrap();

    // The verifier matched the challenge (the fake answers 400 otherwise) and
    // the token request named the resource.
    let token_requests = seen.lock().token_requests.clone();
    assert_eq!(token_requests.len(), 1);
    assert_eq!(token_requests[0]["resource"], format!("{base}/mcp"));
    assert!(token_requests[0]["code_verifier"].len() >= 43);

    // Tokens: in the secret store under the server's name, registered for
    // redaction, bound to the server URL.
    assert_eq!(store.list_names().unwrap(), vec![credential_name("e2e")]);
    let stored = store.get("mcp/e2e/oauth").unwrap().unwrap();
    assert!(stored.expose().contains(ACCESS));
    assert!(stored.expose().contains(REFRESH));
    assert!(stored.expose().contains(&format!("{base}/mcp")));
    let registered = registered.lock().clone();
    assert!(registered.contains(&ACCESS.to_owned()), "{registered:?}");
    assert!(registered.contains(&REFRESH.to_owned()), "{registered:?}");
    assert_eq!(manager.reenable("e2e").await.unwrap(), 1);
    let echo = registry.get("mcp:e2e/echo").unwrap();
    // Signing in grants no permission to the server's tools.
    assert_eq!(echo.spec().effect, tool_registry::Effect::Destructive);
    assert_eq!(
        echo.spec().category,
        tool_registry::PermissionCategory::Accounts
    );
    let result = echo
        .call(
            &tool_registry::ToolContext::default(),
            serde_json::json!({"text": "signed in"}),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["content"][0]["text"],
        "signed in"
    );
    assert_eq!(
        manager.snapshot("e2e").unwrap().status,
        ServerStatus::Connected
    );
    assert!(
        seen.lock()
            .mcp_authorization
            .iter()
            .any(|v| v.as_deref() == Some(&format!("Bearer {ACCESS}")))
    );
    // A replayed redirect has nowhere to go.
    assert!(
        redirect(&url, &format!("code=code-1&state={}", params["state"]))
            .await
            .is_err()
    );
    manager.shutdown().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_state_or_a_refused_code_stores_nothing_and_leaks_nothing() {
    let (base, seen, server) = fake_authorization_server(true).await;
    for query in ["code=code-1&state=forged", "code=stolen-code&state={state}"] {
        let store = Arc::new(MemoryStore::default());
        let sign_in = SignIn::start(
            "bad",
            &format!("{base}/mcp"),
            store.clone(),
            None,
            options(),
        )
        .await
        .unwrap();
        let url = sign_in.authorization_url().to_owned();
        let params = pairs(url.split_once('?').unwrap().1);
        seen.lock().challenge = Some(params["code_challenge"].clone());
        let finishing = tokio::spawn(sign_in.finish(Duration::from_secs(20)));
        let query = query.replace("{state}", &params["state"]);
        let _ = redirect(&url, &format!("{query}&iss={base}")).await;
        let error = finishing.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("OAuth sign-in failed"), "{error}");
        assert!(!error.contains("stolen-code"), "{error}");
        assert!(!error.contains(&params["state"]), "{error}");
        assert_eq!(store.list_names().unwrap(), Vec::<String>::new(), "{query}");
    }
    server.abort();
}

/// The browser runs on the owner's computer: the daemon opens no listener, sends
/// the redirect to the owner's own loopback port, and finishes only from the
/// redirect the desktop forwards (same `state` and address checks as a paste).
#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_sign_in_finishes_from_the_forwarded_redirect_only() {
    let (base, seen, server) = fake_authorization_server(true).await;
    let port: u16 = 53172;
    for (forwarded, ok) in [
        (
            "http://127.0.0.1:53172/callback?code=code-1&state={state}",
            true,
        ),
        (
            "http://127.0.0.1:53172/callback?code=code-1&state=forged",
            false,
        ),
        (
            "http://127.0.0.1:53173/callback?code=code-1&state={state}",
            false,
        ),
        (
            "http://evil.example:53172/callback?code=code-1&state={state}",
            false,
        ),
        (
            "http://127.0.0.1:53172/callback?error=access_denied&state={state}",
            false,
        ),
    ] {
        let store = Arc::new(MemoryStore::default());
        let mut opts = options();
        opts.redirect_port = Some(port);
        opts.scopes = vec!["https://www.googleapis.com/auth/gmail.readonly".into()];
        opts.offline_access = true;
        let sign_in = SignIn::start("relay", &format!("{base}/mcp"), store.clone(), None, opts)
            .await
            .unwrap();
        assert!(sign_in.is_relayed());
        assert_eq!(
            sign_in.redirect_uri(),
            format!("http://127.0.0.1:{port}/callback")
        );
        let url = sign_in.authorization_url().to_owned();
        let params = pairs(url.split_once('?').unwrap().1);
        assert_eq!(
            params["redirect_uri"],
            format!("http://127.0.0.1:{port}/callback")
        );
        assert_eq!(
            params["scope"],
            "https://www.googleapis.com/auth/gmail.readonly"
        );
        assert_eq!(params["access_type"], "offline");
        assert_eq!(params["prompt"], "consent");
        seen.lock().challenge = Some(params["code_challenge"].clone());
        let (send, receive) = tokio::sync::oneshot::channel();
        let finishing =
            tokio::spawn(sign_in.finish_or_pasted(Duration::from_secs(20), Some(receive)));
        send.send(forwarded.replace("{state}", &params["state"]) + &format!("&iss={base}"))
            .unwrap();
        let result = finishing.await.unwrap();
        assert_eq!(result.is_ok(), ok, "{forwarded}: {result:?}");
        if ok {
            assert_eq!(store.list_names().unwrap(), vec![credential_name("relay")]);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(!error.contains("code-1"), "{error}");
            assert_eq!(
                store.list_names().unwrap(),
                Vec::<String>::new(),
                "{forwarded}"
            );
        }
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_sign_in_needs_a_forwarded_redirect_and_a_safe_port() {
    let (base, _seen, server) = fake_authorization_server(true).await;
    let store = Arc::new(MemoryStore::default());
    let mut opts = options();
    opts.redirect_port = Some(80);
    let refused = SignIn::start("relay", &format!("{base}/mcp"), store.clone(), None, opts)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("port"), "{refused}");
    let mut opts = options();
    opts.redirect_port = Some(53174);
    let sign_in = SignIn::start("relay", &format!("{base}/mcp"), store, None, opts)
        .await
        .unwrap();
    // Without a forwarding channel nothing could ever finish it.
    let error = sign_in
        .finish(Duration::from_secs(1))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("forwarded"), "{error}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authorization_server_without_pkce_is_refused_before_any_browser_step() {
    let (base, seen, server) = fake_authorization_server(false).await;
    let store = Arc::new(MemoryStore::default());
    let error = SignIn::start(
        "nopkce",
        &format!("{base}/mcp"),
        store.clone(),
        None,
        options(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("S256"), "{error}");
    assert_eq!(seen.lock().token_requests.len(), 0);
    assert_eq!(store.list_names().unwrap(), Vec::<String>::new());

    // Loopback http needs the explicit development opt-in: without it the
    // policy client refuses discovery itself, before any request is sent.
    let (base, strict_seen, strict) = fake_authorization_server(true).await;
    let mut opts = options();
    opts.allow_loopback_http = false;
    assert!(
        SignIn::start("strict", &format!("{base}/mcp"), store, None, opts)
            .await
            .is_err()
    );
    assert_eq!(strict_seen.lock().paths, Vec::<String>::new());
    server.abort();
    strict.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_is_sent_only_to_its_own_server_and_waiting_for_one_is_not_a_failure() {
    let (a, seen_a, server_a) = fake_authorization_server(true).await;
    let (b, seen_b, server_b) = fake_authorization_server(true).await;
    let store = Arc::new(MemoryStore::default());
    let registered = Arc::new(Mutex::new(Vec::<String>::new()));
    let registrar: SecretRegistrar = {
        let registered = Arc::clone(&registered);
        Arc::new(move |value: &str| registered.lock().push(value.to_owned()))
    };
    let manager = McpManager::with_policy(
        tool_registry::ToolRegistry::new(),
        McpPolicy {
            // One counted failure would quarantine a server.
            quarantine_after: 1,
            backoff: Backoff {
                initial: Duration::from_secs(30),
                max: Duration::from_secs(30),
                max_attempts: 3,
            },
            connect_timeout: Duration::from_secs(20),
            call_timeout: Duration::from_secs(20),
        },
    )
    .with_secret_store(store.clone())
    .with_secret_registrar(registrar);

    // Both servers answer 401: they wait for the user, unquarantined.
    for (id, base) in [("a", &a), ("b", &b)] {
        let error = manager
            .add_server(http_config(id, format!("{base}/mcp")))
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::AuthRequired(_)), "{error}");
        let snapshot = manager.snapshot(id).unwrap();
        assert!(snapshot.auth_required, "{snapshot:?}");
        assert_eq!(snapshot.consecutive_failures, 0, "{snapshot:?}");
        assert_eq!(snapshot.status, ServerStatus::Disconnected, "{snapshot:?}");
    }
    assert_eq!(seen_a.lock().mcp_authorization, vec![None]);

    // The user signs in to A (and only to A).
    let sign_in = manager.start_sign_in("a", options()).await.unwrap();
    let url = sign_in.authorization_url().to_owned();
    let params = pairs(url.split_once('?').unwrap().1);
    seen_a.lock().challenge = Some(params["code_challenge"].clone());
    let finishing = tokio::spawn(sign_in.finish(Duration::from_secs(20)));
    redirect(
        &url,
        &format!("code=code-1&state={}&iss={a}", params["state"]),
    )
    .await
    .unwrap();
    finishing.await.unwrap().unwrap();
    assert_eq!(store.list_names().unwrap(), vec![credential_name("a")]);

    // Reconnect both. On the wire: A gets its bearer, B gets nothing.
    let after_a = manager.reenable("a").await.unwrap_err().to_string();
    let after_b = manager.reenable("b").await.unwrap_err().to_string();
    assert_eq!(
        seen_a.lock().mcp_authorization.last().cloned().flatten(),
        Some(format!("Bearer {ACCESS}"))
    );
    let to_b = seen_b.lock().mcp_authorization.clone();
    assert!(to_b.len() >= 2, "{to_b:?}");
    assert!(to_b.iter().all(Option::is_none), "{to_b:?}");
    for path in &seen_b.lock().paths {
        assert!(!path.contains(ACCESS), "{path}");
    }
    // Nothing the manager reports carries a token.
    let reported = format!(
        "{after_a} {after_b} {:?} {:?}",
        manager.snapshots(),
        manager.stderr_tail("a")
    );
    assert!(!reported.contains(ACCESS), "{reported}");
    assert!(!reported.contains(REFRESH), "{reported}");
    assert!(registered.lock().contains(&ACCESS.to_owned()));
    assert_eq!(manager.snapshot("a").unwrap().consecutive_failures, 0);

    // The same server name pointed at B's URL must not carry A's token there.
    manager.remove_server("a").await.unwrap();
    let _ = manager
        .add_server(http_config("a", format!("{b}/mcp")))
        .await;
    let to_b = seen_b.lock().mcp_authorization.clone();
    assert!(to_b.iter().all(Option::is_none), "{to_b:?}");
    assert_eq!(store.list_names().unwrap(), Vec::<String>::new());

    // Signing out removes the secret.
    assert!(!manager.sign_out("a").await.unwrap().had_credentials);
    manager.shutdown().await;
    server_a.abort();
    server_b.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn confidential_refresh_reads_the_secret_reference_on_every_connection() {
    const CLIENT_SECRET: &str = "confidential-client-sentinel-98fd";
    const ROTATED_SECRET: &str = "rotated-client-sentinel-62be";
    let (base, seen, server) = fake_authorization_server(true).await;
    seen.lock().client_secret = Some(CLIENT_SECRET.into());
    seen.lock().expires_in = Some(45);
    seen.lock().refresh_allowed = true;
    seen.lock().mcp_bearer = Some(REFRESHED.into());
    let store = Arc::new(MemoryStore::default());
    store.set("mcp/client-secret", CLIENT_SECRET).unwrap();
    let registry = tool_registry::ToolRegistry::new();
    let registered = Arc::new(Mutex::new(Vec::new()));
    let registrar: SecretRegistrar = {
        let registered = registered.clone();
        Arc::new(move |value| registered.lock().push(value.to_owned()))
    };
    let manager = McpManager::new(registry.clone())
        .with_secret_store(store.clone())
        .with_secret_registrar(registrar);
    let mut config = http_config("confidential", format!("{base}/mcp"));
    config.oauth.as_mut().unwrap().client_secret_ref = Some("mcp/client-secret".into());
    assert!(!toml::to_string(&config).unwrap().contains(CLIENT_SECRET));
    assert!(!format!("{config:?}").contains(CLIENT_SECRET));
    assert!(matches!(
        manager.add_server(config).await,
        Err(McpError::AuthRequired(_))
    ));
    finish_sign_in(
        manager
            .start_sign_in("confidential", SignInOptions::default())
            .await
            .unwrap(),
        &base,
        &seen,
    )
    .await;
    store.set("mcp/client-secret", ROTATED_SECRET).unwrap();
    seen.lock().client_secret = Some(ROTATED_SECRET.into());
    assert_eq!(manager.reenable("confidential").await.unwrap(), 1);
    let requests = seen.lock().token_requests.clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].get("client_secret").map(String::as_str),
        Some(CLIENT_SECRET)
    );
    assert_eq!(
        requests[1].get("client_secret").map(String::as_str),
        Some(ROTATED_SECRET)
    );
    assert_eq!(requests[1]["grant_type"], "refresh_token");
    assert!(registered.lock().iter().any(|v| v == CLIENT_SECRET));
    assert!(registered.lock().iter().any(|v| v == ROTATED_SECRET));
    assert!(registry.get("mcp:confidential/echo").is_some());
    manager.shutdown().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn metadata_document_is_preferred_to_a_preregistered_client_when_supported() {
    let (base, seen, server) = fake_authorization_server(true).await;
    seen.lock().cimd_supported = true;
    let mut opts = options();
    opts.client_metadata_url = Some("https://client.example/metadata.json".into());
    let sign_in = SignIn::start(
        "cimd",
        &format!("{base}/mcp"),
        Arc::new(MemoryStore::default()),
        None,
        opts,
    )
    .await
    .unwrap();
    let params = pairs(sign_in.authorization_url().split_once('?').unwrap().1);
    assert_eq!(params["client_id"], "https://client.example/metadata.json");
    finish_sign_in(sign_in, &base, &seen).await;
    server.abort();
}

/// Review finding 3: a refresh that is waiting for its answer when the user
/// signs out must not store the refreshed (rotated) credentials afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn sign_out_during_a_blocked_refresh_leaves_no_credentials() {
    let (base, seen, server) = fake_authorization_server(true).await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    {
        let mut seen = seen.lock();
        seen.expires_in = Some(30); // inside the 60 s window: refresh on connect
        seen.refresh_allowed = true;
        seen.rotate_refresh = true;
        seen.mcp_bearer = Some(REFRESHED.into());
        seen.refresh_gate = Some(Arc::clone(&gate));
    }
    let store = Arc::new(MemoryStore::default());
    let manager = Arc::new(
        McpManager::with_policy(tool_registry::ToolRegistry::new(), McpPolicy::default())
            .with_secret_store(store.clone()),
    );
    assert!(matches!(
        manager
            .add_server(http_config("race", format!("{base}/mcp")))
            .await,
        Err(McpError::AuthRequired(_))
    ));
    finish_sign_in(
        manager.start_sign_in("race", options()).await.unwrap(),
        &base,
        &seen,
    )
    .await;
    assert!(store.0.lock().contains_key(&credential_name("race")));

    // Connect: the refresh request reaches the token endpoint and waits.
    let connecting = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.reenable("race").await }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while seen.lock().refreshes_received == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Sign out while it waits: the record is gone right away.
    let signing_out = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.sign_out("race").await }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while store.0.lock().contains_key(&credential_name("race")) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Now the rotated tokens arrive.
    gate.add_permits(1);
    assert!(signing_out.await.unwrap().unwrap().had_credentials);
    let connected = connecting.await.unwrap();
    assert!(
        matches!(connected, Err(McpError::AuthRequired(_))),
        "{connected:?}"
    );
    assert!(
        !store.0.lock().contains_key(&credential_name("race")),
        "a refresh re-stored credentials after sign-out"
    );
    let all = format!("{:?}", store.0.lock());
    assert!(!all.contains(ROTATED) && !all.contains(REFRESHED));
    // No session was opened with the refreshed bearer.
    assert!(
        seen.lock()
            .mcp_authorization
            .iter()
            .all(|a| a.as_deref() != Some(&format!("Bearer {REFRESHED}")))
    );
    manager.shutdown().await;
    server.abort();
}

/// Static DNS that records every lookup: proves which client resolved what.
#[derive(Default)]
struct RecordingResolver {
    table: BTreeMap<String, Vec<std::net::IpAddr>>,
    asked: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl mcp_bridge::oauth::EndpointResolver for RecordingResolver {
    async fn lookup(&self, host: &str, _port: u16) -> Option<Vec<std::net::IpAddr>> {
        self.asked.lock().push(host.to_owned());
        self.table.get(host).cloned()
    }
}

/// Review finding 4: discovery, registration and redirect hops go through
/// the policy client and are refused before any request when they name an
/// internal or metadata address, by alias or literal.
#[tokio::test(flavor = "multi_thread")]
async fn discovery_registration_and_redirects_to_internal_hosts_are_refused_before_any_request() {
    type Setup = fn(&mut Seen, u16);
    let cases: [(&str, Setup, Option<&str>); 6] = [
        (
            "discovery alias",
            |s, port| {
                s.authorization_server = Some(format!("http://internal.attacker.test:{port}"));
            },
            Some("internal.attacker.test"),
        ),
        (
            "discovery metadata literal",
            |s, _| s.authorization_server = Some("http://169.254.169.254".into()),
            None,
        ),
        (
            "registration alias",
            |s, port| {
                s.registration_endpoint =
                    Some(format!("http://imds.attacker.test:{port}/register"));
            },
            Some("imds.attacker.test"),
        ),
        (
            // rmcp refuses a cross-origin discovery redirect itself.
            "discovery redirect to alias",
            |s, port| {
                s.discovery_redirect = Some(format!(
                    "http://internal.attacker.test:{port}/.well-known/oauth-protected-resource"
                ));
            },
            None,
        ),
        (
            // Registration follows redirects: every hop is vetted by us.
            "registration redirect to alias",
            |s, port| {
                s.registration_endpoint = Some(format!("http://127.0.0.1:{port}/register"));
                s.registration_redirect =
                    Some(format!("http://internal.attacker.test:{port}/register"));
            },
            Some("internal.attacker.test"),
        ),
        (
            "redirect to metadata literal",
            |s, _| s.discovery_redirect = Some("http://169.254.169.254/latest/meta-data/".into()),
            None,
        ),
    ];
    for (name, setup, alias) in cases {
        let (base, seen, server) = fake_authorization_server(true).await;
        let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
        setup(&mut seen.lock(), port);
        let resolver = Arc::new(RecordingResolver {
            table: BTreeMap::from([
                (
                    "internal.attacker.test".to_owned(),
                    vec!["10.0.0.5".parse().unwrap()],
                ),
                (
                    "imds.attacker.test".to_owned(),
                    vec!["169.254.169.254".parse().unwrap()],
                ),
            ]),
            ..RecordingResolver::default()
        });
        let store = Arc::new(MemoryStore::default());
        let options = SignInOptions {
            // No client id for the registration case: dynamic registration.
            client_id: None,
            allow_loopback_http: true,
            resolver: Some(resolver.clone()),
            ..SignInOptions::default()
        };
        let result =
            SignIn::start("ssrf", &format!("{base}/mcp"), store.clone(), None, options).await;
        assert!(result.is_err(), "{name}: the flow went ahead");
        if let Some(alias) = alias {
            // Our client resolved it (and refused); rmcp's own did not.
            assert!(
                resolver.asked.lock().iter().any(|h| h == alias),
                "{name}: {alias} was not vetted by the policy client"
            );
        }
        assert!(seen.lock().token_requests.is_empty(), "{name}");
        assert_eq!(store.list_names().unwrap(), Vec::<String>::new(), "{name}");
        server.abort();
    }
}

/// Review finding 5: a confidential client bound to issuer A is never shown
/// to issuer B, even after the MCP server starts advertising B.
#[tokio::test(flavor = "multi_thread")]
async fn a_confidential_client_is_never_presented_to_another_issuer() {
    const CLIENT_SECRET: &str = "issuer-bound-secret-sentinel-4a7c";
    let (base_a, seen_a, server_a) = fake_authorization_server(true).await;
    let (base_b, seen_b, server_b) = fake_authorization_server(true).await;
    {
        let mut a = seen_a.lock();
        a.client_secret = Some(CLIENT_SECRET.into());
        a.expires_in = Some(45); // a refresh is due on the next connect
        a.refresh_allowed = true;
    }
    let store = Arc::new(MemoryStore::default());
    store.set("mcp/client-secret", CLIENT_SECRET).unwrap();
    let manager =
        McpManager::new(tool_registry::ToolRegistry::new()).with_secret_store(store.clone());
    let mut config = http_config("bound", format!("{base_a}/mcp"));
    config.oauth.as_mut().unwrap().client_secret_ref = Some("mcp/client-secret".into());
    // Without an issuer the confidential client is not even accepted.
    let mut unbound = config.clone();
    unbound.oauth.as_mut().unwrap().issuer = None;
    assert!(
        unbound
            .validate()
            .unwrap_err()
            .to_string()
            .contains("issuer")
    );
    assert!(matches!(
        manager.add_server(config).await,
        Err(McpError::AuthRequired(_))
    ));
    finish_sign_in(
        manager
            .start_sign_in("bound", SignInOptions::default())
            .await
            .unwrap(),
        &base_a,
        &seen_a,
    )
    .await;
    // The MCP server now names attacker B as its authorization server.
    seen_a.lock().authorization_server = Some(base_b.clone());
    let refreshed = manager.reenable("bound").await;
    assert!(
        matches!(refreshed, Err(McpError::AuthRequired(_))),
        "{refreshed:?}"
    );
    let error = manager
        .start_sign_in("bound", SignInOptions::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("issuer"), "{error}");
    // B never saw a token request, so never the client id or secret.
    assert_eq!(seen_b.lock().token_requests.len(), 0);
    let b_paths = seen_b.lock().paths.clone();
    assert!(
        !b_paths.iter().any(|p| p == "/token" || p == "/register"),
        "{b_paths:?}"
    );
    let snapshot = manager.snapshot("bound").unwrap();
    assert_eq!(snapshot.consecutive_failures, 0);
    manager.shutdown().await;
    server_a.abort();
    server_b.abort();
}

/// Review finding 13: a session that stays up past the refresh window gets
/// a refreshed bearer before its next call, not after a rejected one.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_session_refreshes_before_its_bearer_enters_the_window() {
    let (base, seen, server) = fake_authorization_server(true).await;
    {
        let mut s = seen.lock();
        s.expires_in = Some(64); // outside the 60 s window at connect (4 s margin)
        s.refresh_allowed = true;
        s.mcp_bearer = Some(ACCESS.into());
    }
    let registry = tool_registry::ToolRegistry::new();
    let manager =
        McpManager::new(registry.clone()).with_secret_store(Arc::new(MemoryStore::default()));
    assert!(matches!(
        manager
            .add_server(http_config("live", format!("{base}/mcp")))
            .await,
        Err(McpError::AuthRequired(_))
    ));
    finish_sign_in(
        manager.start_sign_in("live", options()).await.unwrap(),
        &base,
        &seen,
    )
    .await;
    assert_eq!(manager.reenable("live").await.unwrap(), 1);
    assert_eq!(seen.lock().token_requests.len(), 1, "no refresh at connect");
    // Time passes: the token is now inside the window; the server will only
    // accept the refreshed one from here on.
    tokio::time::sleep(Duration::from_millis(4500)).await;
    seen.lock().mcp_bearer = Some(REFRESHED.into());
    seen.lock().tool_calls.clear();
    let echo = registry.get("mcp:live/echo").unwrap();
    echo.call(
        &tool_registry::ToolContext::default(),
        serde_json::json!({"text": "after"}),
    )
    .await
    .unwrap();
    let requests = seen.lock().token_requests.clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["grant_type"], "refresh_token");
    // The only tool call carried the refreshed bearer: no rejected attempt.
    assert_eq!(
        seen.lock().tool_calls,
        vec![Some(format!("Bearer {REFRESHED}"))]
    );
    manager.shutdown().await;
    server.abort();
}

/// Review finding 10: a client secret issued by dynamic registration is kept
/// (in the secret store, bound to the issuer) and used for refresh after a
/// restart; signing out forgets it.
#[tokio::test(flavor = "multi_thread")]
async fn a_dynamically_registered_secret_survives_a_restart_for_refresh() {
    const DCR_SECRET: &str = "dcr-issued-secret-sentinel-2b8f";
    let (base, seen, server) = fake_authorization_server(true).await;
    {
        let mut s = seen.lock();
        s.dcr_secret = Some(DCR_SECRET.into());
        s.client_secret = Some(DCR_SECRET.into()); // the token endpoint requires it
        s.registration_endpoint = Some(format!("{base}/register"));
        s.expires_in = Some(45); // a refresh is due on the next connect
        s.refresh_allowed = true;
        s.mcp_bearer = Some(REFRESHED.into());
    }
    let store = Arc::new(MemoryStore::default());
    let mut config = McpServerConfig::http("dcr", format!("{base}/mcp"));
    config.oauth = Some(mcp_bridge::OAuthConfig {
        allow_loopback_http: true,
        ..mcp_bridge::OAuthConfig::default()
    });
    let first =
        McpManager::new(tool_registry::ToolRegistry::new()).with_secret_store(store.clone());
    assert!(matches!(
        first.add_server(config.clone()).await,
        Err(McpError::AuthRequired(_))
    ));
    finish_sign_in(
        first
            .start_sign_in("dcr", SignInOptions::default())
            .await
            .unwrap(),
        &base,
        &seen,
    )
    .await;
    first.shutdown().await;
    assert!(store.0.lock().contains_key("mcp/dcr/oauth-client"));
    assert_eq!(seen.lock().token_requests[0]["client_id"], "dcr-client-1");

    // "Restart": a new manager over the same secret store.
    let registry = tool_registry::ToolRegistry::new();
    let second = McpManager::new(registry.clone()).with_secret_store(store.clone());
    second.add_server(config).await.unwrap();
    let requests = seen.lock().token_requests.clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["grant_type"], "refresh_token");
    assert_eq!(
        requests[1].get("client_secret").map(String::as_str),
        Some(DCR_SECRET)
    );
    assert!(registry.get("mcp:dcr/echo").is_some());
    assert!(second.sign_out("dcr").await.unwrap().had_credentials);
    assert!(!store.0.lock().contains_key("mcp/dcr/oauth-client"));
    second.shutdown().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_out_posts_only_the_token_to_the_issuer_revocation_endpoint() {
    let (base, seen, server) = fake_authorization_server(true).await;
    let store = Arc::new(MemoryStore::default());
    let manager = McpManager::new(tool_registry::ToolRegistry::new()).with_secret_store(store);
    assert!(matches!(
        manager
            .add_server(http_config("revoke", format!("{base}/mcp")))
            .await,
        Err(McpError::AuthRequired(_))
    ));
    finish_sign_in(
        manager.start_sign_in("revoke", options()).await.unwrap(),
        &base,
        &seen,
    )
    .await;
    let outcome = manager.sign_out("revoke").await.unwrap();
    assert!(outcome.had_credentials);
    assert_eq!(
        outcome.remote_revocation,
        mcp_bridge::RemoteRevokeOutcome::Revoked
    );
    let revokes = seen.lock().revoke_requests.clone();
    assert_eq!(revokes.len(), 1);
    assert_eq!(revokes[0].get("token").map(String::as_str), Some(REFRESH));
    assert!(!revokes[0].contains_key("client_secret"));
    assert!(!revokes[0].contains_key("client_id"));
    assert!(!revokes[0].contains_key("token_type_hint"));
    manager.shutdown().await;
    server.abort();
}
