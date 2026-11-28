//! Data-driven ACP engines: one `providers/cli/<id>.toml` file per coding CLI that speaks
//! the Agent Client Protocol over stdio and needs no engine-specific Rust code.
//!
//! A file is untrusted-by-default configuration, not code. The schema is strict
//! (`deny_unknown_fields`), argv is fixed text with no interpolation of user, model or
//! prompt text, an approval level can only be mapped to the same or a *narrower* level (an
//! unsupported level falls back to Ask, or is marked not enforceable and refused), below
//! Full the argv and flags may only be tokens from a short allow-list of known-safe ones
//! (anything else is refused at load, not filtered by what it looks like), and an engine is
//! always experimental until a real run is verified. Its install command is unverified too,
//! so it is shown to copy and never run by Praxi. Everything an entry does not state is
//! listed under `unverified` instead of being guessed.
//!
//! Entries are compiled in (`include_str!`), parsed once, validated, and then leaked into
//! `'static` descriptors that the same `CliEngineSpec`, [`LevelPlan`] and launch code as the
//! built-in drivers use. An entry that fails validation is dropped (fail closed): the
//! `embedded_entries_are_all_valid` test makes that a build-time failure instead.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::levels::{ApprovalLevel, LevelPlan};
use crate::registry::{CLI_ENGINES, CliEngineSpec, InstallCommand};

/// Every embedded entry, as `(file name, text)`. Add a file to `providers/cli/` and to this
/// list; `every_file_in_providers_cli_is_embedded` fails when one is forgotten.
const EMBEDDED: &[(&str, &str)] = &[
    (
        "auggie.toml",
        include_str!("../../../providers/cli/auggie.toml"),
    ),
    (
        "cline.toml",
        include_str!("../../../providers/cli/cline.toml"),
    ),
    (
        "droid.toml",
        include_str!("../../../providers/cli/droid.toml"),
    ),
    (
        "grok.toml",
        include_str!("../../../providers/cli/grok.toml"),
    ),
    (
        "junie.toml",
        include_str!("../../../providers/cli/junie.toml"),
    ),
    (
        "openhands.toml",
        include_str!("../../../providers/cli/openhands.toml"),
    ),
];

/// Index of a data-driven engine in [`engines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataId(u8);

impl DataId {
    /// The engine this id names.
    #[must_use]
    pub fn engine(self) -> &'static AcpEngine {
        &engines()[usize::from(self.0)]
    }
}

/// How the model is chosen once the ACP session exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelection {
    /// Praxi never sends a model; a model other than `default` is refused, never swapped.
    None,
    /// ACP `session/set_model`.
    SetModel,
    /// ACP `session/set_config_option` with the `model` option.
    ConfigOption,
}

/// What one approval level means for the engine.
#[derive(Debug, Clone, Copy)]
struct LevelEntry {
    applied: ApprovalLevel,
    native: &'static str,
    mode: Option<&'static str>,
    flags: &'static [&'static str],
    allow_all: bool,
    enforced: bool,
}

/// A validated, data-driven ACP engine.
#[derive(Debug)]
pub struct AcpEngine {
    pub spec: &'static CliEngineSpec,
    /// Fixed argv after the executable, before the level's flags.
    pub args: &'static [&'static str],
    pub model: ModelSelection,
    /// Home-relative folders that hold the CLI's credentials and settings. The file tools and
    /// the never-list must refuse them (checked by tests in those crates).
    pub protected_dirs: &'static [&'static str],
    /// Facts this entry does not claim.
    pub unverified: &'static [&'static str],
    levels: [Option<LevelEntry>; 4],
}

impl AcpEngine {
    /// The level table row. A level without its own entry runs as the Ask entry, so it is
    /// never offered and never wider than Ask.
    ///
    /// # Panics
    /// Never for a parsed entry: [`parse`] always stores the Ask level.
    #[must_use]
    pub fn level_plan(&self, requested: ApprovalLevel) -> LevelPlan {
        let ask = self.levels[0].expect("an entry always has an Ask level");
        let own = self.levels[usize::from(requested.rank())];
        let entry = own.unwrap_or(ask);
        LevelPlan {
            requested,
            applied: entry.applied,
            offered: requested.rank() == entry.applied.rank() && entry.enforced,
            native: entry.native,
            claude_mode: None,
            codex: None,
            flags: entry.flags,
            acp_allow_all: entry.allow_all,
            acp_mode: entry.mode,
            enforced: entry.enforced,
        }
    }

    /// Launch argv: the fixed ACP arguments, then the level's flags. No model, no prompt.
    #[must_use]
    pub fn argv(&self, plan: &LevelPlan) -> Vec<String> {
        self.args
            .iter()
            .chain(plan.flags)
            .map(|a| (*a).to_owned())
            .collect()
    }
}

/// All valid embedded engines, in file-name order.
#[must_use]
pub fn engines() -> &'static [AcpEngine] {
    static ENGINES: OnceLock<Vec<AcpEngine>> = OnceLock::new();
    ENGINES.get_or_init(|| {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for (_, text) in EMBEDDED {
            let Ok(engine) = parse(text) else { continue };
            if out.len() < usize::from(u8::MAX)
                && seen.insert(engine.spec.id.to_owned())
                && !collides(&engine, &out)
            {
                out.push(engine);
            }
        }
        out
    })
}

