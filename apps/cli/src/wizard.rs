//! `bot setup` logic, separated from terminal IO.
//!
//! - [`run_wizard`] drives a [`Prompter`] and returns a [`SetupPlan`].
//! - [`plan_from_args`] builds a plan from flags (non-interactive).
//! - [`apply_plan`] performs the side effects (secret store, `providers.toml`,
//!   bot record, settings file).

use std::path::{Path, PathBuf};

use protocol::{BotId, UserId};
use provider_catalog::{
    Auth, Catalog, CompatFlags, ModelCapabilities, ModelRef, OpenAiCompatConfig, ProviderConfig,
    ProviderEntry, ProviderKind, ProviderSettings, api_key_secret_name, build_chat_provider,
    config_path, test_connection,
};
use secret_store::{SecretRef, SecretStore, SecretValue};
use storage::{Bot, Storage, User, now_ms};

use crate::{
    Result,
    presets::{BotPreset, Lang, PermissionPreset, bot_config},
    prompt::Prompter,
};

/// Models listed above this count are asked as free text instead of a menu.
const MENU_LIMIT: usize = 30;

/// Which provider the user picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderChoice {
    /// A catalog entry by id.
    Catalog(String),
    /// A custom OpenAI-compatible endpoint.
    Custom {
        /// Id to register it under.
        id: String,
        /// Base URL (e.g. `http://localhost:8000/v1`).
        base_url: String,
    },
}

/// Everything decided by the user; applied in one go.
#[derive(Debug)]
pub struct SetupPlan {
    /// Wizard language (saved for next time).
    pub lang: Lang,
    /// Provider.
    pub provider: ProviderChoice,
    /// API key to store (never printed).
    pub key: Option<SecretValue>,
    /// Chosen default model.
    pub model: Option<String>,
    /// First bot preset.
    pub bot_preset: BotPreset,
    /// First bot name.
    pub bot_name: String,
    /// Default permission preset.
    pub permission: PermissionPreset,
}

/// What `apply_plan` did.
#[derive(Debug)]
pub struct SetupOutcome {
    /// Provider id.
    pub provider_id: String,
    /// Secret name the key was stored under.
    pub secret_name: Option<String>,
    /// `provider/model` when a model was chosen.
    pub model_ref: Option<ModelRef>,
    /// Whether this provider is now the effective global default.
    pub is_default: bool,
    /// The created bot (none when no model was chosen).
    pub bot_id: Option<BotId>,
}

/// Connection test seam (real network in production, fake in tests).
pub trait Connector {
    /// Test the provider and return the model ids it offers.
    ///
    /// # Errors
    /// A human-readable failure message.
    fn test(
        &self,
        entry: &ProviderEntry,
        key: Option<&SecretValue>,
        model: Option<&ModelRef>,
    ) -> std::result::Result<Vec<String>, String>;
}

/// [`Connector`] that talks to the real provider.
pub struct NetConnector;

impl Connector for NetConnector {
    fn test(
        &self,
        entry: &ProviderEntry,
        key: Option<&SecretValue>,
        model: Option<&ModelRef>,
    ) -> std::result::Result<Vec<String>, String> {
        let provider = build_chat_provider(
            entry,
            OpenAiCompatConfig::default(),
            key.map(|k| k.expose().to_string()),
        )
        .map_err(|e| e.to_string())?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(test_connection(&*provider, model))
            .map(|r| r.models)
            .map_err(|e| e.to_string())
    }
}

/// Dependencies of the setup logic.
pub struct SetupEnv<'a> {
    /// Provider catalog.
    pub catalog: &'a Catalog,
    /// Secret store.
    pub store: &'a dyn SecretStore,
    /// Data directory.
    pub data_dir: &'a Path,
    /// Connection tester.
    pub connector: &'a dyn Connector,
}

/// Resolve the catalog entry for a choice.
///
/// # Errors
/// Unknown catalog id.
pub fn entry_for(choice: &ProviderChoice, catalog: &Catalog) -> Result<ProviderEntry> {
    match choice {
        ProviderChoice::Catalog(id) => catalog.get(id).cloned().ok_or_else(|| {
            format!("unknown provider {id:?}; use --base-url for a custom one").into()
        }),
        ProviderChoice::Custom { id, base_url } => Ok(custom_entry(id, base_url)),
    }
}

