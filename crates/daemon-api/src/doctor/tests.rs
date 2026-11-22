use super::*;

fn cli(
    id: &str,
    installed: bool,
    auth: cli_provider::AuthState,
    runnable: bool,
) -> cli_provider::DetectedCli {
    cli_provider::DetectedCli {
        id: id.to_owned(),
        display_name: id.to_uppercase(),
        installed,
        version: installed.then(|| "1.0.0".to_owned()),
        auth,
        account_label: None,
        runnable,
        detail: None,
        message: None,
    }
}

#[test]
fn table_rendering_is_unchanged_for_ok_warn_fail() {
    // Snapshot of the `bot doctor` table as it printed before the checks
    // moved into this module.
    let checks = vec![
        Check::ok("data_dir", "data dir", "/tmp/x (writable)"),
        Check::warn(
            "daemon",
            "daemon",
            "not running on port 1",
            "start it with `bot serve`",
        ),
        Check::fail(
            "providers",
            "providers",
            "alpha: API key not available",
            "set the key",
        ),
        Check::ok("mcp", "mcp config", "no MCP servers configured").hint("never shown"),
    ];
    let mut buf = Vec::new();
    render_table(&checks, &mut buf).unwrap();
    let expected = concat!(
        " \u{2713}  data dir    /tmp/x (writable)\n",
        " !  daemon      not running on port 1\n",
        "                fix: start it with `bot serve`\n",
        " \u{2717}  providers   alpha: API key not available\n",
        "                fix: set the key\n",
        " \u{2713}  mcp config  no MCP servers configured\n",
        "\n",
        "1 failed, 1 warning(s), 2 ok\n",
    );
    assert_eq!(String::from_utf8(buf).unwrap(), expected);
}

#[test]
fn skipped_checks_are_counted_separately_and_never_hinted() {
    let checks = vec![
        Check::ok("a", "a", "fine"),
        Check::skipped("b", "b", "not available on this platform").hint("hidden"),
    ];
    let mut buf = Vec::new();
    render_table(&checks, &mut buf).unwrap();
    let t = String::from_utf8(buf).unwrap();
    assert!(t.contains(" -  b  not available on this platform\n"), "{t}");
    assert!(!t.contains("fix:"), "{t}");
    assert!(
        t.ends_with("0 failed, 0 warning(s), 1 ok, 1 skipped\n"),
        "{t}"
    );
    let j = to_json(&checks);
    assert_eq!(j["ok"], true);
    assert_eq!(j["checks"][1]["status"], "skipped");
    assert_eq!(j["checks"][1]["name"], "b");
}

#[test]
fn repairs_are_a_closed_allow_list() {
    for r in Repair::ALL {
        assert_eq!(Repair::parse(r.id()), Some(r));
        assert_eq!(serde_json::to_value(r).unwrap(), r.id());
    }
    for bad in [
        "",
        "regenerate_api_token",
        "CREATE_DATA_DIR",
        "rm -rf /",
        "create_data_dir ",
    ] {
        assert_eq!(Repair::parse(bad), None, "{bad}");
    }
}

#[test]
fn missing_data_dir_offers_the_create_repair() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope");
    let c = check_data_dir(&missing);
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.repair, Some(Repair::CreateDataDir));
    let c = check_data_dir(d.path());
    assert_eq!(c.status, Status::Ok, "{}", c.detail);
    assert!(
        c.detail.contains("writable") && c.detail.contains("MiB free"),
        "{}",
        c.detail
    );
    assert_eq!(c.repair, None);
}

