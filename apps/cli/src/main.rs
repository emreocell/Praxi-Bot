//! Command-line interface entry point for the Praxi-Bot agent (`bot`).

mod backup_cmd;
mod backup_crypto;
mod calendar_host;
mod catalog_cmd;
mod channels;
#[cfg(test)]
mod channels_tests;
mod chats_cmd;
mod cli_artifacts;
mod client;
mod commands;
mod components_host;
mod composio_host;
mod computer;
mod doctor;
mod email_cmd;
mod email_host;
mod homeassistant_host;
mod host;
mod host_links;
mod host_provision;
mod host_runtime;
mod host_setup;
mod mcp_host;
mod mcp_paste;
mod mcp_serve;
mod pc_target;
mod peer;
mod presets;
mod profile_cmd;
mod prompt;
mod remote;
mod serve;
mod server_target;
mod skills_cmd;
mod update;
mod view_targets;
mod voice_gemini;
mod voice_host;
mod web_host;
mod wizard;
mod workflow;

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
#[command(
    name = "bot",
    version,
    about = "Praxi-Bot agent CLI",
    long_about = None,
    after_help = "Exit codes:\n  0  OK\n  1  General error\n  2  Usage error\n  3  Daemon unreachable\n  4  Authentication rejected (or missing API token)\n  5  Not found\n  6  Request rejected (invalid input, conflict, or setup required)\n  7  The run ended with an error or was cancelled"
)]
struct Cli {
    /// Data directory (defaults to `BOT_DATA_DIR` or the OS app-data directory).
    #[arg(long, global = true, help_heading = "Global Options")]
    data_dir: Option<PathBuf>,
    /// Directory with provider catalog `*.toml` files (defaults to the built-in catalog).
    #[arg(
        long,
        global = true,
        env = "BOT_CATALOG_DIR",
        help_heading = "Global Options"
    )]
    catalog_dir: Option<PathBuf>,
    /// Secret backend: `auto` (existing encrypted file, else OS keychain), `keyring`, or `file`.
    /// The file backend reads `BOT_SECRETS_PASSPHRASE` or prompts interactively.
    #[arg(
        long,
        global = true,
        default_value = "auto",
        env = "BOT_SECRETS_BACKEND",
        help_heading = "Global Options"
    )]
    secrets_backend: BackendChoice,
    /// Emit machine-readable JSON (or NDJSON for `chat`) on stdout for supported commands.
    /// Errors always go to stderr as text.
    #[arg(long, global = true, help_heading = "Global Options")]
    json: bool,
    /// Loopback TCP port of the daemon (`serve` binds 127.0.0.1:PORT; clients connect to it).
    #[arg(
        long,
        global = true,
        env = "BOT_PORT",
        default_value_t = daemon_api::DEFAULT_PORT,
        help_heading = "Global Options"
    )]
    port: u16,
    /// Full daemon URL for client commands (overrides `--port`).
    #[arg(long, global = true, env = "BOT_URL", help_heading = "Global Options")]
    url: Option<String>,
    /// Target a paired remote daemon by endpoint ID (conflicts with `--url`).
    #[arg(
        long,
        global = true,
        value_name = "ENDPOINT",
        conflicts_with = "url",
        help_heading = "Global Options"
    )]
    remote: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // one-shot CLI args, built once
