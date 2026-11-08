//! Command-line interface entry point for the Praxi-Bot agent (`bot`).

mod channels;
#[cfg(test)]
mod channels_tests;
mod client;
mod commands;
mod doctor;
mod mcp_host;
mod peer;
mod presets;
mod prompt;
mod remote;
mod serve;
mod wizard;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use provider_catalog::{Catalog, ProviderEntry};
use secret_store::{BackendChoice, SecretStore, SecretValue, open_store};

use client::{Client, ExitError};
use presets::{BotPreset, Lang, PermissionPreset};
use prompt::{TerminalPrompter, ask_passphrase};
use wizard::{NetConnector, ProviderChoice, SetupArgs, SetupEnv};

/// Boxed error used across the CLI.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Praxi-Bot command-line interface.
#[derive(Parser)]
#[command(name = "bot", version, about = "Praxi-Bot agent CLI", long_about = None)]
struct Cli {
    /// Data directory (default: `BOT_DATA_DIR` or the OS application data dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Directory with provider catalog `*.toml` files (default: built-in set).
    #[arg(long, global = true, env = "BOT_CATALOG_DIR")]
    catalog_dir: Option<PathBuf>,
    /// Secret backend: auto (existing file, else OS keychain, else encrypted
    /// file), keyring, or file. The file passphrase comes from
    /// `BOT_SECRETS_PASSPHRASE` or a prompt.
    #[arg(
        long,
        global = true,
        default_value = "auto",
        env = "BOT_SECRETS_BACKEND"
    )]
    secrets_backend: BackendChoice,
    /// Machine-readable output (JSON / NDJSON on stdout) for status, chat,
    /// bots and approve. Errors always go to stderr as text.
    #[arg(long, global = true)]
    json: bool,
    /// TCP port of the daemon (`serve` binds 127.0.0.1:PORT; clients connect to it).
    #[arg(long, global = true, env = "BOT_PORT", default_value_t = daemon_api::DEFAULT_PORT)]
    port: u16,
    /// Full daemon URL for client commands (overrides --port).
    #[arg(long, global = true, env = "BOT_URL")]
    url: Option<String>,
    /// Use a paired daemon endpoint through iroh.
    #[arg(long, global = true, conflicts_with = "url")]
    remote: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive first-run setup (or scripted with --provider).
    Setup(SetupCmd),
    /// Check the installation (data dir, config, providers, daemon, browser, ...).
    Doctor(DoctorCmd),
    /// Manage stored secrets (values are never printed).
    Secrets {
        #[command(subcommand)]
        action: SecretsCmd,
    },
    /// Manage LLM providers.
    Providers {
        #[command(subcommand)]
        action: ProvidersCmd,
    },
    /// Run the local daemon API (127.0.0.1 only) until Ctrl-C / SIGTERM.
    ///
    /// On shutdown running tool steps get `--shutdown-grace` seconds to finish,
    /// then every live run is paused (resume it after the next start).
    ///
    /// Exit codes of the client commands: 0 ok, 1 error, 2 usage, 3 daemon
    /// unreachable, 4 auth rejected, 5 not found, 6 request rejected, 7 run failed.
    Serve {
        /// Run as an always-on host (peer networking defaults on).
        #[arg(long)]
        host: bool,
        /// Enable paired remote API access over iroh.
        #[arg(long)]
        peer: bool,
        /// Use the built-in echo provider instead of configured providers.
        #[cfg(feature = "fake-provider")]
        #[arg(long)]
        fake_provider: bool,
        /// Seconds running tool steps may take to finish on shutdown.
        #[arg(long, default_value_t = 10)]
        shutdown_grace: u64,
    },
    /// Show daemon status (also verifies the API token).
    Status,
    /// Send a message to a bot and stream the answer.
    Chat {
        /// Bot id (see `bot bots list`).
        bot_id: String,
        /// The message.
        message: String,
        /// Continue an existing chat instead of starting a new one.
        #[arg(long)]
        chat: Option<String>,
        /// What to do if the chat has a running turn.
        #[arg(long, value_parser = ["steer", "queue"], default_value = "queue")]
        mode: String,
    },
    /// Manage bots on the daemon.
    Bots {
        #[command(subcommand)]
        action: BotsCmd,
    },
    /// Answer a pending question card (`question_asked` event).
    Answer {
        /// Tool call id of the question (shown in the chat stream).
        call_id: String,
        /// The answer.
        text: String,
    },
    /// List or revoke a bot's persisted "always allow" grants.
    Grants {
        #[command(subcommand)]
        action: GrantsCmd,
    },
    /// Manage scheduled runs (cron / interval / one-shot, English or Turkish).
    Schedules {
        #[command(subcommand)]
        action: SchedulesCmd,
    },
    /// Messaging channels (Telegram).
    Channels {
        #[command(subcommand)]
        action: ChannelsCmd,
    },
    /// Approve (or deny) a pending tool call.
    Approve {
        /// Tool call id (shown in the chat stream / `approval_needed` event).
        tool_call_id: String,
        /// How far the approval reaches.
        #[arg(long, value_parser = ["once", "this_chat", "always", "always_category"], default_value = "once")]
        scope: String,
        /// Deny instead of approving.
        #[arg(long)]
        deny: bool,
        /// Reason given to the model on deny.
        #[arg(long, requires = "deny")]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum GrantsCmd {
    /// List the grants of a bot.
    List {
        /// Bot id.
        bot_id: String,
    },
    /// Remove one grant (the next matching call asks again).
    Revoke {
        /// Bot id.
        bot_id: String,
        /// Revoke the grant of this tool.
        #[arg(long, group = "target")]
        tool: Option<String>,
        /// Revoke the grant of this permission category (fs, shell, ...).
        #[arg(long, group = "target")]
        category: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchedulesCmd {
    /// List schedules.
    List,
    /// Add a schedule: `bot schedules add <bot-id> "every weekday at 9" "summarize my inbox"`.
    Add {
        /// Bot id (see `bot bots list`).
        bot_id: String,
        /// When: a phrase such as "every 15 minutes", "in 2 hours",
        /// "her gün 09:00" (parsed by the daemon).
        when: String,
        /// The prompt the bot runs each time.
        prompt: String,
        /// Name (default: the start of the prompt).
        #[arg(long)]
        name: Option<String>,
        /// IANA timezone for calendar phrases (default UTC).
        #[arg(long)]
        tz: Option<String>,
        /// Post the runs to this chat (default: a chat named after the schedule).
        #[arg(long)]
        chat: Option<String>,
    },
    /// Delete a schedule.
    Rm {
        /// Schedule id.
        id: String,
    },
    /// Fire a schedule now (in the background on the daemon).
    RunNow {
        /// Schedule id.
        id: String,
    },
}

#[derive(Subcommand)]
enum BotsCmd {
    /// List bots.
    List,
    /// Add a bot from a profile TOML file.
    Add {
        /// Path of the profile file.
        file: PathBuf,
    },
}

#[derive(Args)]
struct DoctorCmd {
    /// Also test provider connections over the network.
    #[arg(long)]
    online: bool,
    /// Write a redacted diagnostic bundle (JSON, no secrets) to this file.
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
}

#[derive(Args)]
struct SetupCmd {
    /// Provider id; switches to non-interactive mode.
    #[arg(long)]
    provider: Option<String>,
    /// Base URL; makes `--provider` a custom OpenAI-compatible provider.
    #[arg(long, requires = "provider")]
    base_url: Option<String>,
    /// Environment variable holding the API key.
    #[arg(long, requires = "provider")]
    key_env: Option<String>,
    /// Default model (first listed model when omitted).
    #[arg(long, requires = "provider")]
    model: Option<String>,
    /// Never prompt (required with --provider; documents script intent).
    #[arg(long, short = 'y')]
    yes: bool,
    /// Language of messages saved with the bot.
    #[arg(long, value_enum, default_value = "en")]
    lang: Lang,
    /// First bot preset.
    #[arg(long, value_enum, default_value = "general")]
    bot_preset: BotPreset,
    /// First bot name.
    #[arg(long)]
    bot_name: Option<String>,
    /// Default permission preset.
    #[arg(long, value_enum, default_value = "approve-destructive")]
    permission: PermissionPreset,
}

#[derive(Subcommand)]
enum ChannelsCmd {
    /// Talk to your bots from Telegram.
    Telegram {
        #[command(subcommand)]
        action: TelegramCmd,
    },
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
enum TelegramCmd {
    /// Store the `BotFather` token (hidden prompt, never printed) and enable the channel.
    Setup {
        /// Read the token from this environment variable instead of prompting.
        #[arg(long)]
        token_env: Option<String>,
    },
    /// Print a one-time 6-digit pairing code (valid 10 minutes) from the running daemon.
    Pair,
    /// Show whether the channel is configured, running, and how many users are paired.
    Status,
    /// Turn the channel off (the token stays stored).
    Disable,
}

#[derive(Subcommand)]
enum SecretsCmd {
    /// List secret names.
    List,
    /// Store a secret; value from `--value-env VAR` or a hidden prompt.
    Set {
        /// Secret name.
        name: String,
        /// Read the value from this environment variable.
        #[arg(long)]
        value_env: Option<String>,
    },
    /// Delete a secret.
    Delete {
        /// Secret name.
        name: String,
    },
}

#[derive(Subcommand)]
enum ProvidersCmd {
    /// List configured providers (`*` = global default).
    List,
    /// Add (or update) a provider.
    Add {
        /// Catalog id, or an id for a custom provider with --base-url.
        id: String,
        /// Custom OpenAI-compatible base URL.
        #[arg(long)]
        base_url: Option<String>,
        /// Environment variable holding the API key (else hidden prompt when
        /// the provider needs one).
        #[arg(long)]
        key_env: Option<String>,
        /// Default model.
        #[arg(long)]
        model: Option<String>,
        /// Skip the connection test.
        #[arg(long)]
        no_test: bool,
    },
    /// Test a provider's connection (default provider when omitted).
    Test {
        /// Provider id.
        id: Option<String>,
    },
    /// Choose the global default provider.
    Default {
        /// Provider id.
        id: String,
        /// Also set its default model.
        #[arg(long)]
        model: Option<String>,
    },
}

const BUILTIN_CATALOG: &[&str] = &[
    include_str!("../../../providers/catalog/anthropic.toml"),
    include_str!("../../../providers/catalog/deepseek.toml"),
    include_str!("../../../providers/catalog/gemini-openai-compat.toml"),
    include_str!("../../../providers/catalog/gemini.toml"),
    include_str!("../../../providers/catalog/lmstudio.toml"),
    include_str!("../../../providers/catalog/ollama.toml"),
    include_str!("../../../providers/catalog/openrouter.toml"),
];

fn load_catalog(dir: Option<&PathBuf>) -> Result<Catalog> {
    if let Some(d) = dir {
        return Catalog::load_dir(d).map_err(|errs| {
            errs.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
                .into()
        });
    }
    let mut entries = Vec::new();
    for src in BUILTIN_CATALOG {
        entries.push(toml::from_str::<ProviderEntry>(src)?);
    }
    Ok(Catalog::from_entries(entries))
}

fn data_dir(cli: &Cli) -> Result<PathBuf> {
    cli.data_dir
        .clone()
        .or_else(storage::data_dir)
        .ok_or_else(|| "cannot determine the data directory; use --data-dir".into())
}

fn read_value(env_var: Option<&str>, prompt: &str) -> Result<SecretValue> {
    if let Some(v) = env_var {
        let val = std::env::var(v).map_err(|_| format!("environment variable {v} is not set"))?;
        return Ok(SecretValue::new(val.trim()));
    }
    let v = dialoguer::Password::new().with_prompt(prompt).interact()?;
    Ok(SecretValue::new(v.trim()))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(e.downcast_ref::<ExitError>().map_or(1, |x| x.code));
    }
}

#[allow(clippy::too_many_lines)] // one flat dispatch over every command
fn run() -> Result<()> {
    let cli = Cli::parse();
    let Some(command) = &cli.command else {
        println!("Praxi-Bot. Run `bot setup` to get started, or `bot --help`.");
        return Ok(());
    };
    let data_dir = data_dir(&cli)?;
    let mut say = |s: &str| println!("{s}");

    if cli.remote.is_some()
        && !matches!(
            command,
            Command::Status
                | Command::Chat { .. }
                | Command::Bots { .. }
                | Command::Approve { .. }
                | Command::Answer { .. }
                | Command::Schedules { .. }
                | Command::Grants { .. }
                | Command::Channels {
                    action: ChannelsCmd::Telegram {
                        action: TelegramCmd::Pair
                    }
                }
        )
    {
        return Err("--remote supports daemon client commands (status, bots, chat, approve, answer, grants, schedules, channels telegram pair); this command manages the local installation".into());
    }

    // Daemon commands (no secret store needed, except `serve` with real providers).
    match command {
        Command::Serve { .. } => return run_serve(&cli, command, &data_dir),
        Command::Doctor(c) => return run_doctor(&cli, c, &data_dir),
        Command::Status
        | Command::Chat { .. }
        | Command::Bots { .. }
        | Command::Approve { .. }
        | Command::Answer { .. }
        | Command::Schedules { .. }
        | Command::Grants { .. }
        | Command::Channels {
            action: ChannelsCmd::Telegram {
                action: TelegramCmd::Pair,
            },
        } => {
            return run_client(&cli, command, &data_dir);
        }
        Command::Channels {
            action:
                ChannelsCmd::Telegram {
                    action: TelegramCmd::Disable,
                },
        } => return channels::telegram_disable(&data_dir, &mut say),
        _ => {}
    }

    // Commands that need no secret store.
    match command {
        Command::Providers {
            action: ProvidersCmd::List,
        } => return commands::providers_list(&data_dir, &mut say),
        Command::Providers {
            action: ProvidersCmd::Default { id, model },
        } => return commands::providers_default(&data_dir, id, model.as_deref(), &mut say),
        _ => {}
    }

    let (store, kind) = open_store(cli.secrets_backend, &data_dir, &ask_passphrase)?;
    eprintln!("(secret backend: {kind:?})");
    let store: &dyn SecretStore = store.as_ref();
    let catalog = load_catalog(cli.catalog_dir.as_ref())?;
    let env = SetupEnv {
        catalog: &catalog,
        store,
        data_dir: &data_dir,
        connector: &NetConnector,
    };

    match command {
        Command::Setup(c) => run_setup(c, &env),
        Command::Channels {
            action: ChannelsCmd::Telegram { action },
        } => run_telegram(&cli, &data_dir, store, action, &mut say),
        Command::Secrets { action } => match action {
            SecretsCmd::List => commands::secrets_list(store, &mut say),
            SecretsCmd::Set { name, value_env } => {
                let v = read_value(value_env.as_deref(), &format!("Value for {name}"))?;
                commands::secrets_set(store, name, &v)
            }
            SecretsCmd::Delete { name } => commands::secrets_delete(store, name),
        },
        Command::Doctor(_)
        | Command::Serve { .. }
        | Command::Status
        | Command::Chat { .. }
        | Command::Bots { .. }
        | Command::Approve { .. }
        | Command::Answer { .. }
        | Command::Schedules { .. }
        | Command::Grants { .. } => Ok(()),
        Command::Providers { action } => match action {
            ProvidersCmd::Add {
                id,
                base_url,
                key_env,
                model,
                no_test,
            } => {
                let choice = match base_url {
                    Some(u) => ProviderChoice::Custom {
                        id: id.clone(),
                        base_url: u.clone(),
                    },
                    None => ProviderChoice::Catalog(id.clone()),
                };
                let entry = wizard::entry_for(&choice, &catalog)?;
                let needs_key =
                    key_env.is_some() || matches!(entry.auth, provider_catalog::Auth::ApiKeyEnv(_));
                let key = if needs_key {
                    Some(read_value(
                        key_env.as_deref(),
                        &format!("API key for {id}"),
                    )?)
                } else {
                    None
                };
                let key = key.filter(|k| !k.expose().is_empty());
                commands::providers_add(
                    &env,
                    &choice,
                    key.as_ref(),
                    model.as_deref(),
                    !no_test,
                    &mut say,
                )
                .map(|_| ())
            }
            ProvidersCmd::Test { id } => commands::providers_test(&env, id.as_deref(), &mut say),
            ProvidersCmd::List | ProvidersCmd::Default { .. } => Ok(()),
        },
    }
}

/// `bot doctor`: never prompts, opens the secret store read-only.
fn run_doctor(cli: &Cli, c: &DoctorCmd, data_dir: &std::path::Path) -> Result<()> {
    let catalog = load_catalog(cli.catalog_dir.as_ref()).map_err(|e| e.to_string());
    let pass = std::env::var("BOT_SECRETS_PASSPHRASE").ok();
    let store = open_store(cli.secrets_backend, data_dir, &|_| pass.clone())
        .map(|(s, k)| (s, format!("{k:?}")))
        .map_err(|e| e.to_string());
    let browser = browser_cdp::find_browser();
    let env = doctor::DoctorEnv {
        data_dir,
        catalog: catalog.as_ref().map_err(Clone::clone),
        store: store
            .as_ref()
            .map(|(s, k)| (s.as_ref(), k.clone()))
            .map_err(Clone::clone),
        port: cli.port,
        online: c.online,
        browser: browser.as_ref().ok().cloned(),
        browser_error: browser.err().and_then(|e| match e {
            browser_cdp::Error::BrowserNotFound => None,
            other => Some(other.to_string()),
        }),
    };
    let checks = doctor::run_checks(&env);
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&doctor::to_json(&checks))?
        );
    } else {
        doctor::render_table(&checks, &mut std::io::stdout().lock())?;
    }
    if let Some(p) = &c.report {
        doctor::write_report(&env, &checks, p)?;
        eprintln!("report written to {}", p.display());
    }
    let fails = checks
        .iter()
        .filter(|c| c.status == doctor::Status::Fail)
        .count();
    if fails > 0 {
        return Err(client::fail(
            client::exit::GENERIC,
            format!("{fails} check(s) failed"),
        ));
    }
    Ok(())
}