#[test]
fn data_dir_check_rejects_file_and_checks_free_space_thresholds() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("not-a-dir");
    std::fs::write(&file, b"file").unwrap();
    let c = check_data_dir(&file);
    assert_eq!(c.status, Status::Fail, "{}", c.detail);
    assert!(c.detail.contains("is not a directory"), "{}", c.detail);
    assert!(c.hint.as_deref().unwrap().contains("--data-dir"));
    assert_eq!(c.repair, None);

    let under_file = file.join("child");
    let c = check_data_dir(&under_file);
    assert_eq!(c.status, Status::Fail, "{}", c.detail);
    assert_eq!(c.repair, None);

    let ok = data_dir_with_free_space(d.path(), Ok(DISK_WARN_BYTES));
    assert_eq!(ok.status, Status::Ok);
    assert!(
        ok.detail.contains("writable, 1024 MiB free"),
        "{}",
        ok.detail
    );

    let warn = data_dir_with_free_space(d.path(), Ok(DISK_WARN_BYTES - 1));
    assert_eq!(warn.status, Status::Warn);
    assert!(
        warn.detail.contains("writable, 1023 MiB free"),
        "{}",
        warn.detail
    );
    assert!(warn.hint.as_deref().unwrap().contains("1 GiB"));

    let fail = data_dir_with_free_space(d.path(), Ok(DISK_FAIL_BYTES - 1));
    assert_eq!(fail.status, Status::Fail);
    assert!(
        fail.detail.contains("writable, 99 MiB free"),
        "{}",
        fail.detail
    );
    assert!(
        fail.hint
            .as_deref()
            .unwrap()
            .contains("free some disk space")
    );

    let unknown = data_dir_with_free_space(d.path(), Err("denied".into()));
    assert_eq!(unknown.status, Status::Warn);
    assert!(
        unknown.detail.contains("cannot read free space: denied"),
        "{}",
        unknown.detail
    );
}

#[cfg(unix)]
#[test]
fn read_only_data_dir_fails_writability_probe() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let ro = d.path().join("readonly");
    std::fs::create_dir(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe_blocked = std::fs::File::create(ro.join("root-check")).is_err();
    let _ = std::fs::remove_file(ro.join("root-check"));
    let c = check_data_dir(&ro);
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    if probe_blocked {
        assert_eq!(c.status, Status::Fail, "{}", c.detail);
        assert!(c.detail.contains("is not writable"), "{}", c.detail);
        assert!(c.hint.is_some());
    }
}

#[test]
fn unicode_probe_passes_in_a_non_ascii_temp_dir_and_cleans_up() {
    let d = tempfile::Builder::new()
        .prefix("praxi-çalışma-ğüşİ-")
        .tempdir()
        .unwrap();
    let c = check_unicode_path(d.path());
    assert_eq!(c.status, Status::Ok, "{}", c.detail);
    assert_eq!(
        std::fs::read_dir(d.path()).unwrap().count(),
        0,
        "probe file left behind"
    );
    let c = check_unicode_path(&d.path().join("missing"));
    assert_eq!(c.status, Status::Skipped);
}

#[test]
fn probes_never_touch_an_existing_file_of_the_same_name() {
    let d = tempfile::tempdir().unwrap();
    let victim = d.path().join("providers.toml");
    std::fs::write(&victim, "keep me").unwrap();
    // The probe name is taken: the probe fails, the file is neither
    // truncated nor deleted.
    let err = exclusive_probe(d.path(), "providers.toml", b"ok").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
    // A failing step after creation removes only the probe it created.
    let err = exclusive_probe_with(d.path(), "mine.txt", b"ok", || {
        Err(std::io::Error::other("injected"))
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "injected");
    assert!(!d.path().join("mine.txt").exists());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
}

#[cfg(unix)]
#[test]
fn probes_never_follow_a_symlink() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("providers.toml");
    std::fs::write(&target, "keep me").unwrap();
    let link = d.path().join("probe-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(exclusive_probe(d.path(), "probe-link", b"ok").is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
    assert!(link.symlink_metadata().is_ok(), "the symlink was deleted");
    // A dangling link is not followed into a new file either.
    let dangling = d.path().join("dangling");
    std::os::unix::fs::symlink(d.path().join("created-through-link"), &dangling).unwrap();
    assert!(exclusive_probe(d.path(), "dangling", b"ok").is_err());
    assert!(!d.path().join("created-through-link").exists());
}

#[test]
fn probe_names_are_unpredictable_and_concurrent_probes_do_not_collide() {
    let d = tempfile::tempdir().unwrap();
    let a = probe_name("p-", "");
    let b = probe_name("p-", "");
    assert_ne!(a, b);
    assert!(!a.contains(&std::process::id().to_string()) || a.len() > 34);
    std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| (check_data_dir(d.path()), check_unicode_path(d.path()))))
            .collect();
        for r in runs {
            let (dd, uni) = r.join().unwrap();
            assert_eq!(dd.status, Status::Ok, "{}", dd.detail);
            assert_eq!(uni.status, Status::Ok, "{}", uni.detail);
        }
    });
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
}

