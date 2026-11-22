//! `bot doctor`: environment self-check with fix hints.
//!
//! The checks shared with the daemon (`GET /v1/doctor`) live in
//! [`daemon_api::doctor`]; this module adds the ones only a terminal run can
//! do (port probe, browser launch, `--online` probes, MCP / web / Telegram
//! files) and renders the same typed [`Check`] data.  [`run_checks`] is pure
//! over a [`DoctorEnv`] (everything that touches the host is injected), so
//! tests drive it with temp dirs. Secrets are never read into the output:
//! only their presence is checked, and the `--report` bundle is redacted
//! ([`redact`]).

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use daemon_api::doctor as core;
pub use daemon_api::doctor::{Check, Status, render_table, to_json};
use mcp_bridge::{McpConfigStore, TomlFileStore};
use provider_catalog::{Catalog, OpenAiCompatConfig, ProviderConfig, config_path, test_connection};
use secret_store::SecretStore;
use serde_json::{Value, json};

use crate::Result;

/// File name of the MCP server configuration in the data dir.
pub const MCP_FILE: &str = "mcp.toml";

/// Everything the checks look at.
pub struct DoctorEnv<'a> {
    /// Data directory.
    pub data_dir: &'a Path,
    /// The loaded provider catalog (or why loading failed).
    pub catalog: std::result::Result<&'a Catalog, String>,
    /// The opened secret store and a label of the backend (or why it failed).
    pub store: std::result::Result<(&'a dyn SecretStore, String), String>,
    /// Daemon port.
    pub port: u16,
    /// Test provider connections over the network.
    pub online: bool,
    /// Browser binary found on this machine, if any.
    pub browser: Option<PathBuf>,
    /// Discovery failure for an unusable configured or snap-only browser.
    pub browser_error: Option<String>,
    /// Coding CLIs found on this machine (`None` = detection did not run).
    pub detected: Option<Vec<cli_provider::DetectedCli>>,
    /// Capabilities of the desktop automation backend, if one loads.
    pub desktop: Option<desktop_api::Capabilities>,
    /// Opt-in HTTPS endpoint whose `Date` header is compared with the local
    /// clock (`bot doctor --time-url`). `None` = no network request is made.
    pub time_url: Option<&'a str>,
}

/// Run every check.  The order of the original `bot doctor` lines is kept;
/// checks added later are appended.
#[must_use]
pub fn run_checks(env: &DoctorEnv<'_>) -> Vec<Check> {
    let online: core::OnlineProbe<'_> = &online_test;
    let detected = env.detected.as_deref();
    let mut checks = vec![
        core::check_host(env.data_dir),
        check_peer_relay(env),
        check_host_display(env),
        check_desktop_backend(env),
        check_free_ram(),
        core::check_data_dir(env.data_dir),
        core::check_config(env.data_dir),
        check_decider(env),
        core::check_secrets(&env.store),
        polish_provider_check(core::check_providers(&core::ProviderInputs {
            data_dir: env.data_dir,
            catalog: env.catalog.clone(),
            store: &env.store,
            detected,
            online: env.online.then_some(online),
        })),
        core::check_catalog(env.data_dir, &env.catalog),
        core::check_catalog_freshness(std::time::SystemTime::now()),
        check_daemon(env.data_dir, env.port),
        env.browser_error.as_ref().map_or_else(
            || check_browser(env.browser.as_deref()),
            |e| Check::fail("browser", "browser", e, "install a non-snap browser or set BOT_BROWSER_PATH; use `sudo praxi doctor` for service-context validation"),
        ),
        check_mcp(env.data_dir),
        check_secret_request(env.data_dir),
        check_web(env),
        core::check_schedule_tools(env.data_dir),
        core::check_process_guard(env.data_dir),
        core::check_safe_mode(
            core::safe_mode_from_env(std::env::var("BOT_SAFE_MODE").ok().as_deref())
                || daemon_safe_mode(env.port),
        ),
        core::check_disk(env.data_dir),
        check_local_models(env.data_dir),
        check_resources(),
        check_telegram(env),
        core::check_unicode_path(env.data_dir),
        core::check_cli_accounts(detected),
        core::check_peer_link(env.data_dir, &core::LinkRuntime::Unknown),
        core::check_desktop_automation(std::env::consts::OS, env.desktop.as_ref()),
        core::check_clock(std::time::SystemTime::now(), core::newest_mtime(env.data_dir)),
    ];
    if let Some(url) = env.time_url {
        checks.push(core::check_clock_skew(
            std::time::SystemTime::now(),
            fetch_network_time(url),
        ));
    }
    checks
}

/// `Date` header of one HTTPS HEAD request (5 s limit). Plain http is refused
/// so the opt-in request is never sent unencrypted.
fn fetch_network_time(url: &str) -> std::result::Result<std::time::SystemTime, String> {
    if !url.starts_with("https://") {
        return Err("the time URL must start with https://".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.head(url).send().await.map_err(|e| e.to_string())?;
        let date = resp
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|v| v.to_str().ok())
            .ok_or("no Date header in the answer")?;
        core::parse_http_date(date).ok_or_else(|| "unreadable Date header".to_string())
    })
}

fn check_decider(env: &DoctorEnv<'_>) -> Check {
    use std::fmt::Write as _;

    const ID: &str = "decider";
    const TITLE: &str = "decision pool";
    let config = match agent_core::decider::Settings::load(env.data_dir) {
        Ok(config) => config,
        Err(error) => {
            return Check::warn(
                ID,
                TITLE,
                error,
                "check decider.toml against the decision pool guide",
            );
        }
    };
    let mut detail = format!(
        "enabled={}, timeout_ms={}, max_calls_per_turn={}, backends={}",
        config.enabled,
        config.timeout_ms,
        config.max_calls_per_turn,
        config.backends.len()
    );
    let redactor = if config.backends.is_empty() {
        None
    } else {
        env.store
            .as_ref()
            .ok()
            .and_then(|(store, _)| crate::serve::report_redactor(env.data_dir, *store).ok())
    };
    for backend in config.backends {
        let model = redactor.as_ref().map_or_else(
            || secret_store::REDACTED.to_owned(),
            |r| r.redact(&backend.model),
        );
        let _ = write!(
            detail,
            " | api={}, model={model}, rpm={}",
            backend.api.as_str(),
            backend.rpm
        );
    }
    if config.enabled {
        Check::ok(ID, TITLE, detail)
    } else {
        Check::skipped(ID, TITLE, detail)
    }
}