enum Command {
    /// Record, review, save, and replay workflows from successful runs.
    Workflow {
        #[command(subcommand)]
        action: workflow::WorkflowCmd,
    },
    /// Print a shell completion script to stdout.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate section 1 man pages in the given directory.
    ManPages {
        /// Directory where `.1` man page files will be written.
        output_dir: PathBuf,
    },
    /// Review local skills and explicitly trust reviewed content.
    Skills {
        #[command(subcommand)]
        action: skills_cmd::SkillsCmd,
    },
    /// Create or revoke scoped MCP credentials on the local daemon.
    Token {
        #[command(subcommand)]
        action: mcp_serve::TokenCmd,
    },
    /// Manage the active host and paired devices.
    Host {
        #[command(subcommand)]
        action: host::HostCmd,
    },
    /// Encrypted backup of bots, chats, memory, skills, and settings: `create`, `verify`, `restore`.
    ///
    /// Backups are protected with a passphrase (hidden prompt, `--passphrase-file`
    /// or `--passphrase-env`; never a command-line value). Existing files are never
    /// overwritten. API keys are excluded unless `--include-secrets` is given.
    /// Stopping the daemon first gives a snapshot that is consistent across all files.
    Backup {
        #[command(subcommand)]
        action: backup_cmd::BackupAction,
    },
    /// Restore a plaintext backup made by an older version (stop the daemon first).
    ///
    /// Encrypted backups are restored with `bot backup restore`.
    Restore(backup_cmd::RestoreCmd),
    /// Set up a provider, default model, and first bot (interactive, or scripted with `--provider`).
    Setup(SetupCmd),
    /// Check installation health (data directory, config, providers, daemon, browser, and more).
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
    /// Validate the curated model catalog, check freshness, or diff against a provider snapshot.
    Catalog {
        #[command(subcommand)]
        action: catalog_cmd::CatalogCmd,
    },
    /// Run the local daemon API on 127.0.0.1 until Ctrl-C or SIGTERM.
    ///
    /// On shutdown, running tool steps get `--shutdown-grace` seconds to finish,
    /// then every live run is paused so it can be resumed after the next start.
    Serve {
        /// Run as an always-on host (enables peer networking by default).
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
        /// Read the owner key (one line) from standard input.
        ///
        /// The desktop app uses this so the key never appears in argv, a file, or the environment.
        #[arg(long)]
        owner_key_stdin: bool,
        /// Safe mode (also `BOT_SAFE_MODE=1`): chat works without tools; tools, schedules,
        /// channels, MCP servers, apps, memory work and peer networking stay off.
        ///
        /// Nothing is deleted or changed: restart without it to get everything back. It can
        /// only be chosen here, at process start: no route or bot can switch it.
        #[arg(long, alias = "safe-mode")]
        safe: bool,
    },
    /// Update a Linux installation (does not restart the service).
    SelfUpdate(update::UpdateArgs),
    /// Show daemon status and verify the API token.
    Status,
    /// Open a local loopback tunnel to a paired remote daemon (requires `--remote`).
    ///
    /// Waits up to 20 seconds for the tunnel to connect, prints the loopback URL
    /// and bearer token, and keeps the tunnel open until Ctrl-C.
    /// Treat the printed token as a secret and close the tunnel when finished.
    Proxy,
    /// Send a message to a bot and stream the answer.
    Chat {
        /// Bot ID (see `bot bots list`).
        bot_id: String,
        /// Message text to send.
        message: String,
        /// Continue an existing chat instead of starting a new one.
        #[arg(long)]
        chat: Option<String>,
        /// How to handle the message when the chat already has a running turn.
        #[arg(long, value_parser = ["steer", "queue"], default_value = "queue")]
        mode: String,
    },
    /// List and export chats from the daemon.
    Chats {
        #[command(subcommand)]
        action: chats_cmd::ChatsCmd,
    },
    /// Manage bots on the daemon.
    Bots {
        #[command(subcommand)]
        action: BotsCmd,
    },
    /// Export or import a shareable bot profile (no secrets, grants, memory or paths).
    Profile {
        #[command(subcommand)]
        action: profile_cmd::ProfileCmd,
    },
    /// Answer a pending question from a bot.
    Answer {
        /// Question call ID shown in the chat stream.
        call_id: String,
        /// Answer text to send back to the bot.
        text: String,
    },
    /// List or revoke a bot's persisted "always allow" grants.
    Grants {
        #[command(subcommand)]
        action: GrantsCmd,
    },
    /// Manage scheduled runs (cron, interval, or one-shot).
    Schedules {
        #[command(subcommand)]
        action: SchedulesCmd,
    },
    /// Manage daemon email accounts (IMAP/SMTP or AgentMail).
    #[allow(clippy::doc_markdown)] // doc comment is the --help text; product name stays plain
    Email {
        #[command(subcommand)]
        action: EmailCmd,
    },
    /// Manage daemon MCP servers and stdio exposure.
    Mcp {
        #[command(subcommand)]
        action: McpCmd,
    },
    /// Manage messaging channels (Telegram).
    Channels {
        #[command(subcommand)]
        action: ChannelsCmd,
    },
    /// Approve or deny a pending tool call.
    Approve {
        /// Tool call ID shown in the chat stream or `approval_needed` event.
        tool_call_id: String,
        /// Approval scope (`run_control` applies only when offered by the approval card).
        #[arg(long, value_parser = ["once", "this_chat", "always", "always_category", "run_control"], default_value = "once")]
        scope: String,
        /// Deny the tool call instead of approving it.
        #[arg(long)]
        deny: bool,
        /// Reason given to the model when denying.
        #[arg(long, requires = "deny")]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Add MCP servers from a URL, launch command, or MCP JSON paste.
    ///
    /// Accepts a server URL, a `claude mcp add ...` line, an `npx`/`uvx` command
    /// line, or MCP JSON (`{"mcpServers": ...}`). Quote the paste, or pass `-` to
    /// read from stdin. Values in the paste (`-e KEY=value`, headers, `env`) are
    /// stored in the secret store, never in the config file.
    Add {
        /// Override the derived server name (single server only).
        #[arg(long)]
        name: Option<String>,
        /// Show what would be added without contacting any server or saving changes.
        #[arg(long)]
        dry_run: bool,
        /// Server URL, launch line, or JSON paste (use `-` to read from stdin).
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true, trailing_var_arg = true)]
        input: Vec<String>,
    },
    /// Expose a selected bot as a scoped Praxi MCP server over stdio.
    ///
    /// Uses a scoped credential created with `bot token create`.
    Serve(mcp_serve::ServeArgs),
}