#[test]
fn process_guard_check_shows_degraded_when_protection_failed() {
    use process_guard::{Outcome, Report, record};
    let d = tempfile::tempdir().unwrap();
    assert_eq!(check_process_guard(d.path()).status, Status::Skipped);
    std::fs::write(d.path().join(process_guard::REPORT_FILE), "{\"outc").unwrap();
    let torn = check_process_guard(d.path());
    assert_eq!(torn.status, Status::Skipped);
    assert!(torn.detail.contains("unreadable"), "{}", torn.detail);
    let mut report = Report {
        outcome: Outcome::Applied,
        detail: "ok".into(),
        pid: 1,
    };
    record(d.path(), &report).unwrap();
    assert_eq!(check_process_guard(d.path()).status, Status::Ok);
    report.outcome = Outcome::Failed;
    report.detail = "access denied".into();
    record(d.path(), &report).unwrap();
    let c = check_process_guard(d.path());
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.starts_with("DEGRADED:"), "{}", c.detail);
    assert!(c.hint.is_some());
}

#[test]
fn clock_check_flags_impossible_and_backwards_clocks() {
    let floor = SystemTime::UNIX_EPOCH + Duration::from_secs(CLOCK_FLOOR_SECS);
    let now = floor + Duration::from_secs(365 * 24 * 3600);
    assert_eq!(check_clock(now, None).status, Status::Ok);
    assert_eq!(
        check_clock(now, Some(now - Duration::from_secs(60))).status,
        Status::Ok
    );
    assert_eq!(
        check_clock(now, Some(now + Duration::from_secs(60))).status,
        Status::Ok
    );
    let c = check_clock(now, Some(now + Duration::from_secs(3600)));
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains("60 min"), "{}", c.detail);
    assert_eq!(
        check_clock(floor - Duration::from_secs(1), None).status,
        Status::Fail
    );
}

#[test]
fn clock_skew_warns_above_threshold_and_skips_when_unreadable() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let ok = check_clock_skew(now, Ok(now - Duration::from_secs(30)));
    assert_eq!((ok.id.as_str(), ok.status), ("clock_skew", Status::Ok));
    let ahead = check_clock_skew(now, Ok(now - Duration::from_secs(600)));
    assert_eq!(ahead.status, Status::Warn);
    assert!(ahead.detail.contains("10 min ahead of"), "{}", ahead.detail);
    let behind = check_clock_skew(now, Ok(now + Duration::from_secs(3600)));
    assert_eq!(behind.status, Status::Warn);
    assert!(behind.detail.contains("60 min behind"), "{}", behind.detail);
    // Offline: never a warning or failure.
    let off = check_clock_skew(now, Err("offline".into()));
    assert_eq!(off.status, Status::Ok);
    assert!(off.detail.starts_with("skipped"), "{}", off.detail);
}

#[test]
fn http_date_header_parses() {
    let t = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
    assert_eq!(
        t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(),
        784_111_777
    );
    assert!(parse_http_date("yesterday").is_none());
}

#[test]
fn catalog_freshness_warns_when_older_than_90_days() {
    let mut table = provider_catalog::embedded_metadata().clone();
    table.last_reviewed = "2026-01-01".into();
    for p in &mut table.providers {
        p.last_reviewed = None;
    }
    // 2026-01-01 is 20454 days since 1970-01-01.
    let jan_1 = SystemTime::UNIX_EPOCH + Duration::from_secs(20_454 * 86_400);
    let fresh = check_catalog_freshness_for(&table, jan_1 + Duration::from_secs(90 * 86_400), 90);
    assert_eq!(fresh.id, "catalog_freshness");
    assert_eq!(fresh.status, Status::Ok);

    let stale = check_catalog_freshness_for(&table, jan_1 + Duration::from_secs(91 * 86_400), 90);
    assert_eq!(stale.id, "catalog_freshness");
    assert_eq!(stale.status, Status::Warn);
    assert!(stale.detail.contains("91 day(s) ago"), "{}", stale.detail);
    assert!(stale.hint.is_some());
}

#[test]
fn disk_check_thresholds() {
    assert_eq!(disk_check(Ok(DISK_WARN_BYTES)).status, Status::Ok);
    assert_eq!(disk_check(Ok(DISK_WARN_BYTES - 1)).status, Status::Warn);
    assert_eq!(disk_check(Ok(DISK_FAIL_BYTES - 1)).status, Status::Fail);
    assert_eq!(disk_check(Err("denied".into())).status, Status::Warn);
    let d = tempfile::tempdir().unwrap();
    assert_ne!(check_disk(&d.path().join("a/b")).detail, "");
}

