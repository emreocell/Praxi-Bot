//! MCP Bundles (`.mcpb`): install a local MCP server from one file, without
//! editing JSON.
//!
//! A bundle is a zip archive with a `manifest.json` at its root. The manifest
//! format (spec v0.3) is the public contract of
//! <https://github.com/modelcontextprotocol/mcpb> (Apache-2.0 for new code,
//! MIT for older; see `THIRD_PARTY_NOTICES.md`). This module is an independent
//! Rust implementation of that format; no upstream source code was copied.
//!
//! The flow has two steps so the owner always decides on what will run:
//!
//! 1. [`inspect`] reads the archive in memory, writes nothing and returns an
//!    [`Inspection`]: the approval card (command, environment variable names,
//!    runtime, secret fields, warnings) plus the bundle's SHA-256.
//! 2. [`install`] runs only with the SHA-256 the owner approved. It unpacks
//!    into `<data dir>/mcp/<id>/` (zip-slip, symlink and size checks), stores
//!    `sensitive` values in the secret store and returns an [`McpServerConfig`]
//!    whose `secret_env` points at them.
//!
//! Supported on purpose: `node`, `python`, `uv` and `binary` servers,
//! `${__dirname}`, `${/}` and `${user_config.KEY}`. Anything else (for example
//! `${HOME}`, `multiple` fields) is refused with a plain message instead of
//! being guessed at.

use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

use secret_store::{SecretRef, SecretStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    config::{McpServerConfig, McpTransport},
    error::{McpError, Result},
};

/// Largest bundle accepted (compressed).
pub const MAX_BUNDLE_BYTES: usize = 100 * 1024 * 1024;
/// Largest unpacked size.
pub const MAX_UNPACKED_BYTES: u64 = 400 * 1024 * 1024;
/// Most files in one bundle.
pub const MAX_FILES: usize = 20_000;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// A bundle that unpacks to more than this AND to more than
/// [`MAX_EXPANSION_RATIO`] times its compressed size is treated as a zip bomb.
const BOMB_MIN_UNPACKED: u64 = 50 * 1024 * 1024;
const MAX_EXPANSION_RATIO: u64 = 100;
/// File written next to the unpacked bundle: which bytes were approved.
pub const PIN_FILE: &str = ".praxi-bundle.json";

fn err(msg: impl Into<String>) -> McpError {
    McpError::Config(msg.into())
}

/// What the server needs to run. Shown on the approval card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    /// Needs Node.js (Praxi can install it for the user).
    Node,
    /// Needs Python.
    Python,
    /// Needs `uv`.
    Uv,
    /// A prebuilt program shipped inside the bundle.
    Binary,
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    #[serde(default, alias = "dxt_version")]
    manifest_version: String,
    name: String,
    version: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    long_description: Option<String>,
    #[serde(default)]
    author: Option<Author>,
    server: ServerSpec,
    #[serde(default)]
    user_config: BTreeMap<String, UserField>,
    #[serde(default)]
    compatibility: Option<Compat>,
    #[serde(default)]
    tools: Vec<ToolNote>,
}