fn run_serve(cli: &Cli, command: &Command, data_dir: &std::path::Path) -> Result<()> {
    use std::sync::Arc;

    let (host_mode, peer_enabled) = match command {
        Command::Serve { host, peer, .. } => (*host, *peer),
        _ => (false, false),
    };
    let grace = match command {
        Command::Serve { shutdown_grace, .. } => std::time::Duration::from_secs(*shutdown_grace),
        _ => daemon_api::DEFAULT_SHUTDOWN_GRACE,
    };
    #[cfg(feature = "fake-provider")]
    if let Command::Serve {
        fake_provider: true,
        ..
    } = command
    {
        // Demos: the echo provider and a scripted in-memory desktop.
        let providers = agent_core::Providers::single(Arc::new(daemon_api::fake::FakeProvider));
        let desktop: Arc<dyn desktop_api::DesktopBackend> =
            Arc::new(daemon_api::fake::FakeDesktop::default());
        let opts = serve::ServeOptions {
            port: cli.port,
            json: cli.json,
            grace,
            telegram_store: None,
            host_mode,
            peer_enabled,
        };
        return serve::serve(data_dir, &opts, providers, None, Some(desktop));
    }
    let (store, kind) = open_store(cli.secrets_backend, data_dir, &ask_passphrase)?;
    eprintln!("(secret backend: {kind:?})");
    let catalog = load_catalog(cli.catalog_dir.as_ref())?;
    let providers = serve::build_providers(data_dir, &catalog, store.as_ref(), &mut |w| {
        eprintln!("warning: {w}");
    })?;
    // `POST /v1/providers/reload` rebuilds the providers the same way.
    let store: Arc<dyn secret_store::SecretStore> = Arc::from(store);
    let telegram_store = Arc::clone(&store);
    let host = daemon_api::ProviderHost {
        catalog: catalog.clone(),
        reload: {
            let (dir, catalog) = (data_dir.to_path_buf(), catalog);
            Box::new(move || {
                serve::build_providers(&dir, &catalog, store.as_ref(), &mut |_| {})
                    .map_err(|e| e.to_string())
            })
        },
    };
    let desktop = serve::local_desktop(&mut |w| eprintln!("warning: {w}"));
    let opts = serve::ServeOptions {
        port: cli.port,
        json: cli.json,
        grace,
        telegram_store: Some(telegram_store),
        host_mode,
        peer_enabled,
    };
    serve::serve(data_dir, &opts, providers, Some(host), desktop)
}

