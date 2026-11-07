//! Command-line interface entry point for the Praxi-Bot agent (`bot`).

mod commands;
mod presets;
mod prompt;
mod wizard;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use provider_catalog::{Catalog, ProviderEntry};
use secret_store::{BackendChoice, SecretStore, SecretValue, open_store};

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
    include_str!("../../../providers/catalog/deepseek.toml"),
    include_str!("../../../providers/catalog/gemini-openai-compat.toml"),
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
        std::process::exit(1);
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