fn polish_provider_check(mut check: Check) -> Check {
    if check.status == Status::Fail && check.detail == "setup required: no enabled provider" {
        "setup required: no LLM provider is configured yet (chats cannot run without one)"
            .clone_into(&mut check.detail);
        check.hint = Some(
            "run `bot setup` for interactive setup, or `bot providers add <id>` to add a provider"
                .to_owned(),
        );
    }
    check
}

fn check_host_display(env: &DoctorEnv<'_>) -> Check {
    if !daemon_api::host::HostConfig::load(env.data_dir)
        .is_ok_and(|c| c.mode == daemon_api::host::HostMode::Host)
    {
        return Check::ok("host_display", "host display", "not in host mode");
    }
    let display = std::env::var("DISPLAY").ok();
    let xauthority = std::env::var("XAUTHORITY").ok();
    let xauth_readable = xauthority
        .as_deref()
        .is_some_and(|p| std::fs::File::open(p).is_ok());
    match diagnose_host_display(&HostDisplayFacts {
        display: display.as_deref(),
        xauthority: xauthority.as_deref(),
        xauthority_readable: xauth_readable,
        reachable: browser_cdp::host::has_display(),
        browser: env.browser.as_deref(),
        browser_error: env.browser_error.as_deref(),
    }) {
        Ok(detail) => Check::ok("host_display", "host display", detail),
        Err((detail, hint)) => Check::warn("host_display", "host display", detail, hint),
    }
}

/// What the host display check looked at (injected so tests need no X server).
struct HostDisplayFacts<'a> {
    display: Option<&'a str>,
    xauthority: Option<&'a str>,
    xauthority_readable: bool,
    reachable: bool,
    browser: Option<&'a Path>,
    browser_error: Option<&'a str>,
}

/// `Ok(detail)` when the desktop and the browser are usable, otherwise
/// `Err((what is missing, how to fix it))` naming the first real cause.
fn diagnose_host_display(
    f: &HostDisplayFacts<'_>,
) -> std::result::Result<String, (String, String)> {
    let run_in_service =
        "run `bot doctor` with the service's environment (see /etc/praxi/praxi.env)";
    if !browser_cdp::host::display_configured(f.display) {
        return Err((
            "DISPLAY is not set for this process: the bot cannot use its desktop".into(),
            format!(
                "the service needs DISPLAY=:99 (praxi-xvfb.service, /etc/praxi/praxi.env); {run_in_service}"
            ),
        ));
    }
    if !f.reachable {
        let xauth = match (f.xauthority, f.xauthority_readable) {
            (None, _) => "; XAUTHORITY is not set",
            (Some(_), false) => "; the XAUTHORITY file is missing or unreadable",
            (Some(_), true) => "",
        };
        return Err((
            format!(
                "display {} does not answer{xauth}",
                f.display.unwrap_or_default().trim()
            ),
            "check `systemctl status praxi-xvfb` and that the daemon user can read its XAUTHORITY file".into(),
        ));
    }
    if let Some(e) = f.browser_error {
        return Err((
            format!("display is reachable but the browser is unusable: {e}"),
            "install a non-snap Chrome/Chromium (e.g. /opt/praxi-browser/chrome) or set BOT_BROWSER_PATH".into(),
        ));
    }
    match f.browser {
        None => Err((
            "display is reachable but no browser was found".into(),
            "install a non-snap Chrome/Chromium (e.g. /opt/praxi-browser/chrome) or set BOT_BROWSER_PATH".into(),
        )),
        Some(p) => Ok(format!(
            "display {} reachable, browser {}; see browser launch check",
            f.display.unwrap_or_default().trim(),
            p.display()
        )),
    }
}

fn check_desktop_backend(env: &DoctorEnv<'_>) -> Check {
    if !daemon_api::host::HostConfig::load(env.data_dir)
        .is_ok_and(|c| c.mode == daemon_api::host::HostMode::Host)
    {
        return Check::ok("desktop_backend", "desktop backend", "not in host mode");
    }
    linux_desktop_backend_check()
}

/// Linux host: name the first broken piece of the X11 desktop backend.
#[cfg(target_os = "linux")]
fn linux_desktop_backend_check() -> Check {
    let display = std::env::var("DISPLAY").ok();
    let session = std::env::var("XDG_SESSION_TYPE").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let xauthority = std::env::var("XAUTHORITY").ok();
    let xauthority_readable = xauthority
        .as_deref()
        .is_some_and(|p| std::fs::File::open(p).is_ok());
    let xdpyinfo = command_on_path("xdpyinfo", std::env::var_os("PATH").as_deref(), &|p| {
        p.is_file()
    });
    let probe_x = session_problem(session.as_deref(), wayland.as_deref()).is_none()
        && display.as_deref().is_some_and(|d| !d.trim().is_empty())
        && xauthority.is_some()
        && xauthority_readable;
    let x_error = if probe_x {
        desktop_linux::probe_x_server(Duration::from_secs(2)).err()
    } else {
        None
    };
    finish_desktop_check(&DesktopFacts {
        session_type: session.as_deref(),
        wayland_display: wayland.as_deref(),
        display: display.as_deref(),
        xauthority: xauthority.as_deref(),
        xauthority_readable,
        x_error: x_error.as_deref(),
        x_probed: probe_x,
        xdpyinfo_present: xdpyinfo,
    })
}

/// The X11 probe is compiled only for the Linux host build.
#[cfg(not(target_os = "linux"))]
fn linux_desktop_backend_check() -> Check {
    Check::skipped(
        "desktop_backend",
        "desktop backend",
        "Linux X11 probe runs on the Linux host",
    )
}

