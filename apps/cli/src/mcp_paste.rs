//! `bot mcp add <paste>`: the daemon-facing half. Parsing a paste into server
//! configs lives in [`mcp_bridge::paste`] (shared with the import screen);
//! this file creates the servers through the daemon and stores each pasted
//! value in the secret store.

pub use mcp_bridge::paste::*;
use serde_json::json;

/// `bot mcp add`: create the server, store each pasted value in the secret
/// store (through the daemon, write-only), then enable it; the daemon
/// connects it and answers with its state.
///
/// Staged so a refused add never touches a live credential (review finding
/// 8): a name already in use (in any letter case) is refused before anything
/// is written; the server is created **disabled** first (a 409 / 422 writes
/// no secret); a failed secret write removes the new server again.
///
/// # Errors
/// The daemon refusing a secret or the server (exit 6), or unreachable.
pub async fn add(
    c: &crate::client::Client,
    parsed: &[Parsed],
    dry_run: bool,
    json_out: bool,
    out: &mut dyn std::io::Write,
) -> crate::Result<()> {
    let mut results = Vec::new();
    for p in parsed {
        let body = create_body(&p.config);
        if dry_run {
            let secrets: Vec<&str> = p.secrets.iter().map(|(n, _)| n.as_str()).collect();
            results.push(json!({"would_create": body, "would_store_secrets": secrets}));
            continue;
        }
        let existing = c
            .request(reqwest::Method::GET, "/v1/mcp/servers", None)
            .await?;
        let taken = existing.as_array().is_some_and(|servers| {
            servers.iter().any(|s| {
                s["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&p.config.id))
            })
        });
        if taken {
            return Err(crate::email_cmd::rejected(
                "a server with this name already exists; choose another with --name",
            ));
        }
        if p.secrets.is_empty() {
            results.push(
                c.request(reqwest::Method::POST, "/v1/mcp/servers", Some(body))
                    .await?,
            );
            continue;
        }
        let mut disabled = body;
        disabled["enabled"] = json!(false);
        c.request(reqwest::Method::POST, "/v1/mcp/servers", Some(disabled))
            .await?;
        let path = format!("/v1/mcp/servers/{}", p.config.id);
        for (secret, value) in &p.secrets {
            let stored = c
                .request(
                    reqwest::Method::POST,
                    &format!("/v1/secrets/{secret}"),
                    Some(json!({ "value": value })),
                )
                .await;
            if let Err(error) = stored {
                let _ = c.request(reqwest::Method::DELETE, &path, None).await;
                return Err(error);
            }
        }
        results.push(
            c.request(
                reqwest::Method::PUT,
                &path,
                Some(json!({ "enabled": p.config.enabled })),
            )
            .await?,
        );
    }
    if json_out {
        writeln!(out, "{}", serde_json::to_string_pretty(&results)?)?;
        return Ok(());
    }
    for r in &results {
        if let Some(body) = r.get("would_create") {
            writeln!(
                out,
                "would add {} ({}); secrets to store: {}",
                body["name"].as_str().unwrap_or("?"),
                body["transport"].as_str().unwrap_or("?"),
                r["would_store_secrets"]
            )?;
            continue;
        }
        let name = r["name"].as_str().unwrap_or("?");
        let status = r["status"].as_str().unwrap_or("?");
        writeln!(out, "added {name}: {status}")?;
        match status {
            "auth_required" => writeln!(
                out,
                "  {name} needs a sign-in: open Apps in the desktop app and choose Sign in"
            )?,
            "error" | "quarantined" => {
                if let Some(e) = r["error"].as_str() {
                    writeln!(out, "  {e}")?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bot mcp add` against a live daemon with a real `McpHost`: the pasted
    /// header value lands in the secret store only, the server is created and
    /// tried, and a 401 is reported as a sign-in to do.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_stores_pasted_values_as_secrets_and_reports_the_state() {
        use std::sync::Arc;
        const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        const VALUE: &str = "pasted-header-value-sentinel-4be0";
        let remote = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/mcp"))
            .respond_with(
                wiremock::ResponseTemplate::new(401).insert_header("www-authenticate", "Bearer"),
            )
            .mount(&remote)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
        let agent = crate::serve::build_agent(
            &storage,
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            dir.path(),
            None,
        );
        let store: Arc<dyn secret_store::SecretStore> = Arc::new(secret_store::MemoryStore::new());
        let host = crate::mcp_host::McpHost::start(
            dir.path(),
            agent.tool_registry(),
            Some(Arc::clone(&store)),
            None,
            None,
        );
        let state = daemon_api::AppState::new(agent, storage, daemon_api::DaemonConfig::new(TOKEN))
            .unwrap();
        state.set_owner_key(crate::client::TEST_OWNER_KEY.into());
        state.set_mcp(Arc::clone(&host) as Arc<dyn daemon_api::McpControl>);
        state.set_secret_store(Arc::clone(&store));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
        let client = crate::client::Client::new(&url, TOKEN.to_owned())
            .with_owner_key(Some(crate::client::TEST_OWNER_KEY.to_owned()));

        let paste = format!(
            "claude mcp add --transport http remote {}/mcp --header \"X-Api-Key: {VALUE}\"",
            remote.uri()
        );
        let parsed = parse(&paste, None).unwrap();
        let mut dry = Vec::new();
        add(&client, &parsed, true, false, &mut dry).await.unwrap();
        let dry = String::from_utf8(dry).unwrap();
        assert!(dry.contains("would add remote (http)"), "{dry}");
        assert!(store.get("mcp/remote/header/x-api-key").unwrap().is_none());

        let mut out = Vec::new();
        add(&client, &parsed, false, false, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("added remote: auth_required"), "{out}");
        assert!(out.contains("Sign in"), "{out}");
        assert_eq!(
            store
                .get("mcp/remote/header/x-api-key")
                .unwrap()
                .unwrap()
                .expose(),
            VALUE
        );
        let file = std::fs::read_to_string(dir.path().join(crate::doctor::MCP_FILE)).unwrap();
        assert!(file.contains("mcp/remote/header/x-api-key"), "{file}");
        for text in [&dry, &out, &file] {
            assert!(!text.contains(VALUE));
        }
        host.shutdown().await;
    }

    /// A daemon with a real `McpHost`; `secrets: false` leaves the secret
    /// store out, so every secret write fails.
    async fn paste_daemon(
        secrets: bool,
    ) -> (
        crate::client::Client,
        std::sync::Arc<dyn secret_store::SecretStore>,
        std::sync::Arc<crate::mcp_host::McpHost>,
        tempfile::TempDir,
    ) {
        use std::sync::Arc;
        const TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(storage::Storage::open(dir.path()).unwrap());
        let agent = crate::serve::build_agent(
            &storage,
            agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider)),
            dir.path(),
            None,
        );
        let store: Arc<dyn secret_store::SecretStore> = Arc::new(secret_store::MemoryStore::new());
        let host = crate::mcp_host::McpHost::start(
            dir.path(),
            agent.tool_registry(),
            Some(Arc::clone(&store)),
            None,
            None,
        );
        let state = daemon_api::AppState::new(agent, storage, daemon_api::DaemonConfig::new(TOKEN))
            .unwrap();
        state.set_owner_key(crate::client::TEST_OWNER_KEY.into());
        state.set_mcp(Arc::clone(&host) as Arc<dyn daemon_api::McpControl>);
        if secrets {
            state.set_secret_store(Arc::clone(&store));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(daemon_api::serve(listener, state, std::future::pending()));
        (
            crate::client::Client::new(&url, TOKEN.to_owned())
                .with_owner_key(Some(crate::client::TEST_OWNER_KEY.to_owned())),
            store,
            host,
            dir,
        )
    }

    /// Review finding 8: distinct servers and keys never share a secret, and
    /// a refused or failing add never alters a live credential.
    #[tokio::test(flavor = "multi_thread")]
    async fn pasted_secret_names_never_collide_and_a_refused_add_writes_nothing() {
        // Names differing only in `-` / `_` / case stay apart.
        let a = parse("A_TOKEN=one npx -y pkg@1.2.3", Some("a-b")).unwrap();
        let b = parse("A_TOKEN=two npx -y pkg@1.2.3", Some("a_b")).unwrap();
        assert_ne!(a[0].secrets[0].0, b[0].secrets[0].0);
        let headers = parse(
            "claude mcp add --transport http s https://s.example/mcp -H \"X-A: 1\" -H \"X_B: 2\"",
            None,
        );
        assert!(headers.is_err(), "`X_B` is not a header name");
        for dup in [
            "claude mcp add --transport http s https://s.example/mcp -H \"X-A: 1\" -H \"x-a: 2\"",
            "A=1 a=2 npx -y pkg@1.2.3",
            r#"{"mcpServers": {"a/b": {"url": "https://a.example/mcp"}, "a-b": {"url": "https://b.example/mcp"}}}"#,
        ] {
            assert!(parse(dup, None).is_err(), "{dup}");
        }
        // Names stay within the secret store's 128 bytes; ids are bounded.
        let long = "x".repeat(101);
        assert!(parse("https://s.example/mcp", Some(&long)).is_err());
        let near = "x".repeat(100);
        let near_limit = parse(
            &format!(
                "claude mcp add --transport http {near} https://s.example/mcp -H \"{}: v\"",
                "h".repeat(20)
            ),
            None,
        );
        assert!(near_limit.is_err(), "secret name over 128 bytes accepted");

        let (client, store, host, _dir) = paste_daemon(true).await;
        // An existing server's credential is not overwritten by a second add
        // of the same name (in any letter case).
        // A command that does not exist: nothing is installed or fetched.
        let missing = |value: &str| {
            format!(
                r#"{{"command": "definitely-missing-mcp-server", "env": {{"API_KEY": "{value}"}}}}"#
            )
        };
        let first = parse(&missing("original-value"), Some("files")).unwrap();
        let mut out = Vec::new();
        add(&client, &first, false, false, &mut out).await.unwrap();
        assert_eq!(
            store
                .get("mcp/files/env/API_KEY")
                .unwrap()
                .unwrap()
                .expose(),
            "original-value"
        );
        for name in ["files", "FILES"] {
            let again = parse(&missing("attacker-value"), Some(name)).unwrap();
            assert!(
                add(&client, &again, false, false, &mut Vec::new())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            store
                .get("mcp/files/env/API_KEY")
                .unwrap()
                .unwrap()
                .expose(),
            "original-value"
        );
        host.shutdown().await;

        // A secret that cannot be stored leaves no server behind.
        let (client, _store, host, _dir) = paste_daemon(false).await;
        let parsed = parse(&missing("v"), Some("orphan")).unwrap();
        assert!(
            add(&client, &parsed, false, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(
            daemon_api::McpControl::list(host.as_ref()).len(),
            0,
            "the new server was not rolled back"
        );
        host.shutdown().await;
    }
}