/// A client of the daemon: token from `BOT_API_TOKEN` or the token file,
/// address from `--url` / `--port`.
fn make_client(cli: &Cli, data_dir: &std::path::Path) -> Result<Client> {
    let token = match std::env::var("BOT_API_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_owned(),
        _ => daemon_api::read_token(data_dir).map_err(|e| {
            client::fail(
                client::exit::AUTH,
                format!(
                    "cannot read the API token ({}): {e}; run `bot serve` once or set BOT_API_TOKEN",
                    daemon_api::token_path(data_dir).display()
                ),
            )
        })?,
    };
    let base = cli
        .url
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", cli.port));
    Ok(Client::new(&base, token))
}

fn current_thread_runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

/// `bot channels telegram setup|status` (the ones that read the secret store).
fn run_telegram(
    cli: &Cli,
    data_dir: &std::path::Path,
    store: &dyn SecretStore,
    action: &TelegramCmd,
    say: &mut dyn FnMut(&str),
) -> Result<()> {
    match action {
        TelegramCmd::Setup { token_env } => {
            let token = read_value(
                token_env.as_deref(),
                "Telegram bot token (from @BotFather; input hidden)",
            )?;
            channels::telegram_setup(store, data_dir, &token, say)
        }
        TelegramCmd::Status => {
            let configured = channels::telegram_configured(store)?;
            let enabled = channels::telegram_enabled(data_dir);
            // The daemon's view; unreachable / unauthorised just means "not running".
            let daemon = make_client(cli, data_dir).ok().and_then(|c| {
                current_thread_runtime()
                    .ok()?
                    .block_on(c.request(reqwest::Method::GET, "/v1/channels", None))
                    .ok()?
                    .as_array()?
                    .iter()
                    .find(|ch| ch["id"] == "telegram")
                    .cloned()
            });
            channels::telegram_status(
                configured,
                enabled,
                daemon.as_ref(),
                channels::paired_users_on_disk(data_dir),
                cli.json,
                &mut std::io::stdout().lock(),
            )
        }
        TelegramCmd::Pair | TelegramCmd::Disable => Ok(()),
    }
}