/// Entry for a custom OpenAI-compatible endpoint.
#[must_use]
pub fn custom_entry(id: &str, base_url: &str) -> ProviderEntry {
    let base = base_url.trim_end_matches('/').to_string();
    ProviderEntry {
        id: id.to_string(),
        kind: ProviderKind::OpenAiCompatible,
        models_endpoint: Some(format!("{base}/models")),
        models: Vec::new(),
        base_url: base,
        auth: Auth::None,
        capabilities: ModelCapabilities {
            tools: true,
            vision: false,
            streaming: true,
            json_mode: false,
        },
        compat_flags: CompatFlags::default(),
    }
}

fn sorted_catalog(catalog: &Catalog) -> Vec<&ProviderEntry> {
    let mut v: Vec<_> = catalog.all().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// Interactive wizard. Returns the plan; nothing is persisted until
/// [`apply_plan`].
///
/// # Errors
/// IO errors or the user aborting.
pub fn run_wizard(p: &mut dyn Prompter, env: &SetupEnv<'_>) -> Result<SetupPlan> {
    // 1. language
    let langs = vec!["English".to_string(), "Türkçe".to_string()];
    let lang = if p.select("Language / Dil", &langs, 0)? == 0 {
        Lang::En
    } else {
        Lang::Tr
    };
    let t = |en: &'static str, tr: &'static str| lang.t(en, tr).to_string();

    // 2. provider
    let entries = sorted_catalog(env.catalog);
    let mut items: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    items.push(t("Custom OpenAI-compatible URL", "Özel OpenAI uyumlu URL"));
    let idx = p.select(&t("Choose a provider", "Sağlayıcı seçin"), &items, 0)?;
    let provider = if idx < entries.len() {
        ProviderChoice::Catalog(entries[idx].id.clone())
    } else {
        let id = p.input(&t("Provider id", "Sağlayıcı kimliği"), Some("custom"))?;
        let base_url = p.input("Base URL (http://host:port/v1)", None)?;
        if base_url.trim().is_empty() {
            return Err("base URL is required for a custom provider".into());
        }
        ProviderChoice::Custom {
            id: id.trim().to_string(),
            base_url: base_url.trim().trim_end_matches('/').to_string(),
        }
    };
    let entry = entry_for(&provider, env.catalog)?;
    SecretRef::new(api_key_secret_name(&entry.id))?; // validates the id early

    // 3. key + 4. connection test
    let (key, models) = loop {
        let key =
            if matches!(entry.auth, Auth::None) && matches!(provider, ProviderChoice::Catalog(_)) {
                None
            } else {
                let k = p.password(&t(
                    "Paste the API key (hidden, empty = none)",
                    "API anahtarını yapıştırın (gizli, boş = yok)",
                ))?;
                let k = k.trim();
                (!k.is_empty()).then(|| SecretValue::new(k))
            };
        p.say(&t("Testing connection...", "Bağlantı test ediliyor..."));
        match env.connector.test(&entry, key.as_ref(), None) {
            Ok(models) => {
                p.say(&t("Connection OK.", "Bağlantı başarılı."));
                break (key, models);
            }
            Err(e) => {
                p.say(&format!(
                    "{}: {e}",
                    t("Connection failed", "Bağlantı başarısız")
                ));
                let choices = vec![
                    t("Try again", "Tekrar dene"),
                    t("Continue anyway", "Yine de devam et"),
                    t("Abort", "Vazgeç"),
                ];
                match p.select(&t("What now?", "Şimdi ne yapalım?"), &choices, 0)? {
                    0 => {}
                    1 => break (key, Vec::new()),
                    _ => return Err("setup aborted".into()),
                }
            }
        }
    };

    // 5. model
    let model = choose_model(p, lang, &models)?;

    // 6. first bot
    let labels: Vec<String> = BotPreset::ALL
        .iter()
        .map(|b| b.label(lang).to_string())
        .collect();
    let bot_preset = BotPreset::ALL[p.select(
        &t("Create your first bot", "İlk botunu oluştur"),
        &labels,
        0,
    )?];
    let bot_name = p.input(&t("Bot name", "Bot adı"), Some(bot_preset.default_name()))?;

    // 7. permissions
    let labels: Vec<String> = PermissionPreset::ALL
        .iter()
        .map(|b| b.label(lang).to_string())
        .collect();
    let permission = PermissionPreset::ALL[p.select(
        &t("Default permission preset", "Varsayılan izin ön ayarı"),
        &labels,
        0,
    )?];

    Ok(SetupPlan {
        lang,
        provider,
        key,
        model,
        bot_preset,
        bot_name: bot_name.trim().to_string(),
        permission,
    })
}

fn choose_model(p: &mut dyn Prompter, lang: Lang, models: &[String]) -> Result<Option<String>> {
    let prompt = lang.t("Default model", "Varsayılan model");
    if models.is_empty() {
        let m = p.input(
            lang.t(
                "Default model id (empty = decide later)",
                "Varsayılan model kimliği (boş = sonra)",
            ),
            None,
        )?;
        return Ok((!m.trim().is_empty()).then(|| m.trim().to_string()));
    }
    if models.len() > MENU_LIMIT {
        let m = p.input(prompt, Some(&models[0]))?;
        return Ok(Some(m.trim().to_string()));
    }
    let i = p.select(prompt, models, 0)?;
    Ok(Some(models[i].clone()))
}

/// Flags of the non-interactive mode.
#[derive(Debug, Clone)]
pub struct SetupArgs {
    /// Catalog id, or the id of a custom provider when `base_url` is given.
    pub provider: String,
    /// Base URL for a custom provider.
    pub base_url: Option<String>,
    /// Environment variable that holds the API key.
    pub key_env: Option<String>,
    /// Default model; the first offered model when omitted.
    pub model: Option<String>,
    /// Language.
    pub lang: Lang,
    /// Bot preset.
    pub bot_preset: BotPreset,
    /// Bot name.
    pub bot_name: Option<String>,
    /// Permission preset.
    pub permission: PermissionPreset,
}

/// Build a plan from flags, testing the connection on the way.
///
/// # Errors
/// Unknown provider, unset key variable, failed connection, no model available.
pub fn plan_from_args(
    args: &SetupArgs,
    env: &SetupEnv<'_>,
    read_env: &dyn Fn(&str) -> Option<String>,
) -> Result<SetupPlan> {
    let provider = match &args.base_url {
        Some(url) => ProviderChoice::Custom {
            id: args.provider.clone(),
            base_url: url.clone(),
        },
        None => ProviderChoice::Catalog(args.provider.clone()),
    };
    let entry = entry_for(&provider, env.catalog)?;
    SecretRef::new(api_key_secret_name(&entry.id))?;
    let key = match &args.key_env {
        Some(var) => match read_env(var).filter(|v| !v.trim().is_empty()) {
            Some(v) => Some(SecretValue::new(v.trim())),
            None => return Err(format!("environment variable {var} is not set").into()),
        },
        None => None,
    };
    let wanted = args
        .model
        .as_ref()
        .map(|m| ModelRef(format!("{}/{m}", entry.id)));
    let models = env
        .connector
        .test(&entry, key.as_ref(), wanted.as_ref())
        .map_err(|e| format!("connection test failed: {e}"))?;
    let model = match &args.model {
        Some(m) => Some(m.clone()),
        None => Some(
            models
                .first()
                .cloned()
                .ok_or("the provider lists no models; pass --model explicitly")?,
        ),
    };
    Ok(SetupPlan {
        lang: args.lang,
        provider,
        key,
        model,
        bot_preset: args.bot_preset,
        bot_name: args
            .bot_name
            .clone()
            .unwrap_or_else(|| args.bot_preset.default_name().to_string()),
        permission: args.permission,
    })
}

/// Store the key, register the provider in `providers.toml`, set its model.
/// Shared by `bot setup` and `bot providers add`.
///
/// # Errors
/// Secret store, config or catalog errors.
pub fn configure_provider(
    env: &SetupEnv<'_>,
    choice: &ProviderChoice,
    key: Option<&SecretValue>,
    model: Option<&str>,
) -> Result<(ProviderSettings, ProviderConfig)> {
    let entry = entry_for(choice, env.catalog)?;
    let path = config_path(env.data_dir);
    let _guard = ProviderConfig::lock(&path)?;
    let mut cfg = ProviderConfig::load(&path)?;
    let mut s = ProviderSettings::new(&entry.id, now_ms());
    if let Some(k) = key {
        let name = api_key_secret_name(&entry.id);
        env.store.set(&name, k.expose())?;
        s.key = Some(SecretRef::new(name)?);
    }
    if matches!(choice, ProviderChoice::Custom { .. }) {
        s.custom = Some(entry);
    }
    s.default_model = model.map(str::to_string);
    let id = s.id.clone();
    // Re-adding without a new key / model keeps what was there.
    if let Some(old) = cfg.get(&id) {
        if s.key.is_none() {
            s.key.clone_from(&old.key);
        }
        if s.default_model.is_none() {
            s.default_model.clone_from(&old.default_model);
        }
        if s.model_caps.is_empty() {
            s.model_caps.clone_from(&old.model_caps);
        }
    }
    cfg.upsert(s);
    cfg.save(&path)?;
    let settings = cfg
        .get(&id)
        .cloned()
        .ok_or("provider vanished after save")?;
    Ok((settings, cfg))
}

/// Persist everything in the plan.
///
/// # Errors
/// Secret store, config, or storage errors.
pub fn apply_plan(plan: &SetupPlan, env: &SetupEnv<'_>) -> Result<SetupOutcome> {
    let (settings, cfg) = configure_provider(
        env,
        &plan.provider,
        plan.key.as_ref(),
        plan.model.as_deref(),
    )?;
    let model_ref = settings.model_ref();

    // The bot is a real profile (`bot.id`, `models.executor`, ...) that the
    // agent loop loads; it needs a model. Without one only the provider is
    // saved, and the user adds a bot later (`bot bots add`).
    let bot_id = match &model_ref {
        Some(model) => {
            let storage = Storage::open(env.data_dir)?;
            let user_id = if let Some(u) = storage.list_users()?.into_iter().next() {
                u.id
            } else {
                let u = User {
                    id: UserId::new(),
                    name: "me".to_string(),
                    created_at_ms: now_ms(),
                };
                storage.insert_user(&u)?;
                u.id
            };
            let name = if plan.bot_name.is_empty() {
                plan.bot_preset.default_name().to_string()
            } else {
                plan.bot_name.clone()
            };
            let bot = Bot {
                id: BotId::new(),
                user_id,
                config: bot_config(plan.bot_preset, plan.permission, &name, &model.0, plan.lang),
                name,
                created_at_ms: now_ms(),
            };
            storage.insert_bot(&bot)?;
            Some(bot.id)
        }
        None => None,
    };
    save_language(env.data_dir, plan.lang)?;

    Ok(SetupOutcome {
        provider_id: settings.id.clone(),
        secret_name: settings.key.as_ref().map(|r| r.name().to_string()),
        model_ref,
        is_default: cfg.effective_default().is_some_and(|d| d.id == settings.id),
        bot_id,
    })
}

fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("cli-settings.json")
}