/// Facts for [`diagnose_desktop_backend`]. The caller keeps the strings.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy)]
struct DesktopFacts<'a> {
    session_type: Option<&'a str>,
    wayland_display: Option<&'a str>,
    display: Option<&'a str>,
    xauthority: Option<&'a str>,
    xauthority_readable: bool,
    /// Set when a bounded X handshake was attempted and failed.
    x_error: Option<&'a str>,
    /// `true` when a handshake was attempted (`x_error` empty means it succeeded).
    x_probed: bool,
    xdpyinfo_present: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn finish_desktop_check(facts: &DesktopFacts<'_>) -> Check {
    match diagnose_desktop_backend(facts) {
        Ok(detail) => Check::ok("desktop_backend", "desktop backend", detail),
        Err((detail, hint)) => Check::warn("desktop_backend", "desktop backend", detail, hint),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn session_problem(session: Option<&str>, wayland: Option<&str>) -> Option<String> {
    let session = session.map(str::trim).filter(|s| !s.is_empty());
    let wayland = wayland.map(str::trim).filter(|s| !s.is_empty());
    if session.is_some_and(|s| s.eq_ignore_ascii_case("wayland")) {
        return Some("Wayland session (XDG_SESSION_TYPE=wayland)".into());
    }
    if let Some(name) = wayland
        && !session.is_some_and(|s| s.eq_ignore_ascii_case("x11"))
    {
        return Some(format!("Wayland display '{name}'"));
    }
    None
}

/// `Ok(detail)` when the Linux desktop backend can drive the screen.
/// The `Err` names the first real cause and how to fix it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn diagnose_desktop_backend(f: &DesktopFacts<'_>) -> std::result::Result<String, (String, String)> {
    if let Some(problem) = session_problem(f.session_type, f.wayland_display) {
        return Err((
            format!("{problem}: desktop control needs an X11 session such as Xvfb on :99"),
            "start praxi-xvfb and set XDG_SESSION_TYPE=x11 and DISPLAY=:99 in /etc/praxi/praxi.env"
                .into(),
        ));
    }
    let display = f.display.map(str::trim).filter(|s| !s.is_empty());
    let Some(display) = display else {
        return Err((
            "DISPLAY is not set: the desktop backend cannot open an X11 screen".into(),
            "the service needs DISPLAY=:99 (praxi-xvfb.service, /etc/praxi/praxi.env)".into(),
        ));
    };
    match f.xauthority {
        None => {
            return Err((
                format!(
                    "XAUTHORITY is not set for display {display}: the daemon cannot prove which X cookie to use"
                ),
                "set XAUTHORITY=/var/lib/praxi/.Xauthority (mode 0600, owned by the daemon user) in /etc/praxi/praxi.env"
                    .into(),
            ));
        }
        Some(path) if !f.xauthority_readable => {
            return Err((
                format!("XAUTHORITY file is missing or unreadable: {path}"),
                "check that the daemon user can read the cookie file, then restart praxi-xvfb"
                    .into(),
            ));
        }
        Some(_) => {}
    }
    if let Some(err) = f.x_error {
        return Err((
            err.to_string(),
            "check `systemctl status praxi-xvfb` and run `sudo praxi doctor` so the probe shares the Xvfb namespace"
                .into(),
        ));
    }
    if f.x_probed && !f.xdpyinfo_present {
        return Err((
            "helper `xdpyinfo` is missing".into(),
            "install x11-utils (apt) or xorg-x11-utils (dnf). Clicks and screenshots use in-process X11; xdotool, xclip, scrot and ImageMagick are unused"
                .into(),
        ));
    }
    let x_state = if f.x_probed {
        "X server reachable"
    } else {
        "X server not probed"
    };
    let helper = if f.xdpyinfo_present {
        "helper xdpyinfo present"
    } else {
        "helper xdpyinfo not checked"
    };
    Ok(format!(
        "session=x11; DISPLAY={display}; XAUTHORITY set; {x_state}; {helper}"
    ))
}

/// `true` when `name` is a file in a `PATH` entry. `exists` is injected so tests
/// do not touch the real filesystem layout beyond a temp dir.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn command_on_path(
    name: &str,
    path_env: Option<&std::ffi::OsStr>,
    exists: &dyn Fn(&Path) -> bool,
) -> bool {
    let Some(path_env) = path_env else {
        return false;
    };
    for dir in std::env::split_paths(path_env) {
        if exists(&dir.join(name)) {
            return true;
        }
    }
    false
}

fn check_free_ram() -> Check {
    #[cfg(target_os = "linux")]
    {
        let available = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|l| l.starts_with("MemAvailable:"))
                    .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
            });
        if let Some(kib) = available {
            let mib = kib / 1024;
            return if mib < 512 {
                Check::warn(
                    "free_ram",
                    "free RAM",
                    format!("{mib} MiB available"),
                    "stop unused processes or lower limits.max_computer_runs in host.toml",
                )
            } else {
                Check::ok("free_ram", "free RAM", format!("{mib} MiB available"))
            };
        }
    }
    Check::warn(
        "free_ram",
        "free RAM",
        "available memory is not exposed by this platform probe",
        "check available RAM with the operating system monitor",
    )
}

fn check_peer_relay(env: &DoctorEnv<'_>) -> Check {
    if !env.online {
        return Check::warn(
            "peer_relay",
            "peer relay",
            "external reachability not probed",
            "run `bot doctor --online` to probe the daemon through its advertised relay",
        );
    }
    let probe = || -> Result<()> {
        let token = daemon_api::read_token(env.data_dir)?;
        let rt = crate::current_thread_runtime()?;
        rt.block_on(async {
            let client =
                crate::client::Client::new(&format!("http://127.0.0.1:{}", env.port), token);
            let status = client
                .request(reqwest::Method::GET, "/v1/host", None)
                .await?;
            let key = peer_network::key_from_string(
                status["endpoint_id"]
                    .as_str()
                    .ok_or("peer endpoint disabled")?,
            )?;
            let peer_network::RelayConfig::Custom(relay) = status["relay_url"]
                .as_str()
                .ok_or("no relay advertised")?
                .parse::<peer_network::RelayConfig>()?
            else {
                return Err("invalid advertised relay".into());
            };
            // A throwaway identity cannot redeem invites. An authenticated pairing
            // rejection proves the endpoint answers via the advertised relay route.
            let dir = tempfile::tempdir()?;
            let mut config = peer_network::PeerConfig::new(dir.path().to_path_buf());
            config.local_role = peer_network::Role::Node;
            config.relay = peer_network::RelayConfig::Custom(relay.clone());
            let net = peer_network::PeerNetwork::start(config, None).await?;
            let ticket = peer_network::PairTicket {
                addr: key.into(),
                secret: "0".repeat(64),
                code: "000000".into(),
                role: peer_network::Role::Node,
            };
            let ticket = peer_network::PairTicket {
                addr: ticket.addr.with_relay_url(relay),
                ..ticket
            };
            let result = tokio::time::timeout(Duration::from_secs(15), net.pair(&ticket)).await;
            net.shutdown().await;
            match result {
                Ok(Err(peer_network::PeerError::PairingRejected(_))) => Ok(()),
                _ => Err("endpoint did not answer the relay probe".into()),
            }
        })
    };
    match probe() {
        Ok(()) => Check::ok(
            "peer_relay",
            "peer relay",
            "daemon pairing endpoint answered using its advertised relay",
        ),
        Err(e) => Check::warn(
            "peer_relay",
            "peer relay",
            e.to_string(),
            "check outbound relay access and run `bot serve --peer`; no inbound port is required",
        ),
    }
}

fn check_local_models(data_dir: &Path) -> Check {
    let hw = provider_catalog::probe_hardware(data_dir);
    let advice = provider_catalog::recommend(&hw, &provider_catalog::catalog_candidates());
    core::check_local_models(&hw, &advice)
}