#[derive(Debug, Clone, Deserialize)]
struct Author {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolNote {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Compat {
    #[serde(default)]
    platforms: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ServerSpec {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    entry_point: Option<String>,
    #[serde(default)]
    mcp_config: Option<McpLaunch>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct McpLaunch {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    platform_overrides: BTreeMap<String, McpLaunch>,
}

/// One setting the user fills in before the server starts.
#[derive(Debug, Clone, Deserialize)]
struct UserField {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    sensitive: bool,
    #[serde(default)]
    multiple: bool,
    #[serde(default)]
    default: Option<serde_json::Value>,
}

/// A setting the owner is asked for (the form of the install dialog).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigField {
    /// Key used in `${user_config.KEY}`.
    pub key: String,
    /// `string`, `number`, `boolean`, `directory` or `file`.
    pub kind: String,
    /// Label to show.
    pub title: String,
    /// Help text, may be empty.
    pub description: String,
    /// Must be filled in (no default).
    pub required: bool,
    /// Stored in the secret store, never in the config file.
    pub sensitive: bool,
    /// Default value as text.
    pub default: Option<String>,
}

/// The approval card: everything the owner should see before anything runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inspection {
    /// Server id (becomes `mcp:<id>/<tool>`), derived from the bundle name.
    pub id: String,
    /// Human name.
    pub display_name: String,
    /// Bundle version.
    pub version: String,
    /// Short description from the manifest (untrusted text).
    pub description: String,
    /// Author name, if given (untrusted text).
    pub author: Option<String>,
    /// What it needs to run.
    pub runtime: Runtime,
    /// The program that will be started.
    pub command: String,
    /// Its arguments; `<bundle folder>` stands for the unpack directory.
    pub args: Vec<String>,
    /// Names of the environment variables the server will receive (no values).
    pub env_names: Vec<String>,
    /// Settings the owner must fill in.
    pub fields: Vec<ConfigField>,
    /// SHA-256 (hex) of the bundle file. The approval is for exactly these bytes.
    pub sha256: String,
    /// Number of files that will be unpacked.
    pub file_count: usize,
    /// Unpacked size in bytes.
    pub unpacked_bytes: u64,
    /// Things the owner should read before approving (suspicious text, ...).
    pub warnings: Vec<String>,
}

/// SHA-256 of the bytes as lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

fn current_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// Server id from a manifest name: `[A-Za-z0-9._-]`, at most 60 bytes.
fn id_from_name(name: &str) -> Result<String> {
    let id: String = name
        .trim()
        .trim_start_matches('@')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    let id = id.trim_matches(|c| c == '.' || c == '-').to_string();
    if id.is_empty() || id.contains("..") {
        return Err(err("the bundle has no usable name"));
    }
    Ok(id)
}

/// A relative path from an archive entry: no `..`, no root, no drive, no
/// backslash tricks. `None` = refuse.
fn safe_relative(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') || name.contains(':') {
        return None;
    }
    let path = Path::new(name);
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

struct Archive {
    manifest: Manifest,
    file_count: usize,
    unpacked: u64,
}

fn open_archive(bytes: &[u8]) -> Result<(zip::ZipArchive<Cursor<&[u8]>>, Archive)> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(err("the bundle is larger than 100 MB"));
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| err("this file is not a valid .mcpb bundle (not a zip archive)"))?;
    if zip.len() > MAX_FILES {
        return Err(err("the bundle contains too many files"));
    }
    let mut unpacked = 0u64;
    let mut compressed = 0u64;
    let mut files = 0usize;
    for i in 0..zip.len() {
        let entry = zip
            .by_index_raw(i)
            .map_err(|_| err("the bundle is damaged"))?;
        let entry_name = entry
            .name()
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default();
        if safe_relative(&entry_name).is_none() {
            return Err(err(format!(
                "the bundle contains an unsafe path (`{}`) and was refused",
                entry_name.chars().take(80).collect::<String>()
            )));
        }
        if let Some(mode) = entry.unix_mode() {
            // S_IFLNK: links could point outside the folder.
            if mode & 0o170_000 == 0o120_000 {
                return Err(err("the bundle contains a symbolic link and was refused"));
            }
        }
        if !entry.is_dir() {
            files += 1;
            unpacked = unpacked.saturating_add(entry.size());
            compressed = compressed.saturating_add(entry.compressed_size());
        }
    }
    if unpacked > BOMB_MIN_UNPACKED && unpacked / compressed.max(1) > MAX_EXPANSION_RATIO {
        return Err(err(
            "the bundle expands to an absurd size compared to its file and was refused",
        ));
    }
    if unpacked > MAX_UNPACKED_BYTES {
        return Err(err("the bundle is larger than 400 MB once unpacked"));
    }
    let manifest = {
        let mut m = zip
            .by_name("manifest.json")
            .map_err(|_| err("the bundle has no manifest.json"))?;
        if m.size() > MAX_MANIFEST_BYTES {
            return Err(err("manifest.json is too large"));
        }
        let mut text = String::new();
        m.by_ref()
            .take(MAX_MANIFEST_BYTES)
            .read_to_string(&mut text)
            .map_err(|_| err("manifest.json is not valid text"))?;
        serde_json::from_str::<Manifest>(&text)
            .map_err(|e| err(format!("manifest.json cannot be read: {e}")))?
    };
    Ok((
        zip,
        Archive {
            manifest,
            file_count: files,
            unpacked,
        },
    ))
}

