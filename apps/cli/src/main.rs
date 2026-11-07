//! Command-line interface entry point for the Praxi-Bot agent (`bot`).

mod client;
mod commands;
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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive first-run setup (or scripted with --provider).
    Setup(SetupCmd),
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    let Some(command) = &cli.command else {
        println!("Praxi-Bot. Run `bot setup` to get started, or `bot --help`.");
        return Ok(());
    };
    let data_dir = data_dir(&cli)?;
    let mut say = |s: &str| println!("{s}");

    // Daemon commands (no secret store needed, except `serve` with real providers).
    match command {
        Command::Serve { .. } => return run_serve(&cli, command, &data_dir),
        Command::Status
        | Command::Chat { .. }
        | Command::Bots { .. }
        | Command::Approve { .. }
        | Command::Answer { .. }
        | Command::Grants { .. } => {
            return run_client(&cli, command, &data_dir);
        }
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
        Command::Secrets { action } => match action {
            SecretsCmd::List => commands::secrets_list(store, &mut say),
            SecretsCmd::Set { name, value_env } => {
                let v = read_value(value_env.as_deref(), &format!("Value for {name}"))?;
                commands::secrets_set(store, name, &v)
            }
            SecretsCmd::Delete { name } => commands::secrets_delete(store, name),
        },
        Command::Serve { .. }
        | Command::Status
        | Command::Chat { .. }
        | Command::Bots { .. }
        | Command::Approve { .. }
        | Command::Answer { .. }
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

fn run_serve(cli: &Cli, command: &Command, data_dir: &std::path::Path) -> Result<()> {
    use std::sync::Arc;

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
        return serve::serve(
            data_dir,
            cli.port,
            providers,
            None,
            Some(desktop),
            cli.json,
            grace,
        );
    }
    let (store, kind) = open_store(cli.secrets_backend, data_dir, &ask_passphrase)?;
    eprintln!("(secret backend: {kind:?})");
    let catalog = load_catalog(cli.catalog_dir.as_ref())?;
    let providers = serve::build_providers(data_dir, &catalog, store.as_ref(), &mut |w| {
        eprintln!("warning: {w}");
    })?;
    // `POST /v1/providers/reload` rebuilds the providers the same way.
    let store: Arc<dyn secret_store::SecretStore> = Arc::from(store);
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
    serve::serve(
        data_dir,
        cli.port,
        providers,
        Some(host),
        desktop,
        cli.json,
        grace,
    )
}

fn run_client(cli: &Cli, command: &Command, data_dir: &std::path::Path) -> Result<()> {
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
    let c = Client::new(&base, token);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut out = std::io::stdout().lock();
    let json = cli.json;
    rt.block_on(async {
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
    })
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
    println!("Bot created: {}", out.bot_id);
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