fn check_resources() -> Check {
    let bytes = browser_cdp::host::total_ram_bytes();
    let mut detail = format!(
        "{} MiB physical RAM; default computer-run cap {}",
        bytes / (1024 * 1024),
        browser_cdp::host::default_computer_runs(bytes)
    );
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        use std::fmt::Write;
        for line in status
            .lines()
            .filter(|l| l.starts_with("VmRSS:") || l.starts_with("VmHWM:"))
        {
            let _ = write!(detail, "; {line}");
        }
    }
    detail.push_str("; doctor process only; measure the complete daemon + Xvfb + Chromium process tree on the host");
    Check::ok("resources", "resources", detail)
}

fn online_test(
    p: &provider_catalog::ProviderSettings,
    entry: &provider_catalog::ProviderEntry,
    store: &dyn SecretStore,
) -> std::result::Result<usize, String> {
    let provider = p
        .build_provider(entry, store, OpenAiCompatConfig::default())
        .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let model = p.model_ref();
    rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(20),
            test_connection(&*provider, model.as_ref()),
        )
        .await
    })
    .map_err(|_| "timed out".to_string())?
    .map(|r| r.models.len())
    .map_err(|e| e.to_string())
}

fn check_daemon(dir: &Path, port: u16) -> Check {
    const ID: &str = "daemon";
    const N: &str = "daemon";
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let token = daemon_api::token_path(dir);
    let token_note = match daemon_api::read_token(dir) {
        Ok(_) => format!("token file ok ({})", token.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "no token file yet".to_string(),
        Err(e) => format!("token file unreadable: {e}"),
    };
    let token_bad = token.exists() && daemon_api::read_token(dir).is_err();
    let up = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok();
    if !up {
        return Check::warn(
            ID,
            N,
            format!("not running on port {port}; {token_note}"),
            "start the daemon with `bot serve`",
        );
    }
    let health = daemon_health(port);
    match (health, token_bad) {
        (Some(v), false) => Check::ok(
            ID,
            N,
            format!("running on port {port} (v{v}); {token_note}"),
        ),
        (Some(_), true) => Check::fail(
            ID,
            N,
            format!("running on port {port}; {token_note}"),
            "delete the api-token file and restart `bot serve`",
        ),
        (None, _) => Check::fail(
            ID,
            N,
            format!("port {port} is in use but does not answer like the daemon"),
            "stop the other process or pass `--port` / `BOT_PORT`",
        ),
    }
}

/// Whether the daemon answering on `port` runs in safe mode (`false` when none answers).
fn daemon_safe_mode(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err() {
        return false;
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    rt.block_on(async {
        let Ok(resp) = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/health"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        else {
            return false;
        };
        resp.json::<Value>()
            .await
            .is_ok_and(|v| v["safe_mode"] == true)
    })
}

/// Version string from `GET /v1/health`, `None` if it is not our daemon.
fn daemon_health(port: u16) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/health"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .ok()?;
        let v: Value = resp.json().await.ok()?;
        (v["status"] == "ok" || v["status"] == "setup_required")
            .then(|| v["version"].as_str().unwrap_or("?").to_string())
    })
}

fn check_browser(found: Option<&Path>) -> Check {
    const ID: &str = "browser";
    const N: &str = "browser";
    match found {
        Some(p) => {
            if browser_cdp::launch::is_snap_browser(p) {
                return Check::fail(
                    ID,
                    N,
                    "snap browser is unsupported under the hardened service",
                    "install non-snap Chrome/Chromium or set BOT_BROWSER_PATH",
                );
            }
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(async {
                        let opts = browser_cdp::launch::LaunchOptions {
                            executable: Some(p.to_path_buf()),
                            startup_timeout: Duration::from_secs(5),
                            ..Default::default()
                        };
                        let probe = async {
                            let browser = browser_cdp::Browser::launch(opts).await?;
                            browser.close().await
                        };
                        tokio::time::timeout(Duration::from_secs(8), probe)
                            .await
                            .map_err(|_| "headless launch timed out".to_string())?
                            .map_err(|e| e.to_string())
                    })
                });
            match result {
                Ok(()) => Check::ok(
                    ID,
                    N,
                    format!(
                        "{} (headless launch passed in current user/env)",
                        p.display()
                    ),
                ),
                Err(e) => Check::fail(
                    ID,
                    N,
                    format!("{}: {e}", p.display()),
                    "install a non-snap browser or set BOT_BROWSER_PATH; on systemd use `sudo praxi doctor` to probe with the service user, environment and hardening",
                ),
            }
        }
        None => Check::warn(
            ID,
            N,
            "no usable non-snap browser found (snap-only installs are unsupported)",
            "install a non-snap Chrome/Chromium or set BOT_BROWSER_PATH",
        ),
    }
}

fn check_mcp(dir: &Path) -> Check {
    const ID: &str = "mcp_config";
    const N: &str = "mcp config";
    let path = dir.join(MCP_FILE);
    if !path.exists() {
        return Check::ok(ID, N, "no MCP servers configured");
    }
    match TomlFileStore::new(&path).load() {
        Ok(s) => Check::ok(ID, N, format!("{} server(s)", s.len())),
        Err(e) => Check::fail(ID, N, e.to_string(), format!("fix {}", path.display())),
    }
}

/// `secret_request` is experimental and off by default until the desktop form exists.
fn check_secret_request(dir: &Path) -> Check {
    let host = std::fs::read_to_string(dir.join("host.toml")).unwrap_or_default();
    let env = std::env::var("BOT_SECRET_REQUESTS").ok();
    if crate::serve::secret_requests_enabled(&host, env.as_deref()) {
        Check::warn(
            "secret_request",
            "secret request (experimental)",
            "on: the bot may ask for a credential; the desktop form is not available yet, so a request waits until it expires",
            "turn it off: remove `secret_request = true` under [experimental] in host.toml and unset BOT_SECRET_REQUESTS",
        )
    } else {
        Check::ok(
            "secret_request",
            "secret request (experimental)",
            "off (default): the bot is not offered the secret_request tool",
        )
    }
}

/// Budget for the `--online` web search probe: must exceed the sequential
/// worst-case of per-backend search timeouts (including redirect hops) so
/// later fallbacks (such as Bing) run and structured backend errors surface
/// instead of an outer timeout.
fn web_probe_timeout(cfg: &web_tools::WebToolsConfig, backend_count: usize) -> Duration {
    let per_hop = cfg.http.timeout.min(web_tools::DEFAULT_SEARCH_TIMEOUT);
    let hops_per_backend =
        u32::try_from(cfg.http.max_redirects.saturating_add(1).max(1)).unwrap_or(u32::MAX);
    let count = u32::try_from(backend_count.max(1)).unwrap_or(u32::MAX);
    per_hop
        .saturating_mul(hops_per_backend)
        .saturating_mul(count)
        .saturating_add(Duration::from_secs(5))
}