#[test]
fn local_models_line_is_informational_and_names_the_pull_command() {
    use provider_catalog::{GpuInfo, HardwareInfo, catalog_candidates, recommend};
    let mut hw = HardwareInfo::unknown("linux", "x86_64");
    hw.ram_bytes = Some(32 << 30);
    hw.gpu = Some(GpuInfo {
        name: "NVIDIA GeForce RTX 4090".into(),
        vram_bytes: Some(24 << 30),
    });
    let c = check_local_models(&hw, &recommend(&hw, &catalog_candidates()));
    assert_eq!(c.status, Status::Ok);
    assert!(c.detail.contains("RTX 4090") && c.detail.contains("ollama pull"));
    // Nothing known: neutral, and honest about it.
    let none = HardwareInfo::unknown("linux", "x86_64");
    let c = check_local_models(&none, &recommend(&none, &catalog_candidates()));
    assert_eq!(c.status, Status::Skipped);
    assert!(c.detail.contains("no recommendation"));
}

#[test]
fn unavailable_capabilities_are_neutral_never_failures() {
    let caps = [
        CapabilityStatus {
            id: "browser".into(),
            available: true,
            reason: None,
        },
        CapabilityStatus {
            id: "display".into(),
            available: false,
            reason: Some("not available on this platform".into()),
        },
    ];
    let c = check_capabilities(&caps);
    assert_eq!(c.status, Status::Ok);
    assert!(
        c.detail
            .contains("display (not available on this platform)"),
        "{}",
        c.detail
    );
    let none = [CapabilityStatus {
        id: "display".into(),
        available: false,
        reason: None,
    }];
    assert_eq!(check_capabilities(&none).status, Status::Skipped);
}

#[test]
fn desktop_automation_is_neutral_off_windows() {
    assert_eq!(
        check_desktop_automation("linux", None).status,
        Status::Skipped
    );
    assert_eq!(
        check_desktop_automation("macos", None).detail,
        "not available on this platform"
    );
    assert_eq!(
        check_desktop_automation("windows", None).status,
        Status::Warn
    );
    let caps = desktop_api::Capabilities {
        platform: "windows".into(),
        accessibility_tree: true,
        screenshot: true,
        app_launch: false,
        has_display: true,
    };
    let c = check_desktop_automation("windows", Some(&caps));
    assert_eq!(c.status, Status::Ok);
    assert_eq!(c.detail, "windows: accessibility tree, screenshots");
    let headless = desktop_api::Capabilities {
        has_display: false,
        ..caps
    };
    assert_eq!(
        check_desktop_automation("windows", Some(&headless)).status,
        Status::Skipped
    );
}

#[test]
fn peer_link_reflects_host_toml() {
    let d = tempfile::tempdir().unwrap();
    let net = LinkRuntime::Network(vec![]);
    assert_eq!(check_peer_link(d.path(), &net).status, Status::Skipped);
    std::fs::write(d.path().join("host.toml"), "mode = \"host\"\n").unwrap();
    assert_eq!(check_peer_link(d.path(), &net).status, Status::Ok);
    std::fs::write(
        d.path().join("host.toml"),
        "mode = \"client\"\nstate = \"standby\"\n",
    )
    .unwrap();
    assert_eq!(check_peer_link(d.path(), &net).status, Status::Warn);
    std::fs::write(d.path().join("host.toml"), "mode = \"client\"\n").unwrap();
    assert_eq!(check_peer_link(d.path(), &net).status, Status::Skipped);
    assert_eq!(check_host(d.path()).status, Status::Fail);
}

const LEADER: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn link_peer(key: &str, connected: bool, last_seen_secs: Option<u64>) -> LinkPeer {
    LinkPeer {
        key: key.into(),
        role: "host".into(),
        connected,
        last_seen_secs,
    }
}