#[derive(Subcommand)]
enum GrantsCmd {
    /// List persisted grants for a bot.
    List {
        /// Bot ID (see `bot bots list`).
        bot_id: String,
    },
    /// Revoke one persisted grant so future calls ask again.
    Revoke {
        /// Bot ID (see `bot bots list`).
        bot_id: String,
        /// Revoke the grant for this tool name.
        #[arg(long, group = "target")]
        tool: Option<String>,
        /// Revoke the grant for this permission category (e.g. `fs`, `shell`).
        #[arg(long, group = "target")]
        category: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchedulesCmd {
    /// List schedules.
    List,
    /// Add a schedule for a bot.
    ///
    /// Example: `bot schedules add <bot-id> "every weekday at 9" "summarize my inbox"`.
    Add {
        /// Bot ID (see `bot bots list`).
        bot_id: String,
        /// Schedule phrase such as "every 15 minutes", "in 2 hours", or "her gün 09:00".
        when: String,
        /// Prompt the bot runs each time the schedule fires.
        prompt: String,
        /// Display name (defaults to the start of the prompt).
        #[arg(long)]
        name: Option<String>,
        /// IANA timezone for calendar phrases (defaults to UTC).
        #[arg(long)]
        tz: Option<String>,
        /// Post runs to this chat ID (defaults to a chat named after the schedule).
        #[arg(long)]
        chat: Option<String>,
    },
    /// Delete a schedule.
    Rm {
        /// Schedule ID (see `bot schedules list`).
        id: String,
    },
    /// Trigger a schedule immediately in the background.
    RunNow {
        /// Schedule ID (see `bot schedules list`).
        id: String,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // one-shot CLI args, built once
#[allow(clippy::doc_markdown)] // doc comments are the --help text; product names stay plain
enum EmailCmd {
    /// List configured email accounts (never shows passwords).
    List,
    /// Add an IMAP/SMTP email account or an AgentMail inbox.
    ///
    /// IMAP/SMTP example:
    ///   bot email add work me@gmail.com --preset gmail --bot Atlas --password-env MAIL_PW
    ///
    /// AgentMail example:
    ///   bot email add --provider agentmail --name helper --bot Atlas --api-key-env AGENTMAIL_KEY
    #[command(group = clap::ArgGroup::new("scope").required(true).args(["bot", "all_bots"]))]
    Add {
        /// Account name (letters, digits, `.`, `_`, `-`).
        #[arg(required_unless_present = "name_flag")]
        name: Option<String>,
        /// Address mail is sent from (not used with `--provider agentmail`).
        #[arg(required_unless_present = "provider")]
        address: Option<String>,
        /// Account name as a flag (same as the positional name).
        #[arg(long = "name", conflicts_with = "name")]
        name_flag: Option<String>,
        /// Account provider: `imap_smtp` (your own mailbox, default) or `agentmail`.
        #[arg(long, value_parser = ["imap_smtp", "agentmail"])]
        provider: Option<String>,
        /// AgentMail: hint for the part before `@` in the new address.
        #[arg(long)]
        name_hint: Option<String>,
        /// AgentMail: custom verified domain (defaults to `agentmail.to`).
        #[arg(long)]
        domain: Option<String>,
        /// AgentMail: environment variable holding your API key (hidden prompt when omitted).
        #[arg(long)]
        api_key_env: Option<String>,
        /// Fill IMAP/SMTP server settings for gmail, outlook, yahoo, or icloud.
        #[arg(long, value_parser = ["gmail", "outlook", "yahoo", "icloud"])]
        preset: Option<String>,
        /// IMAP host (port 993, TLS).
        #[arg(long)]
        imap_host: Option<String>,
        /// IMAP port.
        #[arg(long)]
        imap_port: Option<u16>,
        /// SMTP host.
        #[arg(long)]
        smtp_host: Option<String>,
        /// SMTP port (465 with `implicit`, 587 with `start_tls`).
        #[arg(long)]
        smtp_port: Option<u16>,
        /// SMTP transport security.
        #[arg(long, value_parser = ["implicit", "start_tls"])]
        smtp_security: Option<String>,
        /// Login username when it differs from the email address.
        #[arg(long)]
        username: Option<String>,
        /// Restrict this account to one bot (bot name or ID; required unless `--all-bots`).
        #[arg(long)]
        bot: Option<String>,
        /// Allow every bot with email tools to use this account (required unless `--bot`).
        #[arg(long)]
        all_bots: bool,
        /// Sender address or `@domain` allowed to trigger runs (repeatable).
        #[arg(long = "allowed-sender")]
        allowed_senders: Vec<String>,
        /// Environment variable holding the password or app password (hidden prompt when omitted).
        #[arg(long)]
        password_env: Option<String>,
    },
    /// Remove an email account and its stored password or key.
    Rm {
        /// Account name (see `bot email list`).
        name: String,
        /// AgentMail: also delete the inbox at AgentMail (irreversible; off by default).
        #[arg(long)]
        delete_inbox: bool,
    },
    /// Test IMAP/SMTP authentication or the AgentMail API key (sends no mail).
    Test {
        /// Account name (see `bot email list`).
        name: String,
    },
}

#[derive(Subcommand)]
enum BotsCmd {
    /// List configured bots.
    List,
    /// Create a bot from a profile TOML file.
    Add {
        /// Path to the bot profile TOML file.
        file: PathBuf,
    },
}

#[derive(Args)]
struct DoctorCmd {
    /// Also test provider connections and peer relay reachability over the network.
    #[arg(long)]
    online: bool,
    /// Write a redacted diagnostic bundle (JSON, no secrets) to this file.
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
}

#[derive(Args)]
struct SetupCmd {
    /// Provider ID from the catalog (or a custom ID with `--base-url`); enables non-interactive mode.
    #[arg(long)]
    provider: Option<String>,
    /// Custom OpenAI-compatible base URL (requires `--provider`).
    #[arg(long, requires = "provider")]
    base_url: Option<String>,
    /// Environment variable holding the API key (requires `--provider`).
    #[arg(long, requires = "provider")]
    key_env: Option<String>,
    /// Default model ID; uses the first listed model when omitted (requires `--provider`).
    #[arg(long, requires = "provider")]
    model: Option<String>,
    /// Confirm non-interactive setup without prompting (required with `--provider`).
    #[arg(long, short = 'y')]
    yes: bool,
    /// Language for messages saved with the bot.
    #[arg(long, value_enum, default_value = "en")]
    lang: Lang,
    /// Preset for the first bot.
    #[arg(long, value_enum, default_value = "general")]
    bot_preset: BotPreset,
    /// Display name for the first bot.
    #[arg(long)]
    bot_name: Option<String>,
    /// Default permission preset for the first bot.
    #[arg(long, value_enum, default_value = "approve-destructive")]
    permission: PermissionPreset,
}

#[derive(Subcommand)]
enum ChannelsCmd {
    /// Manage the Telegram channel.
    Telegram {
        #[command(subcommand)]
        action: TelegramCmd,
    },
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
enum TelegramCmd {
    /// Store the `BotFather` token (hidden prompt, never printed) and enable Telegram.
    Setup {
        /// Read the token from this environment variable instead of prompting.
        #[arg(long)]
        token_env: Option<String>,
    },
    /// Print a one-time 6-digit pairing code (valid for 10 minutes) from the running daemon.
    Pair,
    /// Show whether Telegram is configured, enabled, running, and how many users are paired.
    Status,
    /// Disable the Telegram channel while keeping the stored token.
    Disable,
}

#[derive(Subcommand)]
enum SecretsCmd {
    /// List stored secret names (never prints values).
    List,
    /// Store a secret from `--value-env` or a hidden prompt.
    Set {
        /// Secret name.
        name: String,
        /// Read the secret value from this environment variable instead of prompting.
        #[arg(long)]
        value_env: Option<String>,
    },
    /// Delete a stored secret.
    Delete {
        /// Secret name (see `bot secrets list`).
        name: String,
    },
}

#[derive(Subcommand)]
enum ProvidersCmd {
    /// List configured providers (`*` marks the global default).
    List,
    /// Add or update an LLM provider.
    ///
    /// Built-in catalog IDs: anthropic, deepseek, gemini, gemini-openai-compat,
    /// lmstudio, ollama, openrouter. Pass `--base-url` to add a custom
    /// OpenAI-compatible provider under any ID.
    Add {
        /// Built-in catalog ID, or a custom ID when used with `--base-url`.
        id: String,
        /// Custom OpenAI-compatible base URL (for example `http://localhost:8000/v1`).
        #[arg(long)]
        base_url: Option<String>,
        /// Environment variable holding the API key (hidden prompt when omitted and required).
        #[arg(long)]
        key_env: Option<String>,
        /// Default model ID for this provider.
        #[arg(long)]
        model: Option<String>,
        /// Skip the live connection test and save the provider immediately.
        #[arg(long)]
        no_test: bool,
    },
    /// Test a provider's connection (uses the default provider when ID is omitted).
    Test {
        /// Provider ID (see `bot providers list`).
        id: Option<String>,
    },
    /// Look for a local model server (Ollama, LM Studio, llama.cpp) on loopback
    /// and print what it found as JSON. Contacts only 127.0.0.1.
    DetectLocal,
    /// Set the global default provider.
    Default {
        /// Provider ID (see `bot providers list`).
        id: String,
        /// Also update this provider's default model ID.
        #[arg(long)]
        model: Option<String>,
    },
}

const BUILTIN_CATALOG: &[&str] = &[
    include_str!("../../../providers/catalog/anthropic.toml"),
    include_str!("../../../providers/catalog/deepseek.toml"),
    include_str!("../../../providers/catalog/gemini-openai-compat.toml"),
    include_str!("../../../providers/catalog/gemini.toml"),
    include_str!("../../../providers/catalog/groq.toml"),
    include_str!("../../../providers/catalog/lmstudio.toml"),
    include_str!("../../../providers/catalog/mistral.toml"),
    include_str!("../../../providers/catalog/ollama.toml"),
    include_str!("../../../providers/catalog/openrouter.toml"),
    include_str!("../../../providers/catalog/together.toml"),
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

fn secret_backend_label(kind: secret_store::BackendKind) -> &'static str {
    match kind {
        secret_store::BackendKind::Keyring => "OS keychain",
        secret_store::BackendKind::File => "encrypted file",
    }
}

fn proxy_state_label(state: &remote_api::ProxyState) -> String {
    match state {
        remote_api::ProxyState::Connected => "connected".to_owned(),
        remote_api::ProxyState::Reconnecting => "still connecting".to_owned(),
        remote_api::ProxyState::Unreachable(reason) => format!("unreachable: {reason}"),
    }
}

fn data_dir(cli: &Cli) -> Result<PathBuf> {
    cli.data_dir
        .clone()
        .or_else(storage::data_dir)
        .ok_or_else(|| {
            "cannot determine the data directory; pass `--data-dir <PATH>` or set `BOT_DATA_DIR`"
                .into()
        })
}

fn read_value(env_var: Option<&str>, prompt: &str) -> Result<SecretValue> {
    if let Some(v) = env_var {
        let val = std::env::var(v).map_err(|_| {
            format!(
                "environment variable `{v}` is not set; export it first or omit the `*-env` flag to enter the value interactively"
            )
        })?;
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
        println!(
            "Praxi-Bot CLI. Run `bot setup` to configure a provider and first bot, or `bot --help` to list commands."
        );
        return Ok(());
    };
    match command {
        Command::Completions { shell } => return Ok(cli_artifacts::completions(*shell)?),
        Command::ManPages { output_dir } => return Ok(cli_artifacts::man_pages(output_dir)?),
        Command::Catalog { action }
            if !matches!(action, catalog_cmd::CatalogCmd::Diff { live: true, .. }) =>
        {
            return catalog_cmd::run_offline(
                action,
                cli.catalog_dir.as_deref(),
                BUILTIN_CATALOG,
                cli.json,
                std::time::SystemTime::now(),
                &mut std::io::stdout().lock(),
            );
        }
        _ => {}
    }
    let data_dir = data_dir(&cli)?;
    let mut say = |s: &str| println!("{s}");

    if cli.remote.is_some()
        && !matches!(
            command,
            Command::Host {
                action: host::HostCmd::Invite { .. }
                    | host::HostCmd::Status
                    | host::HostCmd::Devices
                    | host::HostCmd::Revoke { .. }
                    | host::HostCmd::Standby
                    | host::HostCmd::Activate { .. }
            } | Command::Status
                | Command::Proxy
                | Command::Chat { .. }
                | Command::Bots { .. }
                | Command::Profile { .. }
                | Command::Approve { .. }
                | Command::Answer { .. }
                | Command::Schedules { .. }
                | Command::Workflow { .. }
                | Command::Email { .. }
                | Command::Grants { .. }
                | Command::Channels {
                    action: ChannelsCmd::Telegram {
                        action: TelegramCmd::Pair
                    }
                }
        )
    {
        return Err(
            "`--remote` is not supported for this local command; supported remote commands are `status`, `proxy`, `bots`, `chat`, `approve`, `answer`, `grants`, `schedules`, `workflow`, `email`, `channels telegram pair`, and `host status|devices|invite|revoke|standby|activate`".into(),
        );
    }

    if cli.remote.is_none() && matches!(command, Command::Proxy) {
        return Err(
            "`bot proxy` requires `--remote <ENDPOINT>`; run `bot --remote <ENDPOINT> proxy`"
                .into(),
        );
    }

    if let Command::Host {
        action: host::HostCmd::Join { ticket },
    } = command
    {
        return current_thread_runtime()?.block_on(host::join(&data_dir, ticket));
    }

    // Provisioning runs in the owner's terminal and needs no secret store.
    if let Command::Host {
        action: host::HostCmd::Provision(cmd),
    } = command
    {
        return host_provision::run(cmd, &data_dir, &|| make_client(&cli, &data_dir));
    }

    // Daemon commands (no secret store needed, except `serve` with real providers).
    match command {
        Command::Skills { action } => {
            return skills_cmd::run(&data_dir, action, &mut std::io::stdout());
        }
        Command::Mcp {
            action: McpCmd::Serve(args),
        } => return mcp_serve::run(&cli, args, &data_dir),
        Command::Token { action } => {
            let client = make_client(&cli, &data_dir)?;
            let result =
                current_thread_runtime()?.block_on(mcp_serve::token_command(&client, action))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Command::Serve { .. } => return run_serve(&cli, command, &data_dir),
        Command::Doctor(c) => return run_doctor(&cli, c, &data_dir),
        Command::Backup { action } if !action.needs_secret_store() => {
            return backup_cmd::run(
                action,
                &data_dir,
                cli.port,
                &secret_store::MemoryStore::new(),
                cli.json,
                &mut std::io::stdout(),
            );
        }
        Command::SelfUpdate(args) => return update::run(args),
        Command::Host { .. }
        | Command::Status
        | Command::Proxy
        | Command::Chat { .. }
        | Command::Chats { .. }
        | Command::Bots { .. }
        | Command::Profile { .. }
        | Command::Approve { .. }
        | Command::Answer { .. }
        | Command::Schedules { .. }
        | Command::Workflow { .. }
        | Command::Email { .. }
        | Command::Mcp { .. }
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
    eprintln!("(secret backend: {})", secret_backend_label(kind));
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
        Command::Backup { action } => backup_cmd::run(
            action,
            &data_dir,
            cli.port,
            store,
            cli.json,
            &mut std::io::stdout(),
        ),
        Command::Restore(c) => backup_cmd::restore(
            &data_dir,
            cli.port,
            store,
            c,
            cli.json,
            &mut std::io::stdout(),
        ),
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
        Command::Catalog {
            action:
                catalog_cmd::CatalogCmd::Diff {
                    provider,
                    live: true,
                    metadata,
                    ..
                },
        } => catalog_cmd::run_live_diff(
            provider,
            metadata.as_deref(),
            &data_dir,
            &catalog,
            store,
            cli.json,
            &mut std::io::stdout().lock(),
        ),
        Command::Catalog { .. }
        | Command::Mcp { .. }
        | Command::Completions { .. }
        | Command::ManPages { .. }
        | Command::Skills { .. }
        | Command::Token { .. }
        | Command::Host { .. }
        | Command::SelfUpdate(_)
        | Command::Doctor(_)
        | Command::Serve { .. }
        | Command::Status
        | Command::Proxy
        | Command::Chat { .. }
        | Command::Chats { .. }
        | Command::Bots { .. }
        | Command::Profile { .. }
        | Command::Approve { .. }
        | Command::Answer { .. }
        | Command::Schedules { .. }
        | Command::Workflow { .. }
        | Command::Email { .. }
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
            ProvidersCmd::DetectLocal => commands::providers_detect_local(&mut say),
            ProvidersCmd::List | ProvidersCmd::Default { .. } => Ok(()),
        },
    }
}

/// Coding CLIs on this machine, for `bot doctor` (one probe, at most 10 s).
fn detect_cli_accounts() -> Option<Vec<cli_provider::DetectedCli>> {
    let rt = current_thread_runtime().ok()?;
    rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            cli_provider::Detector::new().detect(true),
        )
        .await
        .ok()
    })
}

/// `bot doctor`: never prompts, opens the secret store read-only.
fn run_doctor(cli: &Cli, c: &DoctorCmd, data_dir: &std::path::Path) -> Result<()> {
    let catalog = load_catalog(cli.catalog_dir.as_ref()).map_err(|e| e.to_string());
    let pass = std::env::var("BOT_SECRETS_PASSPHRASE").ok();
    let store = open_store(cli.secrets_backend, data_dir, &|_| pass.clone())
        .map(|(s, k)| (s, secret_backend_label(k).to_owned()))
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
        detected: detect_cli_accounts(),
        desktop: serve::local_desktop(&mut |_| {}).map(|d| d.capabilities()),
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
            format!("{fails} doctor check(s) failed; follow the fix hints above"),
        ));
    }
    Ok(())
}