/// `web tools`: which search backends are configured, and with `--online`
/// whether a live search answers. A failing probe is a warning, never a fail:
/// the bot still works without web search.
fn check_web(env: &DoctorEnv<'_>) -> Check {
    const ID: &str = "web_search";
    const N: &str = "web search";
    let (file, mut notes) = crate::serve::load_web_file(env.data_dir);
    // The doctor holds the store by reference only: copy the Brave / Tavily keys
    // into a throwaway in-memory store for the probe.
    let mem: std::sync::Arc<dyn SecretStore> =
        std::sync::Arc::new(secret_store::MemoryStore::new());
    let mut copy_secret = |label: &str, key_name: Option<&str>| {
        if let (Some(name), Ok((s, _))) = (key_name, &env.store) {
            match s.get(name) {
                Ok(Some(v)) => {
                    let _ = mem.set(name, v.expose());
                }
                _ => notes.push(format!("{label} key secret not found")),
            }
        }
    };
    copy_secret("brave", file.search.brave_key.as_deref());
    copy_secret("tavily", file.search.tavily_key.as_deref());
    let cfg = crate::serve::web_tools_config(
        env.data_dir,
        &file,
        Some(&mem),
        &crate::serve::daemon_protected_sockets(env.port),
        &mut notes,
    );
    let backends = crate::serve::web_backend_summary(&cfg);
    let detail = |extra: String| {
        let mut d = format!("backends: {backends}");
        if !extra.is_empty() {
            d.push_str("; ");
            d.push_str(&extra);
        }
        if !notes.is_empty() {
            d.push_str("; ");
            d.push_str(&notes.join("; "));
        }
        d
    };
    let hint = "configure Brave, Tavily, or SearXNG in Settings -> Web search or <data_dir>/web.toml (see docs/guide/web.md)";
    if !env.online {
        let c = Check::ok(ID, N, detail("not probed".into()));
        return if notes.is_empty() {
            c.hint("add --online to test that a search answers")
        } else {
            Check {
                status: Status::Warn,
                ..c
            }
            .hint(hint)
        };
    }
    match probe_web_search(&cfg) {
        Ok(backend) => {
            let c = Check::ok(ID, N, detail(format!("{backend} answered")));
            if notes.is_empty() {
                c
            } else {
                Check {
                    status: Status::Warn,
                    ..c
                }
                .hint(hint)
            }
        }
        Err(e) => Check::warn(ID, N, detail(format!("no answer: {e}")), hint),
    }
}

/// Run one live search through the configured backends; the answering backend's name.
fn probe_web_search(cfg: &web_tools::WebToolsConfig) -> std::result::Result<String, String> {
    let reg = tool_registry::ToolRegistry::new();
    web_tools::register(&reg, cfg.clone()).map_err(|e| e.to_string())?;
    let tool = reg.get("web_search").ok_or("web_search not registered")?;
    let backend_count = tool.spec().input_schema["properties"]["backend"]["enum"]
        .as_array()
        .map_or(0, Vec::len);
    let timeout = web_probe_timeout(cfg, backend_count);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let ctx = tool_registry::ToolContext::default();
        let call = tool.call(&ctx, json!({"query": "praxi", "limit": 1}));
        let out = tokio::time::timeout(timeout, call)
            .await
            .map_err(|_| "timed out".to_string())?
            .map_err(|e| e.to_string())?;
        out.content
            .iter()
            .find_map(|p| match p {
                tool_registry::ContentPart::Json { data } => {
                    data["backend"].as_str().map(str::to_owned)
                }
                _ => None,
            })
            .ok_or_else(|| "unexpected search output".to_string())
    })
}

fn check_telegram(env: &DoctorEnv<'_>) -> Check {
    const ID: &str = "telegram";
    const N: &str = "telegram";
    let path = env.data_dir.join(crate::channels::CHANNELS_FILE);
    if path.exists()
        && std::fs::read_to_string(&path).map_or(true, |s| s.parse::<toml::Table>().is_err())
    {
        return Check::fail(
            ID,
            N,
            "channels.toml does not parse",
            "delete channels.toml and run `bot channels telegram setup`",
        );
    }
    let enabled = crate::channels::telegram_enabled(env.data_dir);
    let configured = match &env.store {
        Ok((s, _)) => crate::channels::telegram_configured(*s).ok(),
        Err(_) => None,
    };
    match (configured, enabled) {
        (None, _) => Check::warn(
            ID,
            N,
            "cannot read the bot token (secret store unavailable)",
            "see the secret backend check",
        ),
        (Some(false), _) => Check::ok(ID, N, "not configured (optional)"),
        (Some(true), true) => {
            let paired = crate::channels::paired_users_on_disk(env.data_dir);
            if paired == 0 {
                Check::warn(
                    ID,
                    N,
                    "token stored, enabled, nobody paired",
                    "run `bot channels telegram pair` and send /pair to the bot",
                )
            } else {
                Check::ok(ID, N, format!("enabled, {paired} paired user(s)"))
            }
        }
        (Some(true), false) => Check::ok(ID, N, "token stored, channel switched off"),
    }
}

/// Pattern masking only (no secret list): the home directory, e-mail
/// addresses, user / host names and anything that looks like a key or token.
#[must_use]
#[cfg(test)]
pub fn redact(s: &str) -> String {
    let home = core::home_dir();
    core::redact_report(
        s,
        &core::Scrub {
            secrets: None,
            home: home.as_deref(),
            identities: core::machine_identities(),
            known: core::KnownIds::default(),
        },
    )
}