fn run_client(cli: &Cli, command: &Command, data_dir: &std::path::Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let remote = if let Some(endpoint) = &cli.remote {
        Some(rt.block_on(peer::start_client(data_dir, endpoint))?)
    } else {
        None
    };
    let c = match &remote {
        Some((_, proxy)) => Client::new(&proxy.base_url, proxy.token.clone()),
        None => make_client(cli, data_dir)?,
    };
    let mut out = std::io::stdout().lock();
    let json = cli.json;
    let outcome = rt.block_on(async {
        match command {
            Command::Status => remote::status(&c, json, &mut out).await,
            Command::Bots {
                action: BotsCmd::List,
            } => remote::bots_list(&c, json, &mut out).await,
            Command::Bots {
                action: BotsCmd::Add { file },
            } => remote::bots_add(&c, file, json, &mut out).await,
            Command::Chat {
                bot_id,
                message,
                chat,
                mode,
            } => {
                let opts = remote::ChatOpts {
                    bot_id,
                    message,
                    chat_id: chat.as_deref(),
                    mode,
                };
                remote::chat(&c, &opts, json, &mut out).await
            }
            Command::Answer { call_id, text } => {
                remote::answer(&c, call_id, text, json, &mut out).await
            }
            Command::Grants {
                action: GrantsCmd::List { bot_id },
            } => remote::grants_list(&c, bot_id, json, &mut out).await,
            Command::Grants {
                action:
                    GrantsCmd::Revoke {
                        bot_id,
                        tool,
                        category,
                    },
            } => {
                let target = remote::grant_target_body(tool.as_deref(), category.as_deref())?;
                remote::grants_revoke(&c, bot_id, target, json, &mut out).await
            }
            Command::Schedules { action } => run_schedules(&c, action, json, &mut out).await,
            Command::Channels {
                action:
                    ChannelsCmd::Telegram {
                        action: TelegramCmd::Pair,
                    },
            } => channels::telegram_pair(&c, json, &mut out).await,
            Command::Approve {
                tool_call_id,
                scope,
                deny,
                reason,
            } => {
                remote::approve(
                    &c,
                    tool_call_id,
                    scope,
                    *deny,
                    reason.as_deref(),
                    json,
                    &mut out,
                )
                .await
            }
            _ => Ok(()),
        }
    });
    if let Some((net, proxy)) = remote {
        drop(proxy);
        rt.block_on(net.shutdown());
    }
    outcome
}