fn run_serve(cli: &Cli, command: &Command, data_dir: &std::path::Path) -> Result<()> {
    use std::sync::Arc;

    // First thing: restrict the daemon process before secrets are read or
    // children are spawned. Never fatal; the result is recorded in `serve`
    // and shown by `doctor`.
    // Held for the whole run so `bot backup restore` can tell a daemon is using the data dir.
    let _daemon_lock = backup_cmd::hold_daemon_lock(data_dir)?;
    let guard = process_guard::protect();
    if guard.outcome == process_guard::Outcome::Failed {
        eprintln!("warning: DEGRADED process guard: {}", guard.detail);
    }

    let (host_mode, peer_enabled) = match command {
        Command::Serve { host, peer, .. } => (*host, *peer),
        _ => (false, false),
    };
    let safe_mode = matches!(command, Command::Serve { safe: true, .. })
        || (matches!(command, Command::Serve { .. })
            && daemon_api::doctor::safe_mode_from_env(
                std::env::var("BOT_SAFE_MODE").ok().as_deref(),
            ));
    if safe_mode && (host_mode || peer_enabled) {
        return Err(
            "safe mode cannot be combined with --host or --peer (it keeps peer networking off); start without them, or without --safe / BOT_SAFE_MODE"
                .into(),
        );
    }
    if safe_mode {
        eprintln!(
            "SAFE MODE: tools, schedules, channels, MCP servers and apps are off; nothing was changed. Restart without --safe (and BOT_SAFE_MODE) to leave it."
        );
    }
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
            owner_key: None,
            safe_mode,
        };
        return serve::serve(data_dir, &opts, providers, None, Some(desktop));
    }
    let owner_key = if matches!(
        command,
        Command::Serve {
            owner_key_stdin: true,
            ..
        }
    ) {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let key = line.trim().to_owned();
        (key.len() >= 32).then_some(key)
    } else {
        None
    };
    let (store, kind) = open_store(cli.secrets_backend, data_dir, &ask_passphrase)?;
    eprintln!("(secret backend: {})", secret_backend_label(kind));
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
    // Safe mode loads no desktop backend: nothing can touch the screen.
    let desktop = if safe_mode {
        None
    } else {
        serve::local_desktop(&mut |w| eprintln!("warning: {w}"))
    };
    let opts = serve::ServeOptions {
        port: cli.port,
        json: cli.json,
        grace,
        telegram_store: Some(telegram_store),
        host_mode,
        peer_enabled,
        owner_key,
        safe_mode,
    };
    serve::serve(data_dir, &opts, providers, Some(host), desktop)
}