#[test]
fn peer_link_distinguishes_configured_from_connected() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("host.toml"),
        format!("mode = \"client\"\nstate = \"standby\"\nleader = \"{LEADER}\"\n"),
    )
    .unwrap();
    let run = |live: LinkRuntime| check_peer_link(d.path(), &live);
    // Connected: the only healthy client state.
    let c = run(LinkRuntime::Network(vec![link_peer(LEADER, true, Some(1))]));
    assert_eq!(c.status, Status::Ok, "{}", c.detail);
    assert!(c.detail.starts_with("connected to host"), "{}", c.detail);
    // Paired but the host (VPS) is offline.
    let c = run(LinkRuntime::Network(vec![link_peer(
        LEADER,
        false,
        Some(600),
    )]));
    assert_eq!(c.status, Status::Fail, "{}", c.detail);
    assert!(
        c.detail.contains("not connected (last seen 10 min ago)"),
        "{}",
        c.detail
    );
    assert!(c.hint.is_some());
    // Revoked: the leader is no longer among the paired devices.
    let c = run(LinkRuntime::Network(vec![link_peer(
        "other",
        true,
        Some(1),
    )]));
    assert_eq!(c.status, Status::Fail);
    assert!(c.detail.contains("no longer paired"), "{}", c.detail);
    // Peer networking off in the daemon.
    let c = run(LinkRuntime::NetworkDisabled);
    assert_eq!(c.status, Status::Fail);
    assert!(c.detail.contains("peer networking is off"), "{}", c.detail);
    // No live state (CLI): not verified, never ok.
    let c = run(LinkRuntime::Unknown);
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains("not verified"), "{}", c.detail);

    // Host mode needs a running endpoint too.
    std::fs::write(d.path().join("host.toml"), "mode = \"host\"\n").unwrap();
    assert_eq!(run(LinkRuntime::NetworkDisabled).status, Status::Fail);
    assert_eq!(run(LinkRuntime::Unknown).status, Status::Warn);
    let c = run(LinkRuntime::Network(vec![
        link_peer("a", true, Some(1)),
        link_peer("b", false, None),
    ]));
    assert_eq!(c.status, Status::Ok);
    assert!(
        c.detail.ends_with("2 paired device(s), 1 connected"),
        "{}",
        c.detail
    );
}

#[test]
fn cli_accounts_from_one_detection_result() {
    use cli_provider::AuthState::{SignedIn, SignedOut, Unknown};
    assert_eq!(
        check_cli_accounts(Some(&[cli("claude", false, Unknown, false)])).status,
        Status::Skipped
    );
    let c = check_cli_accounts(Some(&[
        cli("claude", true, SignedIn, true),
        cli("codex", true, Unknown, false),
    ]));
    assert_eq!(c.status, Status::Ok);
    assert_eq!(c.detail, "ready: CLAUDE; detected only: CODEX");
    let c = check_cli_accounts(Some(&[cli("claude", true, SignedOut, true)]));
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains("signed out: CLAUDE"));
}

#[test]
fn unknown_or_unprobed_cli_sign_in_is_a_warning_not_ready() {
    use cli_provider::AuthState::Unknown;
    // Detection did not answer: not verified, with what to do.
    let c = check_cli_accounts(None);
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains("not verified"), "{}", c.detail);
    assert!(c.hint.is_some());
    // Installed and runnable, but the auth output could not be read.
    let c = check_cli_accounts(Some(&[cli("claude", true, Unknown, true)]));
    assert_eq!(c.status, Status::Warn, "{}", c.detail);
    assert_eq!(c.detail, "sign-in not verified: CLAUDE");
    assert!(!c.detail.contains("ready"), "{}", c.detail);
    assert!(c.hint.as_deref().unwrap().contains("sign in"));
}

fn write_providers(dir: &Path, settings: Vec<ProviderSettings>) {
    let mut cfg = ProviderConfig::default();
    for s in settings {
        cfg.upsert(s);
    }
    cfg.save(&config_path(dir)).unwrap();
}

fn cli_settings() -> ProviderSettings {
    let mut s = ProviderSettings::new("cli:claude", 1);
    s.cli = Some(provider_catalog::config::CliSettings::Cli {
        engine: provider_catalog::config::CliEngine::Claude,
        command: "claude".into(),
    });
    s
}