fn save_language(data_dir: &Path, lang: Lang) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(
        settings_path(data_dir),
        serde_json::to_string_pretty(&serde_json::json!({ "language": lang.code() }))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::scripted::{Answer, Scripted};
    use secret_store::MemoryStore;
    use std::cell::RefCell;

    struct FakeConnector {
        result: std::result::Result<Vec<String>, String>,
        seen_keys: RefCell<Vec<Option<String>>>,
    }

    impl FakeConnector {
        fn ok(models: &[&str]) -> Self {
            Self {
                result: Ok(models.iter().map(|s| (*s).to_string()).collect()),
                seen_keys: RefCell::default(),
            }
        }
    }

    impl Connector for FakeConnector {
        fn test(
            &self,
            _entry: &ProviderEntry,
            key: Option<&SecretValue>,
            _model: Option<&ModelRef>,
        ) -> std::result::Result<Vec<String>, String> {
            self.seen_keys
                .borrow_mut()
                .push(key.map(|k| k.expose().to_string()));
            self.result.clone()
        }
    }

    fn catalog() -> Catalog {
        let mk = |id: &str, auth: Auth| ProviderEntry {
            id: id.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example/v1"),
            auth,
            capabilities: ModelCapabilities {
                tools: true,
                vision: false,
                streaming: true,
                json_mode: false,
            },
            compat_flags: CompatFlags::default(),
            models_endpoint: None,
            models: Vec::new(),
        };
        Catalog::from_entries([
            mk("alpha", Auth::ApiKeyEnv("ALPHA_KEY".into())),
            mk("beta", Auth::ApiKeyEnv("BETA_KEY".into())),
            mk("local", Auth::None),
        ])
    }

    struct Fixture {
        dir: tempfile::TempDir,
        catalog: Catalog,
        store: MemoryStore,
        conn: FakeConnector,
    }

    impl Fixture {
        fn new(conn: FakeConnector) -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                catalog: catalog(),
                store: MemoryStore::new(),
                conn,
            }
        }

        fn env(&self) -> SetupEnv<'_> {
            SetupEnv {
                catalog: &self.catalog,
                store: &self.store,
                data_dir: self.dir.path(),
                connector: &self.conn,
            }
        }
    }

    fn txt(s: &str) -> Answer {
        Answer::Text(s.to_string())
    }

    #[test]
    fn wizard_happy_path_english() {
        let f = Fixture::new(FakeConnector::ok(&["m1", "m2"]));
        // lang EN, provider "beta"(1), key, model m2(1), preset browser(1), name default, perm default(0)
        let mut p = Scripted::new(vec![
            Answer::Select(0),
            Answer::Select(1),
            txt("  sk-secret-key-1234567890  "),
            Answer::Select(1),
            Answer::Select(1),
            txt(""),
            Answer::Select(0),
        ]);
        let plan = run_wizard(&mut p, &f.env()).unwrap();
        assert_eq!(plan.lang, Lang::En);
        assert_eq!(plan.provider, ProviderChoice::Catalog("beta".into()));
        assert_eq!(
            plan.key.as_ref().unwrap().expose(),
            "sk-secret-key-1234567890"
        );
        assert_eq!(plan.model.as_deref(), Some("m2"));
        assert_eq!(plan.bot_preset, BotPreset::Browser);
        assert_eq!(plan.bot_name, "Researcher");
        assert_eq!(plan.permission, PermissionPreset::ApproveDestructive);
        assert!(p.said.iter().any(|s| s == "Connection OK."));
        // The key never appears in anything the wizard printed.
        assert!(p.said.iter().all(|s| !s.contains("sk-secret")));
    }

    #[test]
    fn wizard_turkish_messages_and_custom_provider() {
        let f = Fixture::new(FakeConnector::ok(&[]));
        // lang TR, custom (index 3 = after 3 catalog entries), id, url, key empty,
        // model free text (no models listed), preset 0, name, perm 2
        let mut p = Scripted::new(vec![
            Answer::Select(1),
            Answer::Select(3),
            txt("mine"),
            txt("http://localhost:8000/v1/"),
            txt(""),
            txt("my-model"),
            Answer::Select(0),
            txt("Botum"),
            Answer::Select(2),
        ]);
        let plan = run_wizard(&mut p, &f.env()).unwrap();
        assert_eq!(plan.lang, Lang::Tr);
        assert_eq!(
            plan.provider,
            ProviderChoice::Custom {
                id: "mine".into(),
                base_url: "http://localhost:8000/v1".into()
            }
        );
        assert!(plan.key.is_none());
        assert_eq!(plan.model.as_deref(), Some("my-model"));
        assert_eq!(plan.permission, PermissionPreset::ReadOnly);
        assert!(p.said.iter().any(|s| s == "Bağlantı başarılı."));
    }

    #[test]
    fn keyless_catalog_provider_skips_key_prompt() {
        let f = Fixture::new(FakeConnector::ok(&["x"]));
        // alpha, beta, local -> local is index 2; no password prompt expected.
        let mut p = Scripted::new(vec![
            Answer::Select(0),
            Answer::Select(2),
            Answer::Select(0),
            Answer::Select(0),
            txt(""),
            Answer::Select(0),
        ]);
        let plan = run_wizard(&mut p, &f.env()).unwrap();
        assert!(plan.key.is_none());
        assert!(f.conn.seen_keys.borrow().iter().all(Option::is_none));
    }

    #[test]
    fn failed_connection_can_retry_then_abort() {
        let mut conn = FakeConnector::ok(&[]);
        conn.result = Err("401 unauthorized".into());
        let f = Fixture::new(conn);
        let mut p = Scripted::new(vec![
            Answer::Select(0),
            Answer::Select(0),
            txt("bad-key-1"),
            Answer::Select(0), // try again
            txt("bad-key-2"),
            Answer::Select(2), // abort
        ]);
        let err = run_wizard(&mut p, &f.env()).unwrap_err();
        assert!(err.to_string().contains("aborted"));
        assert_eq!(f.conn.seen_keys.borrow().len(), 2);
        assert!(p.said.iter().any(|s| s.contains("401 unauthorized")));
    }

    #[test]
    fn apply_stores_key_in_secret_store_not_in_config() {
        let f = Fixture::new(FakeConnector::ok(&["m1"]));
        let plan = SetupPlan {
            lang: Lang::En,
            provider: ProviderChoice::Catalog("alpha".into()),
            key: Some(SecretValue::new("sk-super-secret-value-123456")),
            model: Some("m1".into()),
            bot_preset: BotPreset::General,
            bot_name: "Helper".into(),
            permission: PermissionPreset::ReadAndDraft,
        };
        let out = apply_plan(&plan, &f.env()).unwrap();
        assert_eq!(out.provider_id, "alpha");
        assert!(out.is_default);
        assert_eq!(out.model_ref.unwrap().0, "alpha/m1");
        assert_eq!(
            f.store
                .get("provider.alpha.api_key")
                .unwrap()
                .unwrap()
                .expose(),
            "sk-super-secret-value-123456"
        );
        // Nothing on disk contains the key value.
        for e in std::fs::read_dir(f.dir.path()).unwrap().flatten() {
            if e.path().is_file() {
                let bytes = std::fs::read(e.path()).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains("sk-super-secret-value"),
                    "{} leaks the key",
                    e.path().display()
                );
            }
        }
        // Bot persisted with preset + permissions.
        let storage = Storage::open(f.dir.path()).unwrap();
        let user = storage.list_users().unwrap().remove(0);
        let bots = storage.list_bots(user.id).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].name, "Helper");
        // The stored config is a real profile the agent loop can load.
        let profile: agent_core::BotProfile =
            serde_json::from_value(bots[0].config.clone()).unwrap();
        assert_eq!(profile.name, "Helper");
        assert_eq!(profile.id, "helper");
        assert_eq!(profile.models.executor, "alpha/m1");
        assert_eq!(
            profile.permissions.preset.as_deref(),
            Some("read-and-draft")
        );
        assert!(profile.tool_visible("ask_user"));
    }

    /// Regression: the first bot saved by `bot setup` used a simplified config
    /// the agent loop could not load ("has no usable profile"). Run the
    /// non-interactive wizard and load the stored bot exactly as the loop does.
    #[test]
    fn setup_bot_loads_as_agent_profile_for_every_preset() {
        for preset in BotPreset::ALL {
            for perm in PermissionPreset::ALL {
                let f = Fixture::new(FakeConnector::ok(&["first", "second"]));
                let mut a = args("alpha");
                a.bot_preset = preset;
                a.permission = perm;
                a.lang = Lang::Tr;
                a.bot_name = Some("Benim Botum 2".into());
                let plan =
                    plan_from_args(&a, &f.env(), &|_| Some("sk-key-0123456789".into())).unwrap();
                let out = apply_plan(&plan, &f.env()).unwrap();

                let storage = Storage::open(f.dir.path()).unwrap();
                let bot = storage.get_bot(out.bot_id.unwrap()).unwrap().unwrap();
                let profile: agent_core::BotProfile = serde_json::from_value(bot.config)
                    .unwrap_or_else(|e| panic!("{preset:?}/{perm:?} has no usable profile: {e}"));
                assert_eq!(profile.models.executor, "alpha/first");
                assert_eq!(profile.id, "benim-botum-2");
                assert_eq!(profile.language.as_deref(), Some("tr"));
                assert_eq!(profile.permissions.preset.as_deref(), Some(perm.id()));
                assert!(profile.policy().is_some());
                assert!(profile.tool_visible("ask_user"));
                assert_eq!(
                    profile.tool_visible("browser_open"),
                    preset == BotPreset::Browser
                );
                assert_eq!(
                    profile.tool_visible("desktop_click"),
                    preset == BotPreset::Desktop
                );
                assert!(!profile.tool_visible("shell_run"));
                // Only the General preset reaches MCP servers; permissions (not
                // visibility) still gate every call.  No preset exposes email:
                // its read tools are prompt-free and would pair with `web_*`.
                assert_eq!(
                    profile.tool_visible("mcp:srv/tool"),
                    preset == BotPreset::General,
                    "{preset:?}"
                );
                for name in ["email_send", "email_read", "email_search"] {
                    assert!(!profile.tool_visible(name), "{preset:?} {name}");
                }
            }
        }
    }

    #[test]
    fn no_model_saves_provider_but_creates_no_bot() {
        let f = Fixture::new(FakeConnector::ok(&[]));
        let plan = SetupPlan {
            lang: Lang::En,
            provider: ProviderChoice::Catalog("alpha".into()),
            key: None,
            model: None,
            bot_preset: BotPreset::General,
            bot_name: "X".into(),
            permission: PermissionPreset::Full,
        };
        let out = apply_plan(&plan, &f.env()).unwrap();
        assert!(out.bot_id.is_none() && out.model_ref.is_none());
        assert!(
            ProviderConfig::load(&config_path(f.dir.path()))
                .unwrap()
                .get("alpha")
                .is_some()
        );
    }

    #[test]
    fn slug_handles_odd_names() {
        use crate::presets::slug;
        assert_eq!(slug("  My Bot!! 2 ", "x"), "my-bot-2");
        assert_eq!(slug("Çalışkan", "general"), "al-kan");
        assert_eq!(slug("!!!", "general"), "general");
    }

    #[test]
    fn first_provider_stays_default_when_second_is_added() {
        let f = Fixture::new(FakeConnector::ok(&["m"]));
        let env = f.env();
        configure_provider(
            &env,
            &ProviderChoice::Catalog("alpha".into()),
            None,
            Some("a"),
        )
        .unwrap();
        let (_, cfg) = configure_provider(
            &env,
            &ProviderChoice::Catalog("beta".into()),
            None,
            Some("b"),
        )
        .unwrap();
        assert_eq!(cfg.effective_default().unwrap().id, "alpha");
        // Re-adding alpha without a model keeps the earlier one.
        let (s, _) =
            configure_provider(&env, &ProviderChoice::Catalog("alpha".into()), None, None).unwrap();
        assert_eq!(s.default_model.as_deref(), Some("a"));
    }

    fn args(provider: &str) -> SetupArgs {
        SetupArgs {
            provider: provider.into(),
            base_url: None,
            key_env: Some("MY_KEY".into()),
            model: None,
            lang: Lang::En,
            bot_preset: BotPreset::General,
            bot_name: None,
            permission: PermissionPreset::ApproveDestructive,
        }
    }

    #[test]
    fn non_interactive_plan_reads_key_from_env_and_picks_first_model() {
        let f = Fixture::new(FakeConnector::ok(&["first", "second"]));
        let plan = plan_from_args(&args("alpha"), &f.env(), &|v| {
            (v == "MY_KEY").then(|| "sk-from-env-0000000000".to_string())
        })
        .unwrap();
        assert_eq!(plan.model.as_deref(), Some("first"));
        assert_eq!(
            plan.key.as_ref().unwrap().expose(),
            "sk-from-env-0000000000"
        );
        assert_eq!(plan.bot_name, "Assistant");
    }

    #[test]
    fn non_interactive_errors() {
        let f = Fixture::new(FakeConnector::ok(&[]));
        // unset env var
        let e = plan_from_args(&args("alpha"), &f.env(), &|_| None).unwrap_err();
        assert!(e.to_string().contains("MY_KEY"));
        // unknown provider without base url
        let e = plan_from_args(&args("nope"), &f.env(), &|_| Some("k".into())).unwrap_err();
        assert!(e.to_string().contains("unknown provider"));
        // no models listed and none passed
        let e = plan_from_args(&args("alpha"), &f.env(), &|_| Some("k".into())).unwrap_err();
        assert!(e.to_string().contains("--model"));
        // explicit model works even with an empty listing
        let mut a = args("alpha");
        a.model = Some("m".into());
        assert!(plan_from_args(&a, &f.env(), &|_| Some("k".into())).is_ok());
    }

    #[test]
    fn non_interactive_custom_provider() {
        let f = Fixture::new(FakeConnector::ok(&["m"]));
        let mut a = args("mine");
        a.base_url = Some("http://localhost:1/v1".into());
        a.key_env = None;
        let plan = plan_from_args(&a, &f.env(), &|_| None).unwrap();
        assert!(matches!(plan.provider, ProviderChoice::Custom { .. }));
        let out = apply_plan(&plan, &f.env()).unwrap();
        let cfg = ProviderConfig::load(&config_path(f.dir.path())).unwrap();
        assert!(cfg.get("mine").unwrap().custom.is_some());
        assert_eq!(out.model_ref.unwrap().0, "mine/m");
    }
}