/// A client of the daemon: token from `BOT_API_TOKEN` or the token file,
/// address from `--url` / `--port`.
fn make_client(cli: &Cli, data_dir: &std::path::Path) -> Result<Client> {
    let token = match std::env::var("BOT_API_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_owned(),
        _ => daemon_api::read_token(data_dir).map_err(|e| {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                "daemon API token not found in this data directory; start the daemon with `bot serve` (or pass the same `--data-dir` / set `BOT_API_TOKEN`)".to_owned()
            } else {
                format!(
                    "cannot read daemon API token ({}): {e}; check file permissions, restart `bot serve`, or set `BOT_API_TOKEN`",
                    daemon_api::token_path(data_dir).display()
                )
            };
            client::fail(client::exit::AUTH, msg)
        })?,
    };
    let base = cli
        .url
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", cli.port));
    // The owner key of this data dir: present when the daemon is a terminal
    // `bot serve` or a host, absent for a daemon the app started (memory only).
    Ok(Client::new(&base, token).with_owner_key(daemon_api::read_owner_key(data_dir).ok()))
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

/// `bot --remote <ENDPOINT> proxy`: print the tunnel's loopback address and token, hold it open.
async fn proxy_hold(
    proxy: Option<&remote_api::ApiProxy>,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let Some(proxy) = proxy else {
        return Err(
            "`bot proxy` requires `--remote <ENDPOINT>`; run `bot --remote <ENDPOINT> proxy`"
                .into(),
        );
    };
    let mut state = proxy.state.clone();
    let up = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        state.wait_for(|s| matches!(s, remote_api::ProxyState::Connected)),
    )
    .await
    .is_ok_and(|r| r.is_ok());
    if !up {
        let now = state.borrow().clone();
        return Err(format!(
            "remote tunnel did not connect within 20 s ({}); verify the endpoint ID and check that the host is online",
            proxy_state_label(&now)
        )
        .into());
    }
    writeln!(out, "url: {}", proxy.base_url)?;
    writeln!(out, "token: {}", proxy.token)?;
    writeln!(out, "tunnel open; press Ctrl-C to close it")?;
    out.flush()?;
    tokio::signal::ctrl_c().await?;
    Ok(())
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
            Command::Host { action } => host::run(&c, action, json, &mut out, data_dir).await,
            Command::Status => remote::status(&c, json, &mut out).await,
            Command::Proxy => proxy_hold(remote.as_ref().map(|(_, p)| p), &mut out).await,
            Command::Bots {
                action: BotsCmd::List,
            } => remote::bots_list(&c, json, &mut out).await,
            Command::Bots {
                action: BotsCmd::Add { file },
            } => remote::bots_add(&c, file, json, &mut out).await,
            Command::Profile { action } => profile_cmd::run(&c, action, json, &mut out).await,
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
            Command::Chats { action } => chats_cmd::run(&c, action, json, &mut out).await,
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
            Command::Workflow { action } => workflow::run(&c, action, &mut out).await,
            Command::Email { action } => run_email(&c, action, json, &mut out).await,
            Command::Mcp { action } => run_mcp(&c, action, json, &mut out).await,
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