#[test]
fn cli_provider_needs_a_signed_in_cli_not_an_api_key() {
    use cli_provider::AuthState::{SignedIn, SignedOut};
    let d = tempfile::tempdir().unwrap();
    write_providers(d.path(), vec![cli_settings()]);
    let cat = Catalog::from_entries([]);
    let store = secret_store::MemoryStore::new();
    let store_ref: StoreRef<'_> = Ok((&store, "Memory".into()));
    let run = |detected: Option<&[cli_provider::DetectedCli]>| {
        check_providers(&ProviderInputs {
            data_dir: d.path(),
            catalog: Ok(&cat),
            store: &store_ref,
            detected,
            online: None,
        })
    };
    let c = run(Some(&[cli("claude", true, SignedIn, true)]));
    assert_eq!(c.status, Status::Ok, "{}", c.detail);
    let c = run(Some(&[cli("claude", true, SignedOut, true)]));
    assert_eq!(c.status, Status::Fail);
    assert!(c.detail.contains("signed out"), "{}", c.detail);
    let c = run(Some(&[cli("claude", false, SignedOut, false)]));
    assert!(c.detail.contains("not installed"), "{}", c.detail);
    // Not probed (detection timed out): a warning saying so, never ready.
    let c = run(None);
    assert_eq!(c.status, Status::Warn, "{}", c.detail);
    assert!(c.detail.contains("CLI was not checked"), "{}", c.detail);
    assert!(!c.detail.contains("CLI ready"), "{}", c.detail);
    // Runnable, sign-in state unknown: a warning naming the sign-in command.
    let c = run(Some(&[cli(
        "claude",
        true,
        cli_provider::AuthState::Unknown,
        true,
    )]));
    assert_eq!(c.status, Status::Warn, "{}", c.detail);
    assert!(c.detail.contains("could not be verified"), "{}", c.detail);
    assert!(!c.detail.contains("CLI ready"), "{}", c.detail);
}

#[test]
fn providers_on_disk_but_not_loaded_offer_a_reload() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        check_providers_loaded(d.path(), &[]).status,
        Status::Skipped
    );
    write_providers(d.path(), vec![cli_settings()]);
    let c = check_providers_loaded(d.path(), &[]);
    assert_eq!(c.status, Status::Warn);
    assert_eq!(c.repair, Some(Repair::ReloadProviders));
    let c = check_providers_loaded(d.path(), &["cli:claude".to_owned()]);
    assert_eq!(c.status, Status::Ok);
    assert_eq!(c.repair, None);
}

fn scrub_with<'a>(secrets: &'a dyn Fn(&str) -> String, home: Option<&'a str>) -> Scrub<'a> {
    Scrub {
        secrets: Some(secrets),
        home,
        identities: vec![],
        known: KnownIds::default(),
    }
}

#[test]
fn redaction_masks_home_secrets_emails_and_tokens() {
    let r = agent_core::SharedRedactor::default();
    r.register("hunter2-correct-horse");
    let redact = |t: &str| r.redact(t);
    let home = r"C:\Users\Emre Öcel";
    let text = format!(
        "path C:\\Users\\Emre Öcel\\AppData\\praxi and c:/users/emre öcel/x; \
         key sk-0123456789abcdef0123456789; secret hunter2-correct-horse; \
         mail emre.ocel+bot@example.co.uk. token {} short-id",
        "z".repeat(40)
    );
    let out = redact_report(&text, &scrub_with(&redact, Some(home)));
    assert!(out.contains("path ~\\AppData\\praxi"), "{out}");
    for leaked in [
        "Emre",
        "Öcel",
        "öcel",
        "emre ",
        "0123456789abcdef",
        "hunter2",
        "example.co",
        "emre.ocel",
        "zzzz",
    ] {
        assert!(!out.contains(leaked), "{leaked} leaked: {out}");
    }
    assert!(out.contains("[email]."), "{out}");
    assert!(out.contains("short-id"), "{out}");
    assert_eq!(mask_emails("a@b"), "a@b");
    assert_eq!(mask_emails("@x.org and x@.org"), "@x.org and x@.org");
}

/// A secret with URL-special characters, as the review planted it.
const SPECIAL: &str = "tok/en+value?with=sym&bols-1234";

#[test]
fn encoded_forms_of_a_known_secret_are_masked() {
    use base64::Engine as _;
    let r = agent_core::SharedRedactor::default();
    r.register(SPECIAL);
    let redact = |t: &str| r.redact(t);
    let scrub = scrub_with(&redact, None);
    let url = "tok%2Fen%2Bvalue%3Fwith%3Dsym%26bols-1234";
    let b64 = base64::engine::general_purpose::STANDARD.encode(SPECIAL);
    let b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(SPECIAL);
    let json = serde_json::to_string(&format!("{SPECIAL}\"")).unwrap();
    for (form, text) in [
        ("literal", format!("value {SPECIAL} end")),
        ("url-encoded", format!("value {url} end")),
        ("base64", format!("value {b64} end")),
        ("base64url", format!("value {b64url} end")),
        ("json-escaped", format!("value {json} end")),
    ] {
        let out = scrub.text(&text);
        for piece in ["tok/en", "tok%2F", "sym&bols", "sym%26bols", "bols-1234"] {
            assert!(!out.contains(piece), "{form}: {piece} leaked: {out}");
        }
        assert!(!out.contains(&b64[4..20]), "{form}: base64 leaked: {out}");
        assert!(
            out.contains("value ") && out.ends_with(" end"),
            "{form}: {out}"
        );
    }
}