async fn run_schedules(
    c: &Client,
    action: &SchedulesCmd,
    json: bool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    match action {
        SchedulesCmd::List => remote::schedules_list(c, json, out).await,
        SchedulesCmd::Add {
            bot_id,
            when,
            prompt,
            name,
            tz,
            chat,
        } => {
            let opts = remote::ScheduleAdd {
                bot_id,
                when,
                prompt,
                name: name.as_deref(),
                tz: tz.as_deref(),
                chat_id: chat.as_deref(),
            };
            remote::schedules_add(c, &opts, json, out).await
        }
        SchedulesCmd::Rm { id } => remote::schedules_rm(c, id, json, out).await,
        SchedulesCmd::RunNow { id } => remote::schedules_run_now(c, id, json, out).await,
    }
}

fn run_setup(c: &SetupCmd, env: &SetupEnv<'_>) -> Result<()> {
    let plan = if let Some(provider) = &c.provider {
        let args = SetupArgs {
            provider: provider.clone(),
            base_url: c.base_url.clone(),
            key_env: c.key_env.clone(),
            model: c.model.clone(),
            lang: c.lang,
            bot_preset: c.bot_preset,
            bot_name: c.bot_name.clone(),
            permission: c.permission,
        };
        wizard::plan_from_args(&args, env, &|v| std::env::var(v).ok())?
    } else {
        if c.yes {
            return Err("--yes needs --provider (non-interactive mode)".into());
        }
        wizard::run_wizard(&mut TerminalPrompter, env)?
    };
    let out = wizard::apply_plan(&plan, env)?;
    println!(
        "Provider: {}{}",
        out.provider_id,
        if out.is_default { " (default)" } else { "" }
    );
    if let Some(m) = &out.model_ref {
        println!("Model:    {m}");
    }
    if let Some(s) = &out.secret_name {
        println!("API key stored as secret {s:?}");
    }
    match out.bot_id {
        Some(id) => println!("Bot created: {id}"),
        None => println!(
            "No model chosen, so no bot was created. Pick one with `bot providers default <id> --model <m>`, then add a bot (`bot bots add <profile.toml>`)."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn builtin_catalog_parses() {
        let c = load_catalog(None).unwrap();
        assert!(c.get("openrouter").is_some());
        assert_eq!(c.len(), BUILTIN_CATALOG.len());
    }

    #[test]
    fn parses_channels_telegram_commands() {
        let parse = |args: &[&str]| {
            let mut v = vec!["bot", "channels", "telegram"];
            v.extend_from_slice(args);
            Cli::try_parse_from(v).map(|c| c.command)
        };
        for (arg, want) in [
            ("pair", TelegramCmd::Pair),
            ("status", TelegramCmd::Status),
            ("disable", TelegramCmd::Disable),
            ("setup", TelegramCmd::Setup { token_env: None }),
        ] {
            assert!(
                matches!(
                    parse(&[arg]).unwrap(),
                    Some(Command::Channels { action: ChannelsCmd::Telegram { action } }) if action == want
                ),
                "{arg}"
            );
        }
        assert!(matches!(
            parse(&["setup", "--token-env", "TG"]).unwrap(),
            Some(Command::Channels { action: ChannelsCmd::Telegram {
                action: TelegramCmd::Setup { token_env: Some(ref v) } } }) if v == "TG"
        ));
        // The token is never an argv value.
        assert!(parse(&["setup", "123:abc"]).is_err());
        assert!(parse(&[]).is_err());
        assert!(parse(&["pair", "extra"]).is_err());
    }

    #[test]
    fn parses_schedules_commands() {
        assert!(matches!(
            Cli::try_parse_from(["bot", "schedules", "list"])
                .unwrap()
                .command,
            Some(Command::Schedules {
                action: SchedulesCmd::List
            })
        ));
        let cli = Cli::try_parse_from([
            "bot",
            "schedules",
            "add",
            "b1",
            "every day at 9",
            "report",
            "--tz",
            "Europe/Istanbul",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Schedules {
                action: SchedulesCmd::Add { ref when, ref prompt, ref tz, .. }
            }) if when == "every day at 9" && prompt == "report" && tz.as_deref() == Some("Europe/Istanbul")
        ));
        assert!(Cli::try_parse_from(["bot", "schedules", "add", "b1", "when"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["bot", "schedules", "rm", "s1"])
                .unwrap()
                .command,
            Some(Command::Schedules {
                action: SchedulesCmd::Rm { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["bot", "schedules", "run-now", "s1"])
                .unwrap()
                .command,
            Some(Command::Schedules {
                action: SchedulesCmd::RunNow { .. }
            })
        ));
    }

    #[test]
    fn parses_peer_and_remote_flags() {
        let cli = Cli::try_parse_from(["bot", "--remote", "endpoint", "status"]).unwrap();
        assert_eq!(cli.remote.as_deref(), Some("endpoint"));
        assert!(
            Cli::try_parse_from([
                "bot",
                "--remote",
                "endpoint",
                "--url",
                "http://127.0.0.1",
                "status"
            ])
            .is_err()
        );
        let cli = Cli::try_parse_from(["bot", "serve", "--host", "--peer"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Serve {
                host: true,
                peer: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_daemon_commands() {
        let cli = Cli::try_parse_from(["bot", "serve", "--port", "0"]).unwrap();
        assert_eq!(cli.port, 0);
        assert!(matches!(cli.command, Some(Command::Serve { .. })));
        let cli = Cli::try_parse_from(["bot", "--json", "chat", "b1", "hello", "--mode", "steer"])
            .unwrap();
        assert!(cli.json);
        assert!(matches!(
            cli.command,
            Some(Command::Chat { ref mode, .. }) if mode == "steer"
        ));
        let cli = Cli::try_parse_from(["bot", "answer", "c1", "blue"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Answer { ref call_id, ref text }) if call_id == "c1" && text == "blue"
        ));
        assert!(Cli::try_parse_from(["bot", "answer", "c1"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["bot", "grants", "list", "b1"])
                .unwrap()
                .command,
            Some(Command::Grants {
                action: GrantsCmd::List { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["bot", "grants", "revoke", "b1", "--category", "fs"])
                .unwrap()
                .command,
            Some(Command::Grants {
                action: GrantsCmd::Revoke { .. }
            })
        ));
        assert!(
            Cli::try_parse_from([
                "bot",
                "grants",
                "revoke",
                "b1",
                "--tool",
                "t",
                "--category",
                "fs"
            ])
            .is_err()
        );
        let cli =
            Cli::try_parse_from(["bot", "approve", "c1", "--scope", "always_category"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Approve { .. })));
        let cli = Cli::try_parse_from(["bot", "serve", "--shutdown-grace", "3"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Serve {
                shutdown_grace: 3,
                ..
            })
        ));
        let cli = Cli::try_parse_from(["bot", "approve", "c1", "--scope", "always"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Approve { deny: false, .. })
        ));
        assert!(Cli::try_parse_from(["bot", "approve", "c1", "--scope", "bogus"]).is_err());
        assert!(Cli::try_parse_from(["bot", "approve", "c1", "--reason", "x"]).is_err());
        assert!(Cli::try_parse_from(["bot", "chat", "b1"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["bot", "bots", "add", "p.toml"])
                .unwrap()
                .command,
            Some(Command::Bots {
                action: BotsCmd::Add { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["bot", "status"]).unwrap().command,
            Some(Command::Status)
        ));
    }

    #[test]
    fn parses_scripted_setup_flags() {
        let cli = Cli::try_parse_from([
            "bot",
            "setup",
            "--provider",
            "openrouter",
            "--key-env",
            "OPENROUTER_API_KEY",
            "--model",
            "x/y",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Setup(SetupCmd { yes: true, .. }))
        ));
        assert!(Cli::try_parse_from(["bot", "setup", "--key-env", "X"]).is_err());
    }
}