/// A redacted diagnostic bundle: versions, OS, check results and provider
/// ids. No keys, tokens, secret names or file contents.  `secrets` is the
/// production redactor's secret list (see [`crate::serve::report_redactor`]);
/// model and provider ids from the catalog and the config stay readable.
#[must_use]
pub fn report_bundle(
    env: &DoctorEnv<'_>,
    checks: &[Check],
    secrets: &agent_core::SharedRedactor,
) -> Value {
    let providers: Vec<Value> = ProviderConfig::load(&config_path(env.data_dir))
        .map(|c| {
            c.providers
                .iter()
                .map(|p| {
                    json!({
                        "id": p.id,
                        "enabled": p.enabled,
                        "has_key_ref": p.key.is_some(),
                        "default_model": p.default_model,
                        "custom": p.custom.is_some(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut v = json!({
        "bot_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "data_dir": env.data_dir.display().to_string(),
        "port": env.port,
        "online": env.online,
        "providers": providers,
        "doctor": to_json(checks),
    });
    let home = core::home_dir();
    let redact = |t: &str| secrets.redact(t);
    let scrub = core::Scrub {
        secrets: Some(&redact),
        home: home.as_deref(),
        identities: core::machine_identities(),
        known: core::KnownIds::collect(env.data_dir, env.catalog.as_ref().ok().copied()),
    };
    redact_value(&mut v, &scrub);
    v
}

fn redact_value(v: &mut Value, scrub: &core::Scrub<'_>) {
    match v {
        Value::String(s) => *s = core::redact_report(s, scrub),
        Value::Array(a) => a.iter_mut().for_each(|x| redact_value(x, scrub)),
        Value::Object(o) => o.values_mut().for_each(|x| redact_value(x, scrub)),
        _ => {}
    }
}

/// Write the report bundle to `path`, redacted with the same secret list as
/// the daemon's support report.  Refuses (and writes nothing) when the
/// secret store cannot be opened or read: without the secret list the report
/// could carry a stored secret in clear.
///
/// # Errors
/// The secret store is unavailable, or the file cannot be written.
pub fn write_report(env: &DoctorEnv<'_>, checks: &[Check], path: &Path) -> Result<()> {
    let store = env.store.as_ref().map_err(|e| {
        format!(
            "not writing the report: the secret store cannot be opened ({e}), so stored secrets could not be masked; fix the secret backend (see the secret backend check) and try again"
        )
    })?;
    let secrets = crate::serve::report_redactor(env.data_dir, store.0).map_err(|e| {
        format!(
            "not writing the report: the secret store cannot be read ({e}), so stored secrets could not be masked; fix the secret backend (see the secret backend check) and try again"
        )
    })?;
    std::fs::write(
        path,
        serde_json::to_string_pretty(&report_bundle(env, checks, &secrets))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_catalog::ProviderSettings;

    #[test]
    fn host_display_check_names_the_first_missing_piece() {
        let base = HostDisplayFacts {
            display: Some(":99"),
            xauthority: Some("/x"),
            xauthority_readable: true,
            reachable: true,
            browser: Some(Path::new("/opt/praxi-browser/chrome")),
            browser_error: None,
        };
        assert!(diagnose_host_display(&base).unwrap().contains(":99"));
        let (d, _) = diagnose_host_display(&HostDisplayFacts {
            display: None,
            ..base
        })
        .unwrap_err();
        assert!(d.contains("DISPLAY is not set"), "{d}");
        let (d, _) = diagnose_host_display(&HostDisplayFacts {
            reachable: false,
            xauthority_readable: false,
            ..base
        })
        .unwrap_err();
        assert!(
            d.contains("does not answer") && d.contains("XAUTHORITY"),
            "{d}"
        );
        let (d, _) = diagnose_host_display(&HostDisplayFacts {
            browser: None,
            ..base
        })
        .unwrap_err();
        assert!(d.contains("no browser"), "{d}");
    }

    #[test]
    fn desktop_backend_names_display_auth_session_and_helper() {
        let ok = DesktopFacts {
            session_type: Some("x11"),
            wayland_display: None,
            display: Some(":99"),
            xauthority: Some("/var/lib/praxi/.Xauthority"),
            xauthority_readable: true,
            x_error: None,
            x_probed: true,
            xdpyinfo_present: true,
        };
        let ready = diagnose_desktop_backend(&ok).unwrap();
        assert!(ready.contains("DISPLAY=:99"), "{ready}");
        assert!(ready.contains("X server reachable"), "{ready}");

        let (d, _) = diagnose_desktop_backend(&DesktopFacts {
            session_type: Some("wayland"),
            ..ok
        })
        .unwrap_err();
        assert!(d.contains("Wayland"), "{d}");

        let (d, _) = diagnose_desktop_backend(&DesktopFacts {
            display: None,
            ..ok
        })
        .unwrap_err();
        assert!(d.contains("DISPLAY is not set"), "{d}");

        let (d, _) = diagnose_desktop_backend(&DesktopFacts {
            xauthority: None,
            ..ok
        })
        .unwrap_err();
        assert!(d.contains("XAUTHORITY is not set"), "{d}");

        let (d, _) = diagnose_desktop_backend(&DesktopFacts {
            xauthority_readable: false,
            ..ok
        })
        .unwrap_err();
        assert!(d.contains("missing or unreadable"), "{d}");

        let (d, _) = diagnose_desktop_backend(&DesktopFacts {
            x_error: Some("X server not reachable at :99: connection refused"),
            ..ok
        })
        .unwrap_err();
        assert!(d.contains("X server not reachable at :99"), "{d}");

        let (d, hint) = diagnose_desktop_backend(&DesktopFacts {
            xdpyinfo_present: false,
            ..ok
        })
        .unwrap_err();
        assert!(d.contains("helper `xdpyinfo` is missing"), "{d}");
        assert!(hint.contains("xdotool"), "{hint}");

        assert!(command_on_path(
            "xdpyinfo",
            Some(std::ffi::OsStr::new("/usr/bin")),
            &|p| p == Path::new("/usr/bin/xdpyinfo")
        ));
        assert!(!command_on_path("xdpyinfo", None, &|_| true));
    }

    use secret_store::{SecretRef, SecretValue};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore(Mutex<BTreeMap<String, String>>);

    impl SecretStore for MemStore {
        fn get(
            &self,
            name: &str,
        ) -> std::result::Result<Option<SecretValue>, secret_store::SecretError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(name)
                .map(|v| SecretValue::new(v.clone())))
        }
        fn set(
            &self,
            name: &str,
            value: &str,
        ) -> std::result::Result<(), secret_store::SecretError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> std::result::Result<bool, secret_store::SecretError> {
            Ok(self.0.lock().unwrap().remove(name).is_some())
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
    }

    fn catalog() -> Catalog {
        let src = r#"
id = "alpha"
kind = "open_ai_compatible"
base_url = "http://127.0.0.1:9/v1"
auth = { ApiKeyEnv = "ALPHA_KEY_DOCTOR_TEST" }
[capabilities]
tools = true
vision = false
streaming = true
json_mode = false
[[models]]
id = "m1"
"#;
        Catalog::from_entries([toml::from_str(src).unwrap()])
    }

    fn get<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks.iter().find(|c| c.title == name).unwrap()
    }

    fn env<'a>(dir: &'a Path, cat: &'a Catalog, store: &'a MemStore, port: u16) -> DoctorEnv<'a> {
        DoctorEnv {
            data_dir: dir,
            catalog: Ok(cat),
            store: Ok((store, "Memory".into())),
            port,
            online: false,
            browser: None,
            browser_error: None,
            detected: None,
            desktop: None,
            time_url: None,
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn empty_data_dir_warns_but_nothing_panics() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let checks = run_checks(&env(&d.path().join("nope"), &cat, &store, free_port()));
        assert_eq!(get(&checks, "data dir").status, Status::Warn);
        assert_eq!(get(&checks, "config").status, Status::Warn);
        assert_eq!(get(&checks, "providers").status, Status::Fail);
        assert_eq!(get(&checks, "daemon").status, Status::Warn);
        assert_eq!(get(&checks, "browser").status, Status::Warn);
        assert_eq!(get(&checks, "mcp config").status, Status::Ok);
        assert_eq!(get(&checks, "process guard").status, Status::Skipped);
        assert_eq!(get(&checks, "telegram").status, Status::Ok);
        assert_eq!(get(&checks, "model catalog").status, Status::Ok);
        assert!(
            checks
                .iter()
                .all(|c| matches!(c.status, Status::Ok | Status::Skipped) || c.hint.is_some())
        );
    }

    #[test]
    fn healthy_setup_passes() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        store.set("provider.alpha.api_key", "sk-secret").unwrap();
        let mut s = ProviderSettings::new("alpha", 1);
        s.key = Some(SecretRef::new("provider.alpha.api_key").unwrap());
        s.default_model = Some("m1".into());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(s);
        cfg.save(&config_path(d.path())).unwrap();
        let mut e = env(d.path(), &cat, &store, free_port());
        e.browser = None;
        let checks = run_checks(&e);
        for n in [
            "data dir",
            "config",
            "secret backend",
            "providers",
            "model catalog",
            "mcp config",
        ] {
            assert_eq!(
                get(&checks, n).status,
                Status::Ok,
                "{n}: {}",
                get(&checks, n).detail
            );
        }
        let dd = get(&checks, "data dir");
        assert!(
            dd.detail.contains("writable") && dd.detail.contains("MiB free"),
            "{}",
            dd.detail
        );

        let file_path = d.path().join("not-a-directory");
        std::fs::write(&file_path, b"file").unwrap();
        let file_checks = run_checks(&env(&file_path, &cat, &store, free_port()));
        let bad_dd = get(&file_checks, "data dir");
        assert_eq!(bad_dd.status, Status::Fail, "{}", bad_dd.detail);
        assert!(
            bad_dd.detail.contains("is not a directory"),
            "{}",
            bad_dd.detail
        );
    }

    #[test]
    fn missing_key_and_bad_files_fail_with_hints() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let mut s = ProviderSettings::new("alpha", 1);
        s.key = Some(SecretRef::new("provider.alpha.api_key").unwrap());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(s);
        cfg.save(&config_path(d.path())).unwrap();
        std::fs::write(d.path().join(MCP_FILE), "[[server]\nbroken").unwrap();
        std::fs::write(d.path().join(crate::channels::CHANNELS_FILE), "= nope").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        for n in ["providers", "mcp config", "telegram"] {
            let c = get(&checks, n);
            assert_eq!(c.status, Status::Fail, "{n}");
            assert!(c.hint.is_some());
        }
        std::fs::write(config_path(d.path()), "not [valid").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        assert_eq!(get(&checks, "config").status, Status::Fail);
    }

    #[test]
    fn unusable_browser_discovery_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let mut e = env(d.path(), &cat, &store, free_port());
        e.browser_error = Some("only snap browsers found; unsupported".into());
        let checks = run_checks(&e);
        assert_eq!(get(&checks, "browser").status, Status::Fail);
        assert!(
            get(&checks, "browser")
                .hint
                .as_ref()
                .unwrap()
                .contains("non-snap")
        );
    }

    #[test]
    fn browser_presence_does_not_imply_it_can_launch() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("broken-browser");
        std::fs::write(&path, b"not an executable").unwrap();
        let check = check_browser(Some(&path));
        assert_eq!(check.status, Status::Fail);
        assert!(check.hint.unwrap().contains("non-snap"));
    }

    #[test]
    fn foreign_listener_on_port_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Accept and drop connections so the health probe fails fast.
        std::thread::spawn(move || {
            for s in l.incoming() {
                drop(s);
            }
        });
        let c = check_daemon(d.path(), port);
        assert_eq!(c.status, Status::Fail);
    }

    #[test]
    fn secret_store_error_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let cat = catalog();
        let e = DoctorEnv {
            data_dir: d.path(),
            catalog: Ok(&cat),
            store: Err("passphrase needed".into()),
            port: free_port(),
            online: false,
            browser: None,
            browser_error: None,
            detected: None,
            desktop: None,
            time_url: None,
        };
        let checks = run_checks(&e);
        assert_eq!(get(&checks, "secret backend").status, Status::Fail);
        assert_eq!(get(&checks, "telegram").status, Status::Warn);
    }

    struct FailingStore;

    impl SecretStore for FailingStore {
        fn get(
            &self,
            _: &str,
        ) -> std::result::Result<Option<SecretValue>, secret_store::SecretError> {
            Err(secret_store::SecretError::Unavailable("injected".into()))
        }
        fn set(&self, _: &str, _: &str) -> std::result::Result<(), secret_store::SecretError> {
            Err(secret_store::SecretError::Unavailable("injected".into()))
        }
        fn delete(&self, _: &str) -> std::result::Result<bool, secret_store::SecretError> {
            Err(secret_store::SecretError::Unavailable("injected".into()))
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            Err(secret_store::SecretError::Unavailable("injected".into()))
        }
    }

    #[test]
    fn report_is_refused_when_the_secrets_cannot_be_read() {
        let d = tempfile::tempdir().unwrap();
        let cat = catalog();
        let path = d.path().join("report.json");
        // The store does not open.
        let mut e = DoctorEnv {
            data_dir: d.path(),
            catalog: Ok(&cat),
            store: Err("passphrase needed".into()),
            port: free_port(),
            online: false,
            browser: None,
            browser_error: None,
            detected: None,
            desktop: None,
            time_url: None,
        };
        let checks = run_checks(&e);
        let err = write_report(&e, &checks, &path).unwrap_err().to_string();
        assert!(err.contains("not writing the report"), "{err}");
        assert!(!path.exists());
        // The store opens, but cannot be read.
        let failing = FailingStore;
        e.store = Ok((&failing, "failing".into()));
        let err = write_report(&e, &checks, &path).unwrap_err().to_string();
        assert!(err.contains("cannot be read"), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn redaction_masks_tokens_and_home() {
        let tok = "a".repeat(64);
        let r = redact(&format!(
            "key sk-0123456789abcdef0123456789abcdef and {tok} ok short-id"
        ));
        assert!(!r.contains("0123456789abcdef"), "{r}");
        assert!(!r.contains(&tok));
        assert!(r.contains("short-id"));
    }

    /// A stored secret no pattern recognises: short segments, no key shape,
    /// no percent-escape.
    const PLAIN_SECRET: &str = "vault pass 9|9";

    #[test]
    fn report_has_no_secrets() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        store
            .set(
                "provider.alpha.api_key",
                "sk-supersecretvalue-0123456789abcdef",
            )
            .unwrap();
        store.set("vault.note", PLAIN_SECRET).unwrap();
        std::fs::write(daemon_api::token_path(d.path()), "b".repeat(64)).unwrap();
        let mut s = ProviderSettings::new("alpha", 1);
        s.key = Some(SecretRef::new("provider.alpha.api_key").unwrap());
        let mut cfg = ProviderConfig::default();
        cfg.upsert(s);
        // A provider id equal to a stored secret: copied into the provider
        // list and the check details; only the secret list can mask it.
        cfg.upsert(ProviderSettings::new(PLAIN_SECRET, 2));
        cfg.save(&config_path(d.path())).unwrap();
        // The patterns alone do not catch it.
        assert!(redact(PLAIN_SECRET).contains("vault pass"));
        let e = env(d.path(), &cat, &store, free_port());
        let checks = run_checks(&e);
        let path = d.path().join("report.json");
        write_report(&e, &checks, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("supersecret"));
        assert!(!text.contains("vault pass"), "{text}");
        assert!(!text.contains(&"b".repeat(64)));
        assert!(!text.contains("provider.alpha.api_key"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["providers"][0]["id"], "alpha");
        assert!(v["doctor"]["checks"].is_array());
    }

    #[test]
    fn table_and_json_render() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let mut buf = Vec::new();
        render_table(&checks, &mut buf).unwrap();
        let t = String::from_utf8(buf).unwrap();
        assert!(t.contains("daemon") && t.contains("fix:") && t.contains("warning(s)"));
        let j = to_json(&checks);
        assert_eq!(j["checks"].as_array().unwrap().len(), checks.len());
        assert_eq!(j["ok"], false);
    }

    #[test]
    fn web_check_reports_backends_without_touching_the_network() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let c = get(&checks, "web search");
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.contains("duckduckgo"), "{}", c.detail);
        assert!(c.detail.contains("not probed"), "{}", c.detail);

        std::fs::write(d.path().join(crate::serve::WEB_FILE), "= nonsense").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let c = get(&checks, "web search");
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("web.toml"), "{}", c.detail);
    }

    #[test]
    fn doctor_reports_a_broken_schedule_toml_as_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let (cat, store) = (catalog(), MemStore::default());
        let c = get(
            &run_checks(&env(d.path(), &cat, &store, free_port())),
            "schedule tools",
        )
        .clone();
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        std::fs::write(d.path().join("schedule.toml"), "min_interval_secs = 0").unwrap();
        let checks = run_checks(&env(d.path(), &cat, &store, free_port()));
        let c = get(&checks, "schedule tools");
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("disabled"), "{}", c.detail);
    }

    #[test]
    fn web_check_online_answers_or_warns_never_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let ok = rt.block_on(async {
            let s = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"results": [{"title": "T", "url": "https://example.org/", "content": "c"}]}),
                ))
                .mount(&s)
                .await;
            s
        });
        let broken = rt.block_on(async {
            let s = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&s)
                .await;
            s
        });
        let (cat, store) = (catalog(), MemStore::default());
        for (server, status, word) in [
            (&ok, Status::Ok, "searxng answered"),
            (&broken, Status::Warn, "no answer"),
        ] {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(
                d.path().join(crate::serve::WEB_FILE),
                format!(
                    "[search]
searxng_url = \"{}\"
duckduckgo = false
duckduckgo_lite = false
bing = false
",
                    server.uri()
                ),
            )
            .unwrap();
            let mut e = env(d.path(), &cat, &store, free_port());
            e.online = true;
            let checks = run_checks(&e);
            let c = get(&checks, "web search");
            assert_eq!(c.status, status, "{}", c.detail);
            assert!(c.detail.contains(word), "{}", c.detail);
        }
    }

    #[test]
    fn web_probe_timeout_covers_all_backends_and_redirects_to_reach_fallback() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let def = web_tools::WebToolsConfig::default();
        let expected_floor = web_tools::DEFAULT_SEARCH_TIMEOUT
            .saturating_mul(u32::try_from(def.http.max_redirects + 1).unwrap())
            .saturating_mul(3);
        assert!(
            web_probe_timeout(&def, 3) > expected_floor,
            "probe budget must exceed enabled_backends * DEFAULT_SEARCH_TIMEOUT * (max_redirects + 1)"
        );

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(async {
            let s = MockServer::start().await;
            // First two keyless backends stall past per-backend timeout (60ms);
            // the third (bing) answers cleanly.
            Mock::given(method("GET"))
                .and(path("/html/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_millis(200))
                        .set_body_string("<html></html>"),
                )
                .mount(&s)
                .await;
            Mock::given(method("GET"))
                .and(path("/lite/"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_millis(200))
                        .set_body_string("<html></html>"),
                )
                .mount(&s)
                .await;
            Mock::given(method("GET"))
                .and(path("/search"))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    b"<ol id=\"b_results\"><li class=\"b_algo\"><h2><a href=\"https://example.org/\">Hit</a></h2></li></ol>".to_vec(),
                    "text/html",
                ))
                .mount(&s)
                .await;
            s
        });
        let mut cfg = web_tools::WebToolsConfig {
            policy: std::sync::Arc::new(web_tools::DefaultUrlPolicy {
                block_loopback: false,
                ..web_tools::DefaultUrlPolicy::default()
            }),
            ..web_tools::WebToolsConfig::default()
        };
        cfg.http.timeout = Duration::from_millis(60);
        cfg.http.max_redirects = 0;
        cfg.search.duckduckgo_endpoint = Some(format!("{}/html/", server.uri()));
        cfg.search.duckduckgo_lite_endpoint = Some(format!("{}/lite/", server.uri()));
        cfg.search.bing_endpoint = Some(format!("{}/search", server.uri()));
        let answered = probe_web_search(&cfg).expect("bing fallback should answer");
        assert_eq!(answered, "bing");
    }
}