#[test]
fn lowercase_percent_encoding_of_short_or_dotted_secrets_is_masked() {
    // Short segments (< 12 characters) that the token heuristic leaves
    // alone; only the registered encodings can catch them.
    let r = agent_core::SharedRedactor::default();
    r.register("pass/wd");
    r.register("my.pass/word");
    let redact = |t: &str| r.redact(t);
    let scrub = scrub_with(&redact, None);
    for text in [
        "pass%2fwd: configured",
        "https://api.example.com/pass%2fwd",
        r"data dir D:\my.pass%2fword\data",
        "https://h/my.pass%2Fword and my%2epass%2fword",
    ] {
        let out = scrub.text(text);
        for piece in ["pass%2", "pass/", "%2fwd", "%2Fword", "%2fword"] {
            assert!(!out.contains(piece), "{piece} leaked from {text:?}: {out}");
        }
    }
    // Without the secret list the same text stays (this is what the
    // registered variants are for).
    assert!(
        Scrub::default()
            .text("pass%2fwd: configured")
            .contains("pass%2fwd")
    );
}

/// A dated model id longer than the 24-character token heuristic.
const DATED_MODEL: &str = "claude-sonnet-4-5-20250929";

fn anthropic_catalog() -> Catalog {
    let entry: ProviderEntry =
        toml::from_str(include_str!("../../../../providers/catalog/anthropic.toml")).unwrap();
    Catalog::from_entries([entry])
}

#[test]
fn model_and_provider_ids_from_trusted_fields_stay_readable() {
    let d = tempfile::tempdir().unwrap();
    let mut def = ProviderSettings::new("anthropic", 1);
    def.default_model = Some(DATED_MODEL.into());
    let mut custom_entry = anthropic_catalog().get("anthropic").unwrap().clone();
    custom_entry.id = "my-vps-llm".into();
    custom_entry.base_url = "https://llm.private-vps.example/v1".into();
    let mut custom = ProviderSettings::new("my-vps-llm", 2);
    custom.custom = Some(custom_entry);
    write_providers(d.path(), vec![def, custom]);
    let cat = anthropic_catalog();
    // The check the review quoted.
    let c = check_catalog(d.path(), &Ok(&cat));
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains(DATED_MODEL), "{}", c.detail);
    let scrub = Scrub {
        known: KnownIds::collect(d.path(), Some(&cat)),
        ..Scrub::default()
    };
    let info = ReportInfo {
        version: "1".into(),
        api_version: "1".into(),
        platform: "x".into(),
        data_dir: PathBuf::from("/data"),
        config_files: vec![],
    };
    let md = report_markdown(&info, std::slice::from_ref(&c), &scrub);
    let md = redact_report_text(&md, &scrub);
    assert!(
        md.contains(&format!(
            "default model {DATED_MODEL} is not in the catalog"
        )),
        "{md}"
    );
    // Catalog hosts stay; the user's own custom host does not.
    let out = scrub.text(
        "anthropic: https://api.anthropic.com/v1/messages failed; my-vps-llm: https://llm.private-vps.example/v1 failed",
    );
    assert!(
        out.contains("https://api.anthropic.com/v1/messages"),
        "{out}"
    );
    assert!(!out.contains("private-vps"), "{out}");
    // Unknown long runs of the same shape are still masked, and a secret equal
    // to a known id is still masked by the exact-value redactor.
    let out = scrub.text("value claude-sonnet-4-5-99999999 here");
    assert!(out.contains("[redacted]"), "{out}");
    let r = agent_core::SharedRedactor::default();
    r.register(DATED_MODEL);
    let redact = |t: &str| r.redact(t);
    let with_secret = Scrub {
        secrets: Some(&redact),
        known: KnownIds::collect(d.path(), Some(&cat)),
        ..Scrub::default()
    };
    assert!(!with_secret.text(&c.detail).contains(DATED_MODEL));
}