/// The data-driven engine with this registry id.
#[must_use]
pub fn data_id(id: &str) -> Option<DataId> {
    engines()
        .iter()
        .position(|e| e.spec.id == id)
        .and_then(|i| u8::try_from(i).ok())
        .map(DataId)
}

/// The id of every data-driven engine.
pub(crate) fn ids() -> impl Iterator<Item = DataId> {
    (0..engines().len())
        .filter_map(|i| u8::try_from(i).ok())
        .map(DataId)
}

/// Every data-driven descriptor, for the registry lookup.
pub(crate) fn specs() -> impl Iterator<Item = &'static CliEngineSpec> {
    engines().iter().map(|e| e.spec)
}

fn names(spec: &CliEngineSpec) -> impl Iterator<Item = &'static str> {
    std::iter::once(spec.command).chain(spec.aliases.iter().copied())
}

/// Ids, commands and aliases are unique across the built-in table and the other entries.
fn collides(engine: &AcpEngine, accepted: &[AcpEngine]) -> bool {
    let used: HashSet<&str> = CLI_ENGINES
        .iter()
        .map(|s| s.id)
        .chain(CLI_ENGINES.iter().flat_map(names))
        .chain(
            accepted
                .iter()
                .flat_map(|e| std::iter::once(e.spec.id).chain(names(e.spec))),
        )
        .collect();
    std::iter::once(engine.spec.id)
        .chain(names(engine.spec))
        .any(|n| used.contains(n))
}

// ---------------------------------------------------------------------------- schema

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    id: String,
    display_name: String,
    command: String,
    #[serde(default)]
    aliases: Vec<String>,
    docs_url: String,
    /// What the owner types to sign in. Shown for copying, never executed.
    login: String,
    /// Only `experimental` is accepted until a real signed-in run is verified.
    support: String,
    /// Bundled fallback list; `default` first.
    models: Vec<String>,
    #[serde(default)]
    protected_dirs: Vec<String>,
    /// What was read and from where, in one short paragraph.
    evidence: String,
    /// Facts the entry does not claim (sign-in status, version floor, ...). Never empty.
    unverified: Vec<String>,
    #[serde(default)]
    install: Install,
    acp: Acp,
    levels: Levels,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Install {
    /// npm package (a registry name, never a path or a version spec); the argv is always
    /// `npm install -g <package>@<version>`.
    npm: Option<String>,
    /// The exact version to install (`1.2.3`): required with `npm`, never a range or a tag.
    version: Option<String>,
    /// Whether that installer is documented for native Windows (default: yes).
    windows: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acp {
    args: Vec<String>,
    model: ModelSelection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Levels {
    ask: Level,
    accept_edits: Option<Level>,
    auto: Option<Level>,
    full: Option<Level>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Level {
    applied: ApprovalLevel,
    native: String,
    mode: Option<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    allow_all: bool,
    #[serde(default = "yes")]
    enforced: bool,
}

const fn yes() -> bool {
    true
}

// ------------------------------------------------------------------------ validation

/// Words that make a flag a grant of more trust. Used only to say *why* a token below Full
/// was refused; what is accepted there is [`SAFE_SINGLES`] and [`SAFE_PAIRS`], not "whatever
/// does not contain one of these words".
const PERMISSIVE: &[&str] = &[
    "yolo",
    "skip-permission",
    "dangerous",
    "allow-all",
    "allow_all",
    "auto-approve",
    "autoapprove",
    "bypass",
    "force",
    "trust-all",
    "no-confirm",
];

/// Tokens a level below Full (and the ACP start argv, which every level shares) may carry on
/// their own. They start the ACP server or turn a CLI's own convenience off.
const SAFE_SINGLES: &[&str] = &["acp", "--acp", "--no-auto-update", "--disallow-temp-dir"];

/// Flag and value pairs a level below Full may carry. A value is pinned: `--auto-approve
/// false` is the safe setting of a flag that is permissive with any other value.
const SAFE_PAIRS: &[(&str, &str)] = &[
    ("--auto-approve", "false"),
    ("--allow-tool", "write"),
    ("--agent-engine", "v3"),
];

/// The whole argv that starts an ACP server, for CLIs whose vendor docs give more than one
/// token. Matched as a whole, not token by token: a subcommand such as `exec` or `agent` is
/// not safe on its own, only as the documented way to start the ACP server.
const ACP_STARTS: &[&[&str]] = &[
    &["agent", "stdio"],
    &["exec", "--output-format", "acp"],
    &["--acp", "true"],
];

fn is_permissive(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    PERMISSIVE.iter().any(|word| lower.contains(word))
}

/// `--flag`, `-f`, `value`: letters, digits, `-`, `_` and `.` only. No path, URL, `key=value`,
/// list, address, whitespace, quoting, shell or interpolation character, so a token can never
/// point the CLI at a file, a host or a config of someone else's choosing.
fn is_fixed_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_slug(text: &str, max: usize) -> bool {
    text.len() >= 2
        && text.len() <= max
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_command_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 48
        && !text.starts_with('-')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_plain_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max
        && text.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

fn is_model_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 80
        && !text.starts_with('-')
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '+')
        })
}