const PHRASES: [&str; 6] = [
    "ignore previous",
    "ignore all previous",
    "ignore the above",
    "do not tell the user",
    "don't tell the user",
    "system prompt",
];

/// Suspicious text in what the author wrote: invisible characters and phrases
/// that talk to the model instead of the user. The manifest is untrusted data.
fn scan_text(label: &str, text: &str, out: &mut Vec<String>) {
    let hidden = text.chars().any(|c| {
        matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
    });
    if hidden {
        out.push(format!("{label} contains hidden characters"));
    }
    let low = text.to_lowercase();
    if PHRASES.iter().any(|p| low.contains(p)) {
        out.push(format!(
            "{label} contains text that tries to instruct the AI"
        ));
    }
}

fn pick_launch(m: &Manifest) -> Result<(Runtime, McpLaunch)> {
    let runtime = match m.server.kind.as_str() {
        "node" => Runtime::Node,
        "python" => Runtime::Python,
        "uv" => Runtime::Uv,
        "binary" => Runtime::Binary,
        other => return Err(err(format!("server type `{other}` is not supported"))),
    };
    let mut launch = m.server.mcp_config.clone().unwrap_or_default();
    if let Some(over) = launch.platform_overrides.get(current_platform()).cloned() {
        if over.command.is_some() {
            launch.command = over.command;
        }
        if !over.args.is_empty() {
            launch.args = over.args;
        }
        launch.env.extend(over.env);
    }
    if launch.command.is_none() {
        let entry = m.server.entry_point.clone().ok_or_else(|| {
            err("the bundle says neither how to start the server nor its entry point")
        })?;
        let dir_entry = format!("${{__dirname}}/{entry}");
        let (cmd, args) = match runtime {
            Runtime::Node => ("node", vec![dir_entry]),
            Runtime::Python => (python_command(), vec![dir_entry]),
            Runtime::Uv => ("uv", vec!["run".to_string(), dir_entry]),
            Runtime::Binary => (dir_entry.as_str(), Vec::new()),
        };
        launch.command = Some(cmd.to_string());
        launch.args = args;
    }
    Ok((runtime, launch))
}