async fn run_mcp(
    c: &Client,
    action: &McpCmd,
    json: bool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let McpCmd::Add {
        name,
        dry_run,
        input,
    } = action
    else {
        return Err("`mcp serve` runs before a daemon client exists".into());
    };
    let input = if input.as_slice() == ["-"] {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        vec![text]
    } else {
        input.clone()
    };
    let parsed =
        mcp_paste::parse_argv(&input, name.as_deref()).map_err(|e| email_cmd::rejected(&e))?;
    mcp_paste::add(c, &parsed, *dry_run, json, out).await
}

async fn run_email(
    c: &Client,
    action: &EmailCmd,
    json: bool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    match action {
        EmailCmd::List => email_cmd::list(c, json, out).await,
        EmailCmd::Add {
            name,
            address,
            name_flag,
            provider,
            name_hint,
            domain,
            api_key_env,
            preset,
            imap_host,
            imap_port,
            smtp_host,
            smtp_port,
            smtp_security,
            username,
            bot,
            all_bots: _,
            allowed_senders,
            password_env,
        } => {
            let Some(account) = name.as_deref().or(name_flag.as_deref()) else {
                return Err(email_cmd::rejected("an account name is required"));
            };
            if provider.as_deref() == Some("agentmail") {
                let hint = name_hint.as_deref().unwrap_or(account);
                let add = email_cmd::AgentmailAdd {
                    name: account,
                    name_hint: hint,
                    domain: domain.as_deref(),
                    bot: bot.as_deref(),
                    allowed_senders,
                };
                let key = read_value(
                    api_key_env.as_deref(),
                    "AgentMail API key (from your own AgentMail account); input hidden",
                )?;
                return email_cmd::add_agentmail(c, &add, key.expose(), json, out).await;
            }
            let Some(address) = address.as_deref() else {
                return Err(email_cmd::rejected("an address is required"));
            };
            let add = email_cmd::EmailAdd {
                name: account,
                address,
                preset: preset.as_deref(),
                imap_host: imap_host.as_deref(),
                imap_port: *imap_port,
                smtp_host: smtp_host.as_deref(),
                smtp_port: *smtp_port,
                smtp_security: smtp_security.as_deref(),
                username: username.as_deref(),
                bot: bot.as_deref(),
                allowed_senders,
            };
            // Validate the servers before asking for a password.
            email_cmd::add_body(&add, "")?;
            let password = read_value(
                password_env.as_deref(),
                &format!("Password (or app password) for {address}; input hidden"),
            )?;
            email_cmd::add(c, &add, password.expose(), json, out).await
        }
        EmailCmd::Rm { name, delete_inbox } => {
            email_cmd::rm_with(c, name, *delete_inbox, json, out).await
        }
        EmailCmd::Test { name } => email_cmd::test(c, name, json, out).await,
    }
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
            return Err(
                "`--yes` requires `--provider <ID>` for non-interactive setup; omit `--yes` for interactive setup or see `bot setup --help`".into(),
            );
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
        println!("Secret:   {s} (API key stored in secret backend)");
    }
    match out.bot_id {
        Some(id) => {
            println!("Bot:      {id}");
            println!(
                "Next: run `bot serve` to start the daemon, then `bot chat {id} \"Hello\"` to chat."
            );
        }
        None => println!(
            "No default model was selected, so no bot was created. Set one with `bot providers default {} --model <MODEL>`, then create a bot with `bot bots add <FILE>`.",
            out.provider_id
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn self_update_flags_parse_and_rollback_conflicts_are_rejected() {
        let cli =
            Cli::try_parse_from(["bot", "self-update", "--version", "1.2.3", "--check"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::SelfUpdate(update::UpdateArgs { check: true, version: Some(v), .. })) if v == "1.2.3")
        );
        assert!(Cli::try_parse_from(["bot", "self-update", "--rollback", "--check"]).is_err());
        assert!(
            Cli::try_parse_from(["bot", "self-update", "--rollback", "--version", "1.0.0"])
                .is_err()
        );
    }

    #[test]
    fn mcp_add_takes_the_paste_verbatim_after_its_options() {
        let cli = Cli::try_parse_from([
            "bot",
            "mcp",
            "add",
            "--name",
            "files",
            "--dry-run",
            "claude",
            "mcp",
            "add",
            "x",
            "-e",
            "A=b c",
            "--",
            "npx",
            "-y",
            "pkg@1.0.0",
        ])
        .unwrap();
        let Some(Command::Mcp {
            action:
                McpCmd::Add {
                    name,
                    dry_run,
                    input,
                },
        }) = cli.command
        else {
            panic!("not `mcp add`");
        };
        assert_eq!(name.as_deref(), Some("files"));
        assert!(dry_run);
        assert_eq!(
            input,
            [
                "claude",
                "mcp",
                "add",
                "x",
                "-e",
                "A=b c",
                "--",
                "npx",
                "-y",
                "pkg@1.0.0"
            ]
        );
        assert!(Cli::try_parse_from(["bot", "mcp", "add"]).is_err());
    }

    #[test]
    fn parses_host_commands() {
        for args in [
            vec!["status"],
            vec!["devices"],
            vec!["standby"],
            vec!["activate"],
            vec!["activate", "--force"],
            vec!["invite", "--role", "phone"],
            vec!["invite", "--role", "desktop"],
            vec!["invite", "--role", "node"],
            vec!["invite", "--role", "host"],
            vec!["join", "bot-pair:TEST"],
            vec!["revoke", "key"],
            vec![
                "add",
                "ubuntu@203.0.113.7",
                "--ssh-port",
                "2222",
                "--key",
                "k",
                "--yes",
            ],
            vec!["repair", "opc@203.0.113.7"],
            vec!["upgrade", "oracle"],
            vec!["remove", "oracle", "--purge", "--yes"],
        ] {
            let cli = Cli::try_parse_from(["bot", "host"].into_iter().chain(args)).unwrap();
            assert!(matches!(cli.command, Some(Command::Host { .. })));
        }
        assert!(Cli::try_parse_from(["bot", "host", "invite", "--role", "admin"]).is_err());
        assert!(Cli::try_parse_from(["bot", "host", "join"]).is_err());
        assert!(
            Cli::try_parse_from(["bot", "host", "remove", "x", "--purge", "--uninstall"]).is_err()
        );
    }

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
    fn parses_catalog_commands() {
        for args in [
            vec!["check"],
            vec!["check", "--max-age-days", "30", "--strict"],
            vec!["diff", "anthropic", "snap.json"],
            vec!["diff", "anthropic", "--live"],
            vec!["render-docs"],
        ] {
            let cli = Cli::try_parse_from(["bot", "catalog"].into_iter().chain(args)).unwrap();
            assert!(matches!(cli.command, Some(Command::Catalog { .. })));
        }
        // Snapshot path and --live are mutually exclusive.
        assert!(
            Cli::try_parse_from(["bot", "catalog", "diff", "anthropic", "snap.json", "--live"])
                .is_err()
        );
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
    fn parses_email_commands() {
        let parsed = |args: &[&str]| Cli::try_parse_from(args).unwrap().command.unwrap();
        assert!(matches!(
            parsed(&["bot", "email", "list"]),
            Command::Email {
                action: EmailCmd::List
            }
        ));
        let Command::Email {
            action:
                EmailCmd::Add {
                    name,
                    address,
                    preset,
                    bot,
                    allowed_senders,
                    password_env,
                    ..
                },
        } = parsed(&[
            "bot",
            "email",
            "add",
            "work",
            "me@gmail.com",
            "--preset",
            "gmail",
            "--bot",
            "b1",
            "--allowed-sender",
            "a@example.com",
            "--allowed-sender",
            "@corp.example",
            "--password-env",
            "MAIL_PW",
        ])
        else {
            panic!("not an email add");
        };
        assert_eq!(
            (name.as_deref(), address.as_deref()),
            (Some("work"), Some("me@gmail.com"))
        );
        assert_eq!(preset.as_deref(), Some("gmail"));
        assert_eq!(bot.as_deref(), Some("b1"));
        assert_eq!(allowed_senders, ["a@example.com", "@corp.example"]);
        assert_eq!(password_env.as_deref(), Some("MAIL_PW"));
        assert!(matches!(
            parsed(&["bot", "email", "rm", "work"]),
            Command::Email {
                action: EmailCmd::Rm { .. }
            }
        ));
        assert!(matches!(
            parsed(&["bot", "email", "test", "work"]),
            Command::Email {
                action: EmailCmd::Test { .. }
            }
        ));
        // No implicit "every bot": a bot or `--all-bots` must be chosen, not both.
        let base = ["bot", "email", "add", "w", "a@b.c", "--preset", "gmail"];
        assert!(Cli::try_parse_from(base).is_err(), "no scope given");
        let with = |extra: &[&str]| Cli::try_parse_from(base.iter().chain(extra));
        assert!(with(&["--all-bots"]).is_ok());
        assert!(with(&["--bot", "Atlas"]).is_ok());
        assert!(with(&["--all-bots", "--bot", "Atlas"]).is_err());
        // Unknown presets and a missing address are refused by the parser.
        assert!(
            Cli::try_parse_from(["bot", "email", "add", "w", "a@b.c", "--preset", "aol"]).is_err()
        );
        assert!(Cli::try_parse_from(["bot", "email", "add", "w"]).is_err());
        // The password is never a command-line flag (it would land in shell history).
        assert!(
            Cli::try_parse_from(["bot", "email", "add", "w", "a@b.c", "--password", "x"]).is_err()
        );
    }

    #[test]
    fn parses_the_agentmail_form_and_keeps_the_key_out_of_argv() {
        let ok = Cli::try_parse_from([
            "bot",
            "email",
            "add",
            "--provider",
            "agentmail",
            "--name",
            "helper",
            "--bot",
            "Atlas",
            "--api-key-env",
            "AGENTMAIL_KEY",
        ])
        .unwrap()
        .command
        .unwrap();
        let Command::Email {
            action:
                EmailCmd::Add {
                    name,
                    name_flag,
                    address,
                    provider,
                    api_key_env,
                    ..
                },
        } = ok
        else {
            panic!("not an email add");
        };
        assert_eq!(name, None);
        assert_eq!(name_flag.as_deref(), Some("helper"));
        assert_eq!(address, None);
        assert_eq!(provider.as_deref(), Some("agentmail"));
        assert_eq!(api_key_env.as_deref(), Some("AGENTMAIL_KEY"));
        // Still a bot or --all-bots, never a default; never a key flag.
        assert!(
            Cli::try_parse_from([
                "bot",
                "email",
                "add",
                "--provider",
                "agentmail",
                "--name",
                "h"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "bot",
                "email",
                "add",
                "--provider",
                "agentmail",
                "--name",
                "h",
                "--all-bots",
                "--api-key",
                "am_x"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "bot",
                "email",
                "add",
                "--provider",
                "agentmail",
                "--name",
                "h",
                "--all-bots",
                "--provider",
                "nope"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["bot", "email", "rm", "h", "--delete-inbox"]).is_ok(),
            "remote deletion is an explicit flag"
        );
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
        let cli = Cli::try_parse_from(["bot", "--remote", "endpoint", "proxy"]).unwrap();
        assert_eq!(cli.remote.as_deref(), Some("endpoint"));
        assert!(matches!(cli.command, Some(Command::Proxy)));
        assert!(
            Cli::try_parse_from([
                "bot",
                "--remote",
                "endpoint",
                "--url",
                "http://127.0.0.1",
                "proxy"
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
        // `bot profile import` defaults to the safest ceiling and needs --yes to create.
        match Cli::try_parse_from(["bot", "profile", "import", "p.praxi-bot.json"])
            .unwrap()
            .command
        {
            Some(Command::Profile {
                action:
                    profile_cmd::ProfileCmd::Import {
                        level, yes: false, ..
                    },
            }) => assert_eq!(level, profile_cmd::Level::ReadOnly),
            other => panic!("unexpected parse: {}", other.is_some()),
        }
        assert!(
            Cli::try_parse_from(["bot", "profile", "import", "p.json", "--level", "root"]).is_err()
        );
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

    #[test]
    fn every_visible_command_has_a_description_and_root_help_lists_exit_codes() {
        fn check_cmd(cmd: &clap::Command, path: &str) {
            for sub in cmd.get_subcommands() {
                if sub.is_hide_set() || sub.get_name() == "help" {
                    continue;
                }
                let full = format!("{path} {}", sub.get_name());
                let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
                assert!(!about.trim().is_empty(), "missing description for `{full}`");
                assert!(
                    about.chars().next().is_some_and(char::is_uppercase),
                    "description for `{full}` should start with a capital letter: {about:?}"
                );
                check_cmd(sub, &full);
            }
        }
        let mut root = Cli::command();
        check_cmd(&root, "bot");
        let help = root.render_long_help().to_string();
        assert!(help.contains("Exit codes:"), "{help}");
        for code in ["0  OK", "3  Daemon unreachable", "6  Request rejected"] {
            assert!(
                help.contains(code),
                "missing exit code `{code}` in:\n{help}"
            );
        }
    }

    #[tokio::test]
    async fn setup_required_messages_explain_why_and_next_step() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let dir = tempfile::tempdir().unwrap();
        let catalog = load_catalog(None).unwrap();
        let store = secret_store::MemoryStore::new();
        let mut warnings = Vec::new();
        let providers = serve::build_providers(dir.path(), &catalog, &store, &mut |w| {
            warnings.push(w.to_owned());
        })
        .unwrap();
        assert!(providers.setup_required());
        assert!(
            warnings.iter().any(|w| w.contains("no LLM provider")
                && w.contains("`bot setup`")
                && w.contains("`bot providers add")),
            "{warnings:?}"
        );

        let checks = doctor::run_checks(&doctor::DoctorEnv {
            data_dir: dir.path(),
            catalog: Ok(&catalog),
            store: Ok((&store, "encrypted file".into())),
            port: 1,
            online: false,
            browser: None,
            browser_error: None,
            detected: None,
            desktop: None,
        });
        let prov = checks.iter().find(|c| c.title == "providers").unwrap();
        assert!(prov.detail.contains("no LLM provider"), "{}", prov.detail);
        let hint = prov.hint.as_deref().unwrap_or_default();
        assert!(
            hint.contains("`bot setup`") && hint.contains("`bot providers add"),
            "{hint}"
        );

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "setup_required",
                "version": "0.1.0",
                "uptime_secs": 3
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/approvals"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chats/c1/messages"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": { "code": "setup_required", "message": "setup required" }
            })))
            .mount(&server)
            .await;

        let client = Client::new(&server.uri(), "tok".into());
        let mut out = Vec::new();
        remote::status(&client, false, &mut out).await.unwrap();
        let status_text = String::from_utf8(out).unwrap();
        assert!(
            status_text.contains("no LLM provider") && status_text.contains("`bot setup`"),
            "{status_text}"
        );

        let err = client
            .request(reqwest::Method::POST, "/v1/chats/c1/messages", None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no LLM provider")
                && msg.contains("`bot setup`")
                && msg.contains("`bot providers add"),
            "{msg}"
        );
        assert_eq!(
            err.downcast_ref::<ExitError>().map(|e| e.code),
            Some(client::exit::REJECTED)
        );
    }

    #[test]
    fn user_facing_labels_avoid_raw_debug_names() {
        assert_eq!(
            secret_backend_label(secret_store::BackendKind::Keyring),
            "OS keychain"
        );
        assert_eq!(
            secret_backend_label(secret_store::BackendKind::File),
            "encrypted file"
        );
        assert_eq!(
            proxy_state_label(&remote_api::ProxyState::Reconnecting),
            "still connecting"
        );
        assert_eq!(
            proxy_state_label(&remote_api::ProxyState::Unreachable("timeout".into())),
            "unreachable: timeout"
        );
    }
}
#[cfg(test)]
mod rag_wiring_tests;