/// A registry package name by npm's own rules, stricter where npm is loose: an optional
/// `@scope/`, then lower-case letters, digits, `-`, `.`, `_`, starting with a letter or digit
/// (so never `.x`, `_x` or `-x`), at most 214 characters. No second `@` (a version or tag),
/// no further `/` (a path or a `user/repo` shorthand) and no `..`.
fn is_npm_package(text: &str) -> bool {
    fn part(text: &str) -> bool {
        text.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && text.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_')
            })
            && !text.contains("..")
    }
    if text.is_empty() || text.len() > 214 {
        return false;
    }
    match text.strip_prefix('@') {
        Some(scoped) => scoped
            .split_once('/')
            .is_some_and(|(scope, name)| part(scope) && part(name)),
        None => part(text),
    }
}

/// An exact release: `major.minor.patch`, plain numbers. Never a range, a tag or `latest`.
fn is_exact_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 9
                && p.chars().all(|c| c.is_ascii_digit())
                && (*p == "0" || !p.starts_with('0'))
        })
}

fn is_https_url(text: &str) -> bool {
    text.len() <= 200
        && text.starts_with("https://")
        && text.chars().all(|c| c.is_ascii_graphic())
        && url::Url::parse(text).is_ok_and(|u| u.host_str().is_some())
}

/// A home-relative folder: one to three plain segments, never absolute, never `..`.
fn is_home_relative(text: &str) -> bool {
    let parts: Vec<&str> = text.split('/').collect();
    (1..=3).contains(&parts.len())
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
}

/// Below Full (`full == false`) only [`SAFE_SINGLES`] and [`SAFE_PAIRS`] pass. At Full a token
/// only has to be a fixed one, because that level grants the trust anyway.
fn check_tokens(tokens: &[String], full: bool, what: &str) -> Result<(), String> {
    if tokens.len() > 8 {
        return Err(format!("{what}: more than 8 arguments"));
    }
    let mut i = 0;
    while let Some(token) = tokens.get(i) {
        if !is_fixed_token(token) {
            return Err(format!("{what}: {token:?} is not a fixed argument"));
        }
        if full || SAFE_SINGLES.contains(&token.as_str()) {
            i += 1;
        } else if tokens
            .get(i + 1)
            .is_some_and(|next| SAFE_PAIRS.contains(&(token.as_str(), next.as_str())))
        {
            i += 2;
        } else if is_permissive(token) {
            return Err(format!(
                "{what}: {token:?} grants trust and is only allowed at Full"
            ));
        } else {
            return Err(format!(
                "{what}: {token:?} is not in the allow-list of arguments below Full"
            ));
        }
    }
    Ok(())
}

fn leak_str(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn leak_strs(items: Vec<String>) -> &'static [&'static str] {
    let items: Vec<&'static str> = items.into_iter().map(leak_str).collect();
    Box::leak(items.into_boxed_slice())
}

fn level_entry(key: ApprovalLevel, level: Level) -> Result<LevelEntry, String> {
    let name = key.as_str();
    if level.applied.rank() > key.rank() {
        return Err(format!(
            "levels.{name}: applied {} is wider than the level",
            level.applied.as_str()
        ));
    }
    if key == ApprovalLevel::Ask && level.applied != ApprovalLevel::Ask {
        return Err("levels.ask must apply Ask".into());
    }
    if !is_plain_text(&level.native, 120) {
        return Err(format!(
            "levels.{name}: native text is empty, long or not plain"
        ));
    }
    if level.allow_all && !(key == ApprovalLevel::Full && level.applied == ApprovalLevel::Full) {
        return Err(format!(
            "levels.{name}: allow_all is only for Full applied at Full"
        ));
    }
    if !level.enforced && level.applied == ApprovalLevel::Full {
        return Err(format!(
            "levels.{name}: Full is never marked not enforceable"
        ));
    }
    if let Some(mode) = &level.mode {
        if !is_fixed_token(mode) {
            return Err(format!("levels.{name}: mode {mode:?} is not a plain id"));
        }
        if key != ApprovalLevel::Full && is_permissive(mode) {
            return Err(format!(
                "levels.{name}: mode {mode:?} grants trust and is only allowed at Full"
            ));
        }
    }
    check_tokens(
        &level.flags,
        key == ApprovalLevel::Full,
        &format!("levels.{name}.flags"),
    )?;
    Ok(LevelEntry {
        applied: level.applied,
        native: leak_str(level.native),
        mode: level.mode.map(leak_str),
        flags: leak_strs(level.flags),
        allow_all: level.allow_all,
        enforced: level.enforced,
    })
}