#[test]
fn unknown_secrets_are_masked_by_pattern() {
    // No known secrets at all: the patterns alone must catch these.
    let scrub = Scrub::default();
    // Lower-case percent escapes are not among the registered variants.
    let out = scrub.text("bad value tok%2fen%2bvalue%3fwith in line");
    assert!(!out.contains("tok%2f"), "{out}");
    // URL credentials, a non-loopback host and the query string.
    let out = scrub.text(
        "custom.vps: https://alice:s3cr3t-pass@my-vps.example.net:8443/v1/models?api_key=abc123&x=1 failed",
    );
    for leaked in ["alice", "s3cr3t", "my-vps", "example.net", "abc123"] {
        assert!(!out.contains(leaked), "{leaked} leaked: {out}");
    }
    assert!(
        out.contains("https://[redacted]@[host]/v1/models?[redacted] failed"),
        "{out}"
    );
    // Loopback stays readable.
    let out = scrub.text("not running on http://127.0.0.1:7878/v1/health");
    assert!(out.contains("http://127.0.0.1:7878/v1/health"), "{out}");
    // Credential assignments in any quoting.
    for text in [
        "api_key = \"short-pass1\" here",
        "password: hunter22 here",
        "Authorization: Bearer abc.def here",
        "auth_token='q w e' here",
    ] {
        let out = scrub.text(text);
        for leaked in ["short-pass1", "hunter22", "abc.def", "q w e"] {
            assert!(
                !out.contains(leaked),
                "{leaked} leaked from {text:?}: {out}"
            );
        }
        assert!(out.ends_with(" here"), "{out}");
    }
}

#[test]
fn parse_errors_lose_their_quoted_config_lines() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        config_path(d.path()),
        "[[providers]]\nid = \"alpha\"\nnote = owner-text-xyz private.host.example\n",
    )
    .unwrap();
    let c = check_config(d.path());
    assert_eq!(c.status, Status::Fail);
    let out = Scrub::default().text(&c.detail);
    assert!(!out.contains("owner-text-xyz"), "{out}");
    assert!(!out.contains("private.host"), "{out}");
    assert!(out.contains("line 3"), "location kept: {out}");
}

#[test]
fn user_host_names_and_peer_keys_are_masked() {
    let scrub = Scrub {
        secrets: None,
        home: None,
        identities: vec![("alice".into(), "[user]"), ("DESKTOP-K7Q".into(), "[host]")],
        known: KnownIds::default(),
    };
    let out =
        scrub.text(r"data D:\alice\bot on desktop-k7q; malice stays; client of host abcdef01… ok");
    assert_eq!(
        out,
        r"data D:\[user]\bot on [host]; malice stays; client of host [peer] ok"
    );
}

#[test]
fn secrets_with_markdown_characters_are_masked_before_escaping() {
    let r = agent_core::SharedRedactor::default();
    r.register("pipe|secret\nline-2x");
    let redact = |t: &str| r.redact(t);
    let scrub = scrub_with(&redact, None);
    let info = ReportInfo {
        version: "1".into(),
        api_version: "1".into(),
        platform: "x".into(),
        data_dir: PathBuf::from("/data"),
        config_files: vec![],
    };
    let md = report_markdown(
        &info,
        &[Check::fail(
            "x",
            "x",
            "got pipe|secret\nline-2x back",
            "fix",
        )],
        &scrub,
    );
    assert!(!md.contains("pipe"), "{md}");
    assert!(!md.contains("line-2x"), "{md}");
}

#[test]
fn report_lists_checks_and_config_names_only() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("providers.toml"), "secret = \"inside\"").unwrap();
    std::fs::write(d.path().join("api-token"), "t").unwrap();
    let info = ReportInfo {
        version: "1.2.3".into(),
        api_version: "1.0".into(),
        platform: "linux/x86_64".into(),
        data_dir: d.path().to_path_buf(),
        config_files: present_config_files(d.path()),
    };
    let md = report_markdown(
        &info,
        &[
            Check::fail("x", "x | y", "bad\nline", "fix it"),
            Check::skipped("s", "s", "n/a"),
        ],
        &Scrub::default(),
    );
    assert!(md.contains("- Config files: providers.toml\n"), "{md}");
    assert!(!md.contains("inside") && !md.contains("api-token"), "{md}");
    assert!(
        md.contains("| fail | x \\| y | bad line (fix: fix it) |"),
        "{md}"
    );
    assert!(
        md.contains("1 failed, 0 warning(s), 0 ok, 1 skipped"),
        "{md}"
    );
}