fn python_command() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn field_of(key: &str, f: &UserField) -> Result<ConfigField> {
    if !matches!(
        f.kind.as_str(),
        "string" | "number" | "boolean" | "directory" | "file"
    ) {
        return Err(err(format!("setting `{key}` has an unknown type")));
    }
    if f.multiple {
        return Err(err(format!(
            "setting `{key}` accepts several values, which Praxi does not support yet"
        )));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        || key.is_empty()
        || key.len() > 40
    {
        return Err(err(format!("setting name `{key}` is not allowed")));
    }
    let default = f.default.as_ref().map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    Ok(ConfigField {
        key: key.to_string(),
        kind: f.kind.clone(),
        title: f.title.clone().unwrap_or_else(|| key.to_string()),
        description: f.description.clone().unwrap_or_default(),
        required: f.required && default.is_none(),
        sensitive: f.sensitive,
        default,
    })
}

/// Read a bundle and describe what installing it would do. Writes nothing.
///
/// # Errors
/// [`McpError::Config`] with a plain-language reason (not a zip, unsafe path,
/// symbolic link, too big, unsupported manifest, wrong operating system).
pub fn inspect(bytes: &[u8]) -> Result<Inspection> {
    let (_, archive) = open_archive(bytes)?;
    let m = &archive.manifest;
    if m.compatibility.as_ref().is_some_and(|c| {
        !c.platforms.is_empty() && !c.platforms.iter().any(|p| p == current_platform())
    }) {
        return Err(err("this bundle does not support this operating system"));
    }
    if !m.manifest_version.is_empty() && !m.manifest_version.starts_with("0.") {
        return Err(err(format!(
            "manifest version `{}` is newer than Praxi understands",
            m.manifest_version
        )));
    }
    let id = id_from_name(&m.name)?;
    let (runtime, launch) = pick_launch(m)?;
    let mut fields = Vec::new();
    for (key, f) in &m.user_config {
        fields.push(field_of(key, f)?);
    }
    let command = launch.command.clone().unwrap_or_default();
    let show = |s: &str| s.replace("${__dirname}", "<bundle folder>");
    let mut warnings = Vec::new();
    scan_text("the description", &m.description, &mut warnings);
    if let Some(long) = &m.long_description {
        scan_text("the long description", long, &mut warnings);
    }
    for t in &m.tools {
        scan_text(
            &format!("the tool `{}`", t.name),
            &t.description,
            &mut warnings,
        );
    }
    if let Some(a) = &m.author {
        scan_text("the author name", &a.name, &mut warnings);
    }
    if runtime == Runtime::Binary {
        warnings.push("this bundle runs a program it ships itself, not a standard runtime".into());
    }
    let inspection = Inspection {
        id,
        display_name: m.display_name.clone().unwrap_or_else(|| m.name.clone()),
        version: m.version.clone(),
        description: m.description.clone(),
        author: m.author.as_ref().map(|a| a.name.clone()),
        runtime,
        command: show(&command),
        args: launch.args.iter().map(|a| show(a)).collect(),
        env_names: launch.env.keys().cloned().collect(),
        fields,
        sha256: sha256_hex(bytes),
        file_count: archive.file_count,
        unpacked_bytes: archive.unpacked,
        warnings,
    };
    // Resolve with placeholder values now so a bad template is reported on the
    // approval card, not after the owner said yes.
    let probe: BTreeMap<String, String> = inspection
        .fields
        .iter()
        .map(|f| (f.key.clone(), "x".to_string()))
        .collect();
    resolve(
        &inspection.id,
        &launch,
        &m.user_config,
        &probe,
        Path::new("/bundle"),
    )?;
    Ok(inspection)
}

struct Resolved {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    secret_env: BTreeMap<String, String>,
    /// (secret name, value)
    secrets: Vec<(String, String)>,
}

fn secret_name(id: &str, key: &str) -> String {
    format!("mcp/{id}/{key}")
}

/// Replace `${...}` variables in one string. Returns the text and the sensitive
/// key when the whole string is exactly one sensitive reference.
fn substitute(
    text: &str,
    dir: &str,
    fields: &BTreeMap<String, UserField>,
    values: &BTreeMap<String, String>,
    allow_sensitive: bool,
) -> Result<(String, Option<String>)> {
    let mut out = String::new();
    let mut rest = text;
    let mut sensitive = None;
    let mut count = 0;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| err(format!("the template `{text}` has an unclosed `${{`")))?;
        let var = &after[..end];
        count += 1;
        if var == "__dirname" {
            out.push_str(dir);
        } else if var == "/" || var == "pathSeparator" {
            out.push(std::path::MAIN_SEPARATOR);
        } else if let Some(key) = var.strip_prefix("user_config.") {
            let field = fields
                .get(key)
                .ok_or_else(|| err(format!("the bundle uses an undeclared setting `{key}`")))?;
            if field.sensitive {
                if !allow_sensitive || text != format!("${{{var}}}") {
                    return Err(err(format!(
                        "the secret setting `{key}` may only be passed alone as an environment variable"
                    )));
                }
                sensitive = Some(key.to_string());
                if count > 1 {
                    return Err(err("a secret must be passed alone"));
                }
            } else {
                out.push_str(values.get(key).map_or("", String::as_str));
            }
        } else {
            return Err(err(format!(
                "the variable `${{{var}}}` is not supported by Praxi"
            )));
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok((out, sensitive))
}

fn resolve(
    id: &str,
    launch: &McpLaunch,
    fields: &BTreeMap<String, UserField>,
    values: &BTreeMap<String, String>,
    dir: &Path,
) -> Result<Resolved> {
    let dir_s = dir.to_string_lossy().to_string();
    let no_escape = |s: &str| -> Result<()> {
        if s.contains(&dir_s) && Path::new(s).components().any(|c| c == Component::ParentDir) {
            return Err(err("a start path leaves the bundle folder"));
        }
        Ok(())
    };
    let (command, _) = substitute(
        launch.command.as_deref().unwrap_or_default(),
        &dir_s,
        fields,
        values,
        false,
    )?;
    no_escape(&command)?;
    let mut args = Vec::new();
    for a in &launch.args {
        let (v, _) = substitute(a, &dir_s, fields, values, false)?;
        no_escape(&v)?;
        args.push(v);
    }
    let mut env = BTreeMap::new();
    let mut secret_env = BTreeMap::new();
    let mut secrets = Vec::new();
    for (name, tmpl) in &launch.env {
        let (v, sensitive) = substitute(tmpl, &dir_s, fields, values, true)?;
        if let Some(key) = sensitive {
            let Some(value) = values.get(&key).filter(|v| !v.is_empty()) else {
                // An optional secret left empty: the variable is simply not set.
                continue;
            };
            let sname = secret_name(id, &key);
            SecretRef::new(sname.as_str())
                .map_err(|_| err(format!("the setting name `{key}` is not allowed")))?;
            secret_env.insert(name.clone(), sname.clone());
            secrets.push((sname, value.clone()));
        } else {
            env.insert(name.clone(), v);
        }
    }
    Ok(Resolved {
        command,
        args,
        env,
        secret_env,
        secrets,
    })
}

fn check_values(
    fields: &BTreeMap<String, UserField>,
    given: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for key in given.keys() {
        if !fields.contains_key(key) {
            return Err(err(format!("`{key}` is not a setting of this bundle")));
        }
    }
    for (key, f) in fields {
        let field = field_of(key, f)?;
        let value = given
            .get(key)
            .filter(|v| !v.is_empty())
            .cloned()
            .or(field.default.clone());
        let Some(value) = value else {
            if f.required {
                return Err(err(format!(
                    "please fill in “{}” before installing",
                    field.title
                )));
            }
            continue;
        };
        match f.kind.as_str() {
            "number" if value.parse::<f64>().is_err() => {
                return Err(err(format!("“{}” must be a number", field.title)));
            }
            "boolean" if !matches!(value.as_str(), "true" | "false") => {
                return Err(err(format!("“{}” must be true or false", field.title)));
            }
            "directory" | "file" if !Path::new(&value).is_absolute() => {
                return Err(err(format!("“{}” must be a full path", field.title)));
            }
            _ => {}
        }
        if value.contains('\0') || value.len() > 4096 {
            return Err(err(format!("“{}” is not a valid value", field.title)));
        }
        out.insert(key.clone(), value);
    }
    Ok(out)
}

/// Install an approved bundle.
///
/// `approved_sha256` is the digest from the [`Inspection`] the owner approved;
/// a different file is refused. `values` are the owner's answers to
/// [`Inspection::fields`]. Secret values go to `secrets` and never into the
/// returned config. Refuses to overwrite an existing install.
///
/// # Errors
/// [`McpError::Config`] for a mismatch, an unsafe archive, a missing required
/// setting or an invalid result; [`McpError::Store`] for disk or secret-store
/// failures. On failure nothing is left behind.
pub fn install(
    bytes: &[u8],
    approved_sha256: &str,
    values: &BTreeMap<String, String>,
    data_dir: &Path,
    secrets: &dyn SecretStore,
) -> Result<McpServerConfig> {
    let digest = sha256_hex(bytes);
    if !digest.eq_ignore_ascii_case(approved_sha256) {
        return Err(err(
            "the file changed after you approved it; please open it again",
        ));
    }
    let inspection = inspect(bytes)?;
    let (mut zip, archive) = open_archive(bytes)?;
    let m = archive.manifest;
    let (_, launch) = pick_launch(&m)?;
    let values = check_values(&m.user_config, values)?;

    let root = data_dir.join("mcp");
    let target = root.join(&inspection.id);
    if target.exists() {
        return Err(err(format!(
            "`{}` is already installed; remove it first",
            inspection.id
        )));
    }
    let store = |e: std::io::Error| McpError::Store(format!("cannot unpack the bundle: {e}"));
    fs::create_dir_all(&root).map_err(store)?;
    let partial = root.join(format!(".{}.partial", inspection.id));
    if partial.exists() {
        fs::remove_dir_all(&partial).map_err(store)?;
    }
    fs::create_dir(&partial).map_err(store)?;
    let outcome = (|| -> Result<McpServerConfig> {
        unpack(&mut zip, &partial)?;
        let resolved = resolve(&inspection.id, &launch, &m.user_config, &values, &target)?;
        let pin = serde_json::json!({
            "sha256": digest, "name": m.name, "version": m.version,
        });
        fs::write(partial.join(PIN_FILE), pin.to_string()).map_err(store)?;
        let mut config = McpServerConfig::stdio(
            inspection.id.clone(),
            resolved.command.clone(),
            resolved.args.clone(),
        );
        config.transport = McpTransport::Stdio {
            command: resolved.command,
            args: resolved.args,
            env: resolved.env,
            cwd: Some(target.clone()),
        };
        config.secret_env = resolved.secret_env;
        config.validate()?;
        let saved = save_secrets(secrets, &resolved.secrets)?;
        if let Err(e) = fs::rename(&partial, &target).map_err(store) {
            saved.undo(secrets);
            return Err(e);
        }
        Ok(config)
    })();
    if outcome.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    outcome
}

/// Secrets written by [`save_secrets`], with what they replaced.
pub(crate) struct SavedSecrets(Vec<(String, Option<String>)>);

impl SavedSecrets {
    /// Put the store back as it was before [`save_secrets`].
    pub(crate) fn undo(self, secrets: &dyn SecretStore) {
        for (name, previous) in self.0.into_iter().rev() {
            let _ = match previous {
                Some(old) => secrets.set(&name, &old),
                None => secrets.delete(&name).map(|_| ()),
            };
        }
    }
}

/// Write all `(name, value)` pairs; when one fails, the ones already written
/// are rolled back so a failed install leaves no stray secret.
pub(crate) fn save_secrets(
    secrets: &dyn SecretStore,
    list: &[(String, String)],
) -> Result<SavedSecrets> {
    let mut saved = SavedSecrets(Vec::new());
    for (name, value) in list {
        let previous = secrets
            .get(name)
            .ok()
            .flatten()
            .map(|v| v.expose().to_string());
        if secrets.set(name, value).is_err() {
            saved.undo(secrets);
            return Err(McpError::Store("cannot save a secret setting".into()));
        }
        saved.0.push((name.clone(), previous));
    }
    Ok(saved)
}

fn unpack(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, dest: &Path) -> Result<()> {
    let io = |e: std::io::Error| McpError::Store(format!("cannot unpack the bundle: {e}"));
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|_| err("the bundle is damaged"))?;
        let entry_name = entry
            .name()
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default();
        let rel = safe_relative(&entry_name).ok_or_else(|| err("unsafe path in the bundle"))?;
        let out = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(io)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .map_err(io)?;
        // Count what is really written: the header size may lie.
        let remaining = MAX_UNPACKED_BYTES.saturating_sub(total);
        let written =
            std::io::copy(&mut entry.by_ref().take(remaining + 1), &mut file).map_err(io)?;
        total += written;
        if total > MAX_UNPACKED_BYTES {
            return Err(err("the bundle is larger than 400 MB once unpacked"));
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o755));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use secret_store::{MemoryStore, SecretStore};

    /// Fails every `set` after the first `allow` ones.
    pub(crate) struct FlakyStore {
        pub inner: MemoryStore,
        pub allow: std::sync::atomic::AtomicUsize,
    }

    impl SecretStore for FlakyStore {
        fn get(
            &self,
            n: &str,
        ) -> std::result::Result<Option<secret_store::SecretValue>, secret_store::SecretError>
        {
            self.inner.get(n)
        }
        fn set(&self, n: &str, v: &str) -> std::result::Result<(), secret_store::SecretError> {
            use std::sync::atomic::Ordering;
            if self
                .allow
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |a| a.checked_sub(1))
                .is_err()
            {
                return Err(secret_store::SecretError::NotFound("boom".into()));
            }
            self.inner.set(n, v)
        }
        fn delete(&self, n: &str) -> std::result::Result<bool, secret_store::SecretError> {
            self.inner.delete(n)
        }
        fn list_names(&self) -> std::result::Result<Vec<String>, secret_store::SecretError> {
            self.inner.list_names()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use secret_store::MemoryStore;

    use super::{test_support::FlakyStore, *};

    fn bundle(manifest: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", opts).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        for (name, data) in extra {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const MANIFEST: &str = r#"{
      "manifest_version": "0.3", "name": "weather", "version": "1.2.0",
      "description": "Weather lookups",
      "server": {"type": "node", "entry_point": "server/index.js",
        "mcp_config": {"command": "node", "args": ["${__dirname}/server/index.js", "${user_config.units}"],
          "env": {"API_KEY": "${user_config.api_key}", "LANG_HINT": "en"}}},
      "user_config": {
        "api_key": {"type": "string", "title": "API key", "required": true, "sensitive": true},
        "units": {"type": "string", "default": "metric"}}
    }"#;

    #[test]
    fn card_shows_command_and_env_names_but_no_values() {
        let b = bundle(MANIFEST, &[("server/index.js", b"//")]);
        let card = inspect(&b).unwrap();
        assert_eq!(card.id, "weather");
        assert_eq!(card.runtime, Runtime::Node);
        assert_eq!(card.command, "node");
        assert_eq!(card.args[0], "<bundle folder>/server/index.js");
        assert_eq!(card.env_names, ["API_KEY", "LANG_HINT"]);
        assert_eq!(card.sha256, sha256_hex(&b));
        assert!(
            card.fields
                .iter()
                .any(|f| f.key == "api_key" && f.sensitive)
        );
    }

    #[test]
    fn install_stores_secret_in_vault_not_in_config() {
        let b = bundle(MANIFEST, &[("server/index.js", b"//")]);
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new();
        let values = BTreeMap::from([("api_key".to_string(), "sk-live-123".to_string())]);
        let cfg = install(&b, &sha256_hex(&b), &values, dir.path(), &store).unwrap();
        assert_eq!(cfg.secret_env["API_KEY"], "mcp/weather/api_key");
        assert_eq!(
            store.get("mcp/weather/api_key").unwrap().unwrap().expose(),
            "sk-live-123"
        );
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("sk-live-123"));
        let McpTransport::Stdio { args, env, .. } = &cfg.transport else {
            panic!()
        };
        assert!(args[0].ends_with("index.js") && args[1] == "metric");
        assert_eq!(env["LANG_HINT"], "en");
        assert!(dir.path().join("mcp/weather/server/index.js").is_file());
        assert!(dir.path().join("mcp/weather").join(PIN_FILE).is_file());
    }

    #[test]
    fn changed_file_after_approval_is_refused() {
        let b = bundle(MANIFEST, &[]);
        let dir = tempfile::tempdir().unwrap();
        let wrong = sha256_hex(b"other");
        let values = BTreeMap::from([("api_key".to_string(), "k".to_string())]);
        let e = install(&b, &wrong, &values, dir.path(), &MemoryStore::new()).unwrap_err();
        assert!(e.to_string().contains("changed after you approved"));
        assert!(!dir.path().join("mcp").exists());
    }

    #[test]
    fn zip_slip_and_symlink_are_refused() {
        let evil = bundle(MANIFEST, &[("../evil.txt", b"x")]);
        assert!(
            inspect(&evil)
                .unwrap_err()
                .to_string()
                .contains("unsafe path")
        );

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", opts).unwrap();
        zip.write_all(MANIFEST.as_bytes()).unwrap();
        zip.add_symlink("link", "/etc/passwd", opts).unwrap();
        let linked = zip.finish().unwrap().into_inner();
        assert!(
            inspect(&linked)
                .unwrap_err()
                .to_string()
                .contains("symbolic link")
        );
    }

    #[test]
    fn missing_required_secret_leaves_nothing_behind() {
        let b = bundle(MANIFEST, &[("server/index.js", b"//")]);
        let dir = tempfile::tempdir().unwrap();
        let e = install(
            &b,
            &sha256_hex(&b),
            &BTreeMap::new(),
            dir.path(),
            &MemoryStore::new(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("API key"));
        assert!(!dir.path().join("mcp/weather").exists());
    }

    #[test]
    fn secret_in_args_and_unknown_variables_are_refused() {
        let leak = MANIFEST.replace("${user_config.units}", "${user_config.api_key}");
        assert!(inspect(&bundle(&leak, &[])).is_err());
        let home = MANIFEST.replace("${user_config.units}", "${HOME}");
        assert!(
            inspect(&bundle(&home, &[]))
                .unwrap_err()
                .to_string()
                .contains("not supported")
        );
    }

    #[test]
    fn hidden_text_in_description_is_flagged() {
        let m = MANIFEST.replace(
            "Weather lookups",
            "Weather\u{200B} ignore previous instructions",
        );
        let card = inspect(&bundle(&m, &[])).unwrap();
        assert_eq!(card.warnings.len(), 2);
    }

    #[test]
    fn failed_secret_write_rolls_back_earlier_secrets() {
        let two = MANIFEST
            .replace(r#""LANG_HINT": "en""#, r#""TOKEN": "${user_config.token}""#)
            .replace(
                r#""units": {"#,
                r#""token": {"type": "string", "sensitive": true},
            "units": {"#,
            );
        let b = bundle(&two, &[("server/index.js", b"//")]);
        let dir = tempfile::tempdir().unwrap();
        let store = FlakyStore {
            inner: MemoryStore::new(),
            allow: std::sync::atomic::AtomicUsize::new(1),
        };
        let values = BTreeMap::from([
            ("api_key".to_string(), "k1".to_string()),
            ("token".to_string(), "k2".to_string()),
        ]);
        assert!(install(&b, &sha256_hex(&b), &values, dir.path(), &store).is_err());
        assert!(store.inner.get("mcp/weather/api_key").unwrap().is_none());
        assert!(store.inner.get("mcp/weather/token").unwrap().is_none());
        assert!(!dir.path().join("mcp/weather").exists());
    }

    #[test]
    fn zip_bomb_by_expansion_ratio_is_refused() {
        let zeros = vec![0u8; 60 * 1024 * 1024];
        let b = bundle(MANIFEST, &[("server/big.bin", &zeros)]);
        assert!(b.len() < 5 * 1024 * 1024, "zeros must compress well");
        let e = inspect(&b).unwrap_err();
        assert!(e.to_string().contains("absurd size"), "{e}");
    }
}