/// Parse and validate one entry.
///
/// # Errors
/// A description of the first rule the entry breaks.
#[allow(clippy::too_many_lines)] // One rule per line.
pub fn parse(text: &str) -> Result<AcpEngine, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    if !is_slug(&file.id, 24) {
        return Err(format!("id {:?} is not a short lower-case slug", file.id));
    }
    if !is_plain_text(&file.display_name, 40) {
        return Err("display_name is empty, long or not plain".into());
    }
    for name in std::iter::once(&file.command).chain(&file.aliases) {
        if !is_command_name(name) {
            return Err(format!("command {name:?} is not a bare executable name"));
        }
    }
    if file.aliases.len() > 3 {
        return Err("more than 3 aliases".into());
    }
    if !is_https_url(&file.docs_url) {
        return Err("docs_url must be an https URL".into());
    }
    if !is_plain_text(&file.login, 80)
        || file.login.split(' ').next() != Some(file.command.as_str())
    {
        return Err("login must be plain text that starts with the command".into());
    }
    if file.support != "experimental" {
        return Err("support must be \"experimental\" until a real run is verified".into());
    }
    if file.models.first().map(String::as_str) != Some("default")
        || file.models.len() > 8
        || !file.models.iter().all(|m| is_model_id(m))
    {
        return Err("models must start with \"default\" and hold plain model ids".into());
    }
    if !is_plain_text(&file.evidence, 700) {
        return Err("evidence is empty, long or not plain".into());
    }
    if file.unverified.is_empty()
        || file.unverified.len() > 12
        || !file.unverified.iter().all(|u| is_plain_text(u, 120))
    {
        return Err("unverified must list what the entry does not claim".into());
    }
    if file.protected_dirs.len() > 6 || !file.protected_dirs.iter().all(|d| is_home_relative(d)) {
        return Err("protected_dirs must be home-relative folders".into());
    }
    if let Some(package) = &file.install.npm {
        if !is_npm_package(package) {
            return Err(format!(
                "install.npm {package:?} is not a plain package name"
            ));
        }
        if !file
            .install
            .version
            .as_deref()
            .is_some_and(is_exact_version)
        {
            return Err("install.version must be an exact x.y.z release".into());
        }
    } else if file.install.windows.is_some() || file.install.version.is_some() {
        return Err("install.windows and install.version need install.npm".into());
    }
    if file.acp.args.is_empty() {
        return Err("acp.args must start the ACP server".into());
    }
    let documented_start = ACP_STARTS.iter().any(|start| {
        start
            .iter()
            .copied()
            .eq(file.acp.args.iter().map(String::as_str))
    });
    if !documented_start {
        check_tokens(&file.acp.args, false, "acp.args")?;
    }

    let Levels {
        ask,
        accept_edits,
        auto,
        full,
    } = file.levels;
    let levels = [
        Some(level_entry(ApprovalLevel::Ask, ask)?),
        accept_edits
            .map(|l| level_entry(ApprovalLevel::AcceptEdits, l))
            .transpose()?,
        auto.map(|l| level_entry(ApprovalLevel::Auto, l))
            .transpose()?,
        full.map(|l| level_entry(ApprovalLevel::Full, l))
            .transpose()?,
    ];

    let install = file
        .install
        .npm
        .zip(file.install.version)
        .map(|(package, version)| {
            let args: &'static [&'static str] = Box::leak(Box::new([
                "install",
                "-g",
                leak_str(format!("{package}@{version}")),
            ]));
            InstallCommand {
                program: "npm",
                args,
                // Shown to copy, like an experimental built-in's, and never run by Praxi: no
                // install of this exact package and version has been recorded. It becomes
                // runnable only through a reviewed change that records a real install.
                verified: false,
                executable: false,
            }
        });
    let windows = file.install.windows.unwrap_or(true);
    let spec: &'static CliEngineSpec = Box::leak(Box::new(CliEngineSpec {
        id: leak_str(file.id),
        display_name: leak_str(file.display_name),
        command: leak_str(file.command),
        aliases: leak_strs(file.aliases),
        runnable: false,
        version_args: &["--version"],
        auth_args: &[],
        minimum_version: None,
        install_windows: install.filter(|_| windows),
        install_unix: install,
        models: leak_strs(file.models),
        login: leak_str(file.login),
        docs_url: leak_str(file.docs_url),
    }));
    Ok(AcpEngine {
        spec,
        args: leak_strs(file.acp.args),
        model: file.acp.model,
        protected_dirs: leak_strs(file.protected_dirs),
        unverified: leak_strs(file.unverified),
        levels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
id = "demo"
display_name = "Demo"
command = "demo"
docs_url = "https://example.com/docs"
login = "demo login"
support = "experimental"
models = ["default"]
evidence = "Read from the vendor docs."
unverified = ["sign-in status"]
[install]
npm = "demo-cli"
version = "1.2.3"
[acp]
args = ["--acp"]
model = "none"
[levels.ask]
applied = "ask"
native = "default"
[levels.full]
applied = "full"
native = "--yes"
flags = ["--auto-approve", "true"]
allow_all = true
"#;

    fn broken(from: &str, to: &str) -> String {
        assert!(GOOD.contains(from), "{from}");
        GOOD.replacen(from, to, 1)
    }

    #[test]
    fn a_good_entry_parses_and_never_widens() {
        let engine = parse(GOOD).unwrap();
        assert_eq!(engine.spec.id, "demo");
        assert!(!engine.spec.runnable);
        let install = engine.spec.install_unix.unwrap();
        assert_eq!(install.args, ["install", "-g", "demo-cli@1.2.3"]);
        // No install has been recorded: shown to copy, never run by Praxi.
        assert!(!install.verified && !install.executable);
        for level in ApprovalLevel::ALL {
            let plan = engine.level_plan(level);
            assert!(plan.applied.rank() <= level.rank());
            assert_eq!(plan.offered, plan.applied == level);
        }
        // Accept edits and Auto have no entry: they run as Ask and are not offered.
        assert_eq!(
            engine.level_plan(ApprovalLevel::AcceptEdits).applied,
            ApprovalLevel::Ask
        );
        assert!(!engine.level_plan(ApprovalLevel::Auto).offered);
        let full = engine.level_plan(ApprovalLevel::Full);
        assert!(full.offered && full.acp_allow_all);
        assert_eq!(engine.argv(&full), ["--acp", "--auto-approve", "true"]);
        assert_eq!(
            engine.argv(&engine.level_plan(ApprovalLevel::Ask)),
            ["--acp"]
        );
    }

    #[test]
    fn the_schema_is_strict() {
        let cases = [
            // Unknown keys, including a free-form command.
            (
                broken(
                    "command = \"demo\"",
                    "command = \"demo\"\nrun = \"sh -c x\"",
                ),
                "unknown field",
            ),
            // Interpolation, quoting, whitespace and shell syntax in argv.
            (
                broken("args = [\"--acp\"]", "args = [\"--acp\", \"{prompt}\"]"),
                "fixed argument",
            ),
            (
                broken("args = [\"--acp\"]", "args = [\"--acp $HOME\"]"),
                "fixed argument",
            ),
            (
                broken("args = [\"--acp\"]", "args = [\"--acp;rm\"]"),
                "fixed argument",
            ),
            // A command is a bare name, never a path.
            (
                broken("command = \"demo\"", "command = \"/usr/bin/demo\""),
                "bare executable",
            ),
            (
                broken("command = \"demo\"", "command = \"..\\\\demo\""),
                "bare executable",
            ),
            // Only fixed npm packages install.
            (
                broken("npm = \"demo-cli\"", "npm = \"demo-cli && curl x | sh\""),
                "plain package",
            ),
            (
                broken("npm = \"demo-cli\"", "npm = \"--prefix=/\""),
                "plain package",
            ),
            // A package needs an exact version.
            (broken("version = \"1.2.3\"\n", ""), "exact"),
            (
                broken("version = \"1.2.3\"", "version = \"^1.2.3\""),
                "exact",
            ),
            (
                broken("version = \"1.2.3\"", "version = \"latest\""),
                "exact",
            ),
            (broken("version = \"1.2.3\"", "version = \"1.2\""), "exact"),
            (
                broken(
                    "npm = \"demo-cli\"\nversion = \"1.2.3\"",
                    "version = \"1.2.3\"",
                ),
                "need install.npm",
            ),
            // Experimental until a real run is verified.
            (
                broken("support = \"experimental\"", "support = \"verified\""),
                "experimental",
            ),
            // Unknown facts are stated.
            (
                broken("unverified = [\"sign-in status\"]", "unverified = []"),
                "unverified",
            ),
            // https docs only.
            (
                broken("https://example.com/docs", "http://example.com/docs"),
                "https",
            ),
            (
                broken("https://example.com/docs", "javascript:alert(1)"),
                "https",
            ),
            (
                broken("login = \"demo login\"", "login = \"other login\""),
                "starts with the command",
            ),
            (
                broken("models = [\"default\"]", "models = [\"gpt\"]"),
                "default",
            ),
            (
                broken(
                    "evidence = \"Read from the vendor docs.\"",
                    "evidence = \"\"",
                ),
                "evidence",
            ),
        ];
        for (text, needle) in cases {
            let error = parse(&text)
                .err()
                .unwrap_or_else(|| panic!("accepted: {text}"));
            assert!(error.contains(needle), "{needle}: {error}");
        }
    }

    #[test]
    fn a_multi_token_start_must_be_a_documented_one_as_a_whole() {
        for args in [
            r#"["agent", "stdio"]"#,
            r#"["exec", "--output-format", "acp"]"#,
            r#"["--acp", "true"]"#,
        ] {
            parse(&broken("args = [\"--acp\"]", &format!("args = {args}"))).unwrap();
        }
        // A subcommand is not safe by itself, and a documented start takes nothing extra.
        for args in [
            r#"["exec"]"#,
            r#"["agent"]"#,
            r#"["stdio"]"#,
            r#"["exec", "--output-format", "json"]"#,
            r#"["exec", "--output-format", "acp", "--skip-permissions-unsafe"]"#,
            r#"["agent", "stdio", "--yolo"]"#,
            r#"["agent", "--always-approve", "stdio"]"#,
            r#"["--acp", "true", "--yolo"]"#,
            r#"["--acp=true"]"#,
        ] {
            assert!(
                parse(&broken("args = [\"--acp\"]", &format!("args = {args}"))).is_err(),
                "{args}"
            );
        }
    }

    #[test]
    fn a_level_is_never_wider_than_its_name() {
        // Accept edits applying Full, or Ask applying anything else.
        let wider = format!("{GOOD}\n[levels.accept_edits]\napplied = \"full\"\nnative = \"x\"\n");
        assert!(parse(&wider).is_err());
        let auto_as_full = GOOD.replace(
            "[levels.full]",
            "[levels.auto]\napplied = \"full\"\nnative = \"x\"\n[levels.full]",
        );
        assert!(parse(&auto_as_full).unwrap_err().contains("wider"));
        let ask_wide = broken(
            "[levels.ask]\napplied = \"ask\"",
            "[levels.ask]\napplied = \"auto\"",
        );
        assert!(parse(&ask_wide).is_err());
        // Approve-all only for Full applied at Full.
        let ask_all = broken(
            "native = \"default\"",
            "native = \"default\"\nallow_all = true",
        );
        assert!(parse(&ask_all).unwrap_err().contains("allow_all"));
        // Full is never "not enforceable"; below Full it may be.
        let weak_full = broken("allow_all = true", "allow_all = true\nenforced = false");
        assert!(parse(&weak_full).unwrap_err().contains("never marked"));
        let weak_ask = broken(
            "native = \"default\"",
            "native = \"not enforceable\"\nenforced = false",
        );
        let engine = parse(&weak_ask).unwrap();
        let plan = engine.level_plan(ApprovalLevel::Ask);
        assert!(!plan.enforced && !plan.offered);
        // The fallback levels inherit that: nothing below Full is enforced.
        assert!(!engine.level_plan(ApprovalLevel::Auto).enforced);
        assert!(engine.level_plan(ApprovalLevel::Full).enforced);
    }

    #[test]
    fn permissive_flags_are_only_accepted_at_full() {
        for flag in [
            "--yolo",
            "--dangerously-skip-permissions",
            "--allow-all-tools",
            "--force",
            "--auto-approve",
        ] {
            let in_ask = broken(
                "native = \"default\"",
                &format!("native = \"default\"\nflags = [\"{flag}\"]"),
            );
            assert!(
                parse(&in_ask).unwrap_err().contains("only allowed at Full"),
                "{flag}"
            );
            let in_args = broken(
                "args = [\"--acp\"]",
                &format!("args = [\"--acp\", \"{flag}\"]"),
            );
            assert!(parse(&in_args).is_err(), "{flag}");
        }
        // Pinning the safe value below Full is allowed.
        let pinned = broken(
            "native = \"default\"",
            "native = \"default\"\nflags = [\"--auto-approve\", \"false\"]",
        );
        assert!(parse(&pinned).is_ok());
    }

    #[test]
    fn npm_package_names_follow_npms_rules() {
        for good in [
            "cline",
            "demo-cli",
            "a",
            "@augmentcode/auggie",
            "@scope/pkg.name_1",
            "x2",
            &"a".repeat(214),
        ] {
            assert!(is_npm_package(good), "{good}");
        }
        let too_long = "a".repeat(215);
        for bad in [
            "",
            "/bin/sh",
            "/opt/x",
            "./x",
            "../x",
            "a/b",
            "user/repo",
            "a@1.2.3",
            "a@latest",
            "@scope/pkg@1.2.3",
            "@@scope/pkg",
            "@scope",
            "@scope/",
            "@/pkg",
            "@scope/a/b",
            ".hidden",
            "_private",
            "-flag",
            "--prefix",
            "@.scope/pkg",
            "@scope/.pkg",
            "@scope/_pkg",
            "Upper",
            "a b",
            "a;b",
            "a..b",
            "git+https://example.com/x.git",
            "https://example.com/x.tgz",
            "file:../x",
            "C:\\x",
            &too_long,
        ] {
            assert!(!is_npm_package(bad), "{bad}");
        }
        // And through the loader, whatever the version.
        for bad in ["/bin/sh", "a@1.2.3", "@s/n@1", "_x", "../x", "a/b"] {
            let text = broken("npm = \"demo-cli\"", &format!("npm = \"{bad}\""));
            assert!(parse(&text).unwrap_err().contains("plain package"), "{bad}");
        }
    }

    #[test]
    fn a_version_is_an_exact_release() {
        for good in ["0.0.1", "1.2.3", "10.20.30"] {
            assert!(is_exact_version(good), "{good}");
        }
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.x",
            "^1.2.3",
            "~1.2.3",
            ">=1.2.3",
            "1.2.3-rc.1",
            "latest",
            "1.2.3 ",
            "1.2.3@x",
            "*",
        ] {
            assert!(!is_exact_version(bad), "{bad}");
        }
    }

    #[test]
    fn an_install_command_is_unverified_and_never_runs() {
        for engine in engines() {
            for install in [engine.spec.install_windows, engine.spec.install_unix]
                .into_iter()
                .flatten()
            {
                assert!(
                    !install.verified && !install.executable,
                    "{}: no real install is recorded",
                    engine.spec.id
                );
                // The pin is in the argv: `npm install -g <name>@<x.y.z>`.
                assert_eq!(&install.args[..2], ["install", "-g"]);
                let (name, version) = install.args[2].rsplit_once('@').unwrap();
                assert!(is_npm_package(name) && is_exact_version(version));
            }
            // The one-click install path takes only a verified, executable command.
            assert!(
                crate::engine_spec(engine.spec.id)
                    .and_then(CliEngineSpec::install)
                    .is_none_or(|i| !(i.verified && i.executable))
            );
        }
    }

    #[test]
    fn nothing_outside_the_allow_list_passes_below_full() {
        let refused = [
            // A new grant under a name nobody listed.
            "--yes",
            "--auto",
            "--unrestricted",
            "--no-sandbox",
            "--approval-mode",
            "--trust",
            "--config=/attacker",
            "--config",
            "--config=x",
            "--url=https://evil.example",
            "--registry=https://evil.example",
            "--host=evil",
            "--tools=a,b",
            "/etc/passwd",
            "C:/x",
            "user@host",
            "a:b",
            "a,b",
            "key=value",
            // The permissive flag with any value but the pinned safe one.
            "--auto-approve",
        ];
        for token in refused {
            let in_flags = broken(
                "native = \"default\"",
                &format!("native = \"default\"\nflags = [\"{token}\"]"),
            );
            assert!(parse(&in_flags).is_err(), "flags {token}");
            let in_args = broken(
                "args = [\"--acp\"]",
                &format!("args = [\"--acp\", \"{token}\"]"),
            );
            assert!(parse(&in_args).is_err(), "args {token}");
        }
        // A pair is checked as a pair: the pinned value, not any value.
        for flags in [
            r#"["--auto-approve", "true"]"#,
            r#"["--auto-approve", "false", "--auto-approve", "true"]"#,
            r#"["--allow-tool", "bash"]"#,
            r#"["--allow-tool"]"#,
            r#"["write"]"#,
            r#"["false"]"#,
        ] {
            let text = broken(
                "native = \"default\"",
                &format!("native = \"default\"\nflags = {flags}"),
            );
            assert!(parse(&text).is_err(), "{flags}");
        }
        // Every level below Full is held to it, not only Ask.
        for level in ["accept_edits", "auto"] {
            let text = format!(
                "{GOOD}\n[levels.{level}]\napplied = \"ask\"\nnative = \"x\"\nflags = [\"--yes\"]\n"
            );
            assert!(parse(&text).is_err(), "{level}");
        }
        // The known-safe tokens pass.
        let safe = broken(
            "native = \"default\"",
            "native = \"default\"\nflags = [\"--no-auto-update\", \"--disallow-temp-dir\", \"--auto-approve\", \"false\", \"--allow-tool\", \"write\"]",
        );
        assert!(parse(&safe).is_ok());
        // A mode below Full cannot be a grant either; at Full it can.
        let yolo_ask = broken(
            "native = \"default\"",
            "native = \"default\"\nmode = \"yolo\"",
        );
        assert!(
            parse(&yolo_ask)
                .unwrap_err()
                .contains("only allowed at Full")
        );
        let yolo_full = broken("allow_all = true", "allow_all = true\nmode = \"yolo\"");
        assert!(parse(&yolo_full).is_ok());
        // At Full a token is still a fixed one: no path, URL, `=` or list.
        for token in ["--config=/attacker", "/bin/sh", "a:b", "a@b", "a,b"] {
            let text = broken("\"--auto-approve\", \"true\"", &format!("\"{token}\""));
            assert!(
                parse(&text).unwrap_err().contains("fixed argument"),
                "{token}"
            );
        }
    }

    /// A data entry for `id` with the given ACP argv, model selection and level tables.
    fn entry(id: &str, command: &str, args: &str, levels: &str) -> String {
        format!(
            r#"
id = "{id}"
display_name = "Parity {id}"
command = "{command}"
docs_url = "https://example.com/{id}"
login = "{command} login"
support = "experimental"
models = ["default"]
evidence = "Test copy of an existing Rust driver."
unverified = ["everything"]
[acp]
args = {args}
model = "none"
{levels}"#
        )
    }

    /// Five built-in ACP drivers written as data produce exactly the same level plans and
    /// launch argv as their Rust code, so migrating one is a change of where the table lives,
    /// not of what it says. (Antigravity, Qoder, Goose, Copilot and Kiro; their Rust arms stay
    /// in this slice because each also has engine-specific detection, sign-in and model code.)
    #[test]
    #[allow(clippy::too_many_lines)] // Five tables, one test.
    fn the_schema_expresses_existing_drivers_without_a_behaviour_change() {
        use crate::levels::{Driver, acp_args, level_plan};
        let cases: [(Driver, &str, &str, &str); 5] = [
            (
                Driver::Agy,
                "agy",
                r#"["acp"]"#,
                r#"
[levels.ask]
applied = "ask"
mode = "default"
native = "mode default"
[levels.accept_edits]
applied = "accept_edits"
mode = "accept-edits"
native = "mode accept-edits"
[levels.auto]
applied = "auto"
mode = "auto"
native = "mode auto"
[levels.full]
applied = "full"
native = "--dangerously-skip-permissions"
flags = ["--dangerously-skip-permissions"]
allow_all = true
"#,
            ),
            (
                Driver::Qoder,
                "qodercli",
                r#"["--acp"]"#,
                r#"
[levels.ask]
applied = "ask"
mode = "default"
native = "mode default"
[levels.accept_edits]
applied = "accept_edits"
mode = "acceptEdits"
native = "mode acceptEdits"
[levels.auto]
applied = "auto"
mode = "auto"
native = "mode auto"
[levels.full]
applied = "full"
mode = "yolo"
native = "mode yolo"
allow_all = true
"#,
            ),
            (
                Driver::Goose,
                "goose",
                r#"["acp"]"#,
                r#"
[levels.ask]
applied = "ask"
mode = "approve"
native = "mode approve"
[levels.auto]
applied = "auto"
mode = "smart_approve"
native = "mode smart_approve"
[levels.full]
applied = "full"
mode = "auto"
native = "mode auto"
allow_all = true
"#,
            ),
            (
                Driver::Copilot,
                "copilot",
                r#"["--acp"]"#,
                r#"
[levels.ask]
applied = "ask"
native = "default (asks before edits and commands)"
flags = ["--no-auto-update", "--disallow-temp-dir"]
[levels.accept_edits]
applied = "accept_edits"
native = "--allow-tool write"
flags = ["--no-auto-update", "--disallow-temp-dir", "--allow-tool", "write"]
[levels.full]
applied = "full"
native = "permission requests approved automatically"
flags = ["--no-auto-update", "--disallow-temp-dir"]
allow_all = true
"#,
            ),
            (
                Driver::Kiro,
                "kiro-cli",
                r#"["acp", "--agent-engine", "v3"]"#,
                r#"
[levels.ask]
applied = "ask"
native = "default permissions"
[levels.full]
applied = "full"
native = "every permission request approved"
allow_all = true
"#,
            ),
        ];
        for (driver, command, args, levels) in cases {
            let engine = parse(&entry("parity", command, args, levels))
                .unwrap_or_else(|e| panic!("{driver:?}: {e}"));
            for level in ApprovalLevel::ALL {
                let (rust, data) = (level_plan(driver, level), engine.level_plan(level));
                assert_eq!(rust, data, "{driver:?} {level:?}");
                assert_eq!(
                    acp_args(driver, &rust, None).unwrap(),
                    engine.argv(&data),
                    "{driver:?} {level:?}"
                );
            }
        }
    }

    #[test]
    fn embedded_entries_are_all_valid() {
        for (name, text) in EMBEDDED {
            parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        assert_eq!(engines().len(), EMBEDDED.len(), "an entry was dropped");
    }

    #[test]
    fn protected_dirs_stay_on_one_line_for_the_cross_crate_checks() {
        // The file tools and the never-list read these lines from the files in their own
        // tests, without a TOML parser.
        for (name, text) in EMBEDDED {
            let engine = parse(text).unwrap();
            let quoted: usize = text
                .lines()
                .filter(|l| l.trim_start().starts_with("protected_dirs"))
                .map(|l| l.split('"').skip(1).step_by(2).count())
                .sum();
            assert_eq!(quoted, engine.protected_dirs.len(), "{name}");
            assert!(
                !engine.protected_dirs.is_empty(),
                "{name}: no credential folder named"
            );
        }
    }

    #[test]
    fn every_file_in_providers_cli_is_embedded() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/cli");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| {
                std::path::Path::new(n)
                    .extension()
                    .is_some_and(|e| e == "toml")
            })
            .collect();
        on_disk.sort();
        let embedded: Vec<&str> = EMBEDDED.iter().map(|(n, _)| *n).collect();
        assert_eq!(on_disk, embedded);
    }

    #[test]
    fn entries_do_not_collide_with_built_in_engines_or_each_other() {
        for engine in engines() {
            assert!(
                !CLI_ENGINES.iter().any(|s| s.id == engine.spec.id
                    || names(s).any(|n| names(engine.spec).any(|m| m == n))),
                "{}",
                engine.spec.id
            );
        }
        let mut ids = HashSet::new();
        assert!(engines().iter().all(|e| ids.insert(e.spec.id)));
        let again = parse(GOOD).unwrap();
        assert!(!collides(&again, &[]));
        assert!(collides(
            &again,
            std::slice::from_ref(&parse(GOOD).unwrap())
        ));
        // A data entry cannot take a built-in id or command.
        assert!(collides(
            &parse(&broken("id = \"demo\"", "id = \"claude\"")).unwrap(),
            &[]
        ));
        let other = broken("command = \"demo\"", "command = \"codex\"")
            .replace("login = \"demo login\"", "login = \"codex login\"");
        assert!(collides(&parse(&other).unwrap(), &[]));
    }
}
