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
//! 1. [`inspect`] validates the archive without installing it and returns an
//!    [`Inspection`]: the approval card (command, environment variable names,
//!    runtime, secret fields, warnings) plus the bundle's SHA-256.
//! 2. [`install`] runs only with the SHA-256 the owner approved. It unpacks
//!    into `<data dir>/mcp/<id>/` (zip-slip, symlink and size checks), stores
//!    `sensitive` values in the secret store and returns an [`McpServerConfig`]
//!    whose `secret_env` points at them.
//!
//! Supports Node, Python, uv and bundled executables, platform overrides,
//! and scalar or multiple file/directory user settings.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

use secret_store::SecretStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    catalog::check_stdio_package_pin,
    config::{McpImport, McpServerConfig, McpTransport},
    error::{McpError, Result},
};
use zeroize::{Zeroize as _, Zeroizing};

mod signature;

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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    args: Option<Vec<String>>,
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
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
}

/// A setting the owner is asked for (the form of the install dialog).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// Multiple full paths, one per line in the approval form.
    pub multiple: bool,
    /// Default value as text.
    pub default: Option<String>,
}

/// The approval card: everything the owner should see before anything runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// Manifest arguments, including any substitutions.
    pub args: Vec<String>,
    /// Names of the environment variables the server will receive (no values).
    pub env_names: Vec<String>,
    /// Settings the owner must fill in.
    pub fields: Vec<ConfigField>,
    /// SHA-256 (hex) of the bundle file. The approval is for exactly these bytes.
    pub sha256: String,
    /// The file matched an independently supplied SHA-256.
    pub checksum_verified: bool,
    /// The embedded CMS signature was verified, when present.
    pub signature_verified: bool,
    /// Names of tools declared by the bundle. Actual tools are discovered on enable.
    pub tools: Vec<String>,
    /// Number of files that will be unpacked.
    pub file_count: usize,
    /// Unpacked size in bytes.
    pub unpacked_bytes: u64,
    /// Things the owner should read before approving (suspicious text, ...).
    pub warnings: BTreeSet<Warning>,
}

/// Findings from the static review of untrusted manifest text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    /// A field includes invisible formatting characters.
    HiddenText,
    /// A description appears to address the model.
    AiInstructions,
    /// The bundle supplies a native executable.
    BundledBinary,
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
    if id.is_empty() || id.contains("..") || !portable_name(&id) {
        return Err(err("the bundle has no usable name"));
    }
    Ok(id)
}

/// A relative path from an archive entry: no `..`, no root, no drive, no
/// backslash tricks. `None` = refuse.
fn safe_relative(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return None;
    }
    let path = Path::new(name);
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(part) if portable_name(&part.to_string_lossy()) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn portable_name(part: &str) -> bool {
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    !part.ends_with(['.', ' '])
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}

struct Archive {
    manifest: Manifest,
    paths: BTreeSet<PathBuf>,
    file_count: usize,
    unpacked: u64,
}

fn open_archive(bytes: &[u8]) -> Result<(zip::ZipArchive<Cursor<&[u8]>>, Archive)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| err("this file is not a valid .mcpb bundle (not a zip archive)"))?;
    if zip.len() > MAX_FILES {
        return Err(err("the bundle contains too many files"));
    }
    let mut unpacked = 0u64;
    let mut compressed = 0u64;
    let mut files = 0usize;
    let mut paths = BTreeSet::new();
    let mut file_paths = BTreeSet::new();
    for i in 0..zip.len() {
        let entry = zip
            .by_index_raw(i)
            .map_err(|_| err("the bundle is damaged"))?;
        let entry_name = entry
            .name()
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default();
        let path =
            safe_relative(&entry_name).ok_or_else(|| err("the bundle contains an unsafe path"))?;
        let folded = path.to_string_lossy().to_lowercase();
        if path
            .components()
            .next()
            .is_some_and(|part| part.as_os_str().eq_ignore_ascii_case(PIN_FILE))
            || !paths.insert(folded)
        {
            return Err(err("the bundle contains a duplicate or reserved path"));
        }
        if let Some(mode) = entry.unix_mode() {
            // S_IFLNK: links could point outside the folder.
            if mode & 0o170_000 == 0o120_000 {
                return Err(err("the bundle contains a symbolic link and was refused"));
            }
        }
        if !entry.is_dir() {
            file_paths.insert(path);
            files += 1;
            unpacked = unpacked.saturating_add(entry.size());
            compressed = compressed.saturating_add(entry.compressed_size());
        }
    }
    if unpacked > MAX_UNPACKED_BYTES {
        return Err(err("the bundle is larger than 400 MB once unpacked"));
    }
    if unpacked > BOMB_MIN_UNPACKED && unpacked / compressed.max(1) > MAX_EXPANSION_RATIO {
        return Err(err(
            "the bundle expands to an absurd size compared to its file and was refused",
        ));
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
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|_| err("manifest.json is not valid text"))?;
        if text.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(err("manifest.json is too large"));
        }
        serde_json::from_str::<Manifest>(&text)
            .map_err(|_| err("manifest.json does not match the MCPB format"))?
    };
    Ok((
        zip,
        Archive {
            manifest,
            paths: file_paths,
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
fn scan_text(text: &str, out: &mut BTreeSet<Warning>) {
    let hidden = text.chars().any(|c| {
        matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
    });
    if hidden {
        out.insert(Warning::HiddenText);
    }
    let low = text.to_lowercase();
    if PHRASES.iter().any(|p| low.contains(p)) {
        out.insert(Warning::AiInstructions);
    }
}

fn pick_launch(m: &Manifest, paths: &BTreeSet<PathBuf>) -> Result<(Runtime, McpLaunch)> {
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
        if over.args.is_some() {
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
        launch.args = Some(args);
    }
    if runtime == Runtime::Binary {
        let command = launch.command.as_deref().unwrap_or_default();
        let command = [
            "${__dirname}/",
            "${__dirname}${/}",
            "${__dirname}${pathSeparator}",
        ]
        .iter()
        .find_map(|prefix| command.strip_prefix(prefix))
        .unwrap_or(command);
        let mut path = safe_relative(command)
            .ok_or_else(|| err("the bundle executable must be inside the archive"))?;
        if cfg!(windows) && !paths.contains(&path) && path.extension().is_none() {
            path.set_extension("exe");
        }
        if !paths.contains(&path) {
            return Err(err("the bundle executable is missing from the archive"));
        }
        launch.command = Some(format!("${{__dirname}}/{}", path.display()));
    } else if m
        .server
        .entry_point
        .as_ref()
        .and_then(|entry| safe_relative(entry))
        .is_none_or(|path| !paths.contains(&path))
    {
        return Err(err("the bundle entry point is missing from the archive"));
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
    if f.multiple && (!matches!(f.kind.as_str(), "directory" | "file") || f.sensitive) {
        return Err(err("multiple values require a file or directory setting"));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        || key.is_empty()
        || key.len() > 40
    {
        return Err(err(format!("setting name `{key}` is not allowed")));
    }
    let default = f
        .default
        .as_ref()
        .filter(|_| !f.sensitive)
        .map(|v| default_setting(v, f.multiple))
        .transpose()?;
    Ok(ConfigField {
        key: key.to_string(),
        kind: f.kind.clone(),
        title: crate::registry::clean_text(f.title.as_deref().unwrap_or(key)),
        description: crate::registry::clean_text(f.description.as_deref().unwrap_or_default()),
        required: f.required && default.is_none(),
        sensitive: f.sensitive,
        multiple: f.multiple,
        default,
    })
}

fn default_setting(value: &serde_json::Value, multiple: bool) -> Result<String> {
    use serde_json::Value;
    let expand = |text: &str| {
        substitute(text, "", &BTreeMap::new(), &BTreeMap::new(), false).map(|(value, _)| value)
    };
    match value {
        Value::Array(paths) if multiple && paths.len() <= 64 => paths
            .iter()
            .map(|path| {
                expand(
                    path.as_str()
                        .ok_or_else(|| err("a default path must be text"))?,
                )
            })
            .collect::<Result<Vec<_>>>()
            .map(|paths| paths.join("\n")),
        Value::String(text) if !multiple => expand(text),
        Value::Bool(_) | Value::Number(_) if !multiple => Ok(value.to_string()),
        _ => Err(err("the bundle has an invalid setting default")),
    }
}

/// Read a bundle and describe what installing it would do.
///
/// # Errors
/// [`McpError::Config`] with a plain-language reason (not a zip, unsafe path,
/// symbolic link, too big, unsupported manifest, wrong operating system).
pub fn inspect(bytes: &[u8]) -> Result<Inspection> {
    inspect_with_checksum(bytes, None)
}

/// Read an approval card and compare a publisher-supplied SHA-256 if provided.
///
/// # Errors
/// Invalid bundle, signature or SHA-256 mismatch.
pub fn inspect_with_checksum(bytes: &[u8], expected: Option<&str>) -> Result<Inspection> {
    Ok(prepare(
        bytes,
        expected.map(|hash| (hash, "the bundle does not match the supplied SHA-256")),
    )?
    .inspection)
}

struct PreparedBundle<'a> {
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    manifest: Manifest,
    launch: McpLaunch,
    inspection: Inspection,
}

fn check_digest(actual: &str, expected: &str, message: &str) -> Result<()> {
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(err(message));
    }
    Ok(())
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.compatibility.as_ref().is_some_and(|compat| {
        !compat.platforms.is_empty() && !compat.platforms.iter().any(|p| p == current_platform())
    }) {
        return Err(err("this bundle does not support this operating system"));
    }
    if !matches!(
        manifest.manifest_version.as_str(),
        "0.1" | "0.2" | "0.3" | "0.4"
    ) {
        return Err(err("the bundle manifest version is unsupported"));
    }
    if manifest.version.len() > 128
        || semver::Version::parse(&manifest.version).is_err()
        || manifest.description.trim().is_empty()
        || manifest
            .author
            .as_ref()
            .is_none_or(|author| author.name.trim().is_empty())
    {
        return Err(err(
            "the bundle manifest needs a valid version, description and author",
        ));
    }
    Ok(())
}

fn prepare<'a>(bytes: &'a [u8], expected: Option<(&str, &str)>) -> Result<PreparedBundle<'a>> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(err("the bundle is larger than 100 MB"));
    }
    let sha256 = sha256_hex(bytes);
    if let Some((expected, message)) = expected {
        check_digest(&sha256, expected, message)?;
    }
    let (zip_bytes, signature) = signature::split(bytes)?;
    if let Some(signed) = &signature {
        signature::verify(signed.signed_content, signed.der)?;
    }
    let (zip, archive) = open_archive(zip_bytes)?;
    let m = archive.manifest;
    validate_manifest(&m)?;
    let id = id_from_name(&m.name)?;
    let (runtime, launch) = pick_launch(&m, &archive.paths)?;
    let args = launch.args.as_deref().unwrap_or_default();
    if m.user_config.len() > 50 || launch.env.len() > 50 || args.len() > 100 {
        return Err(err("the bundle has too many launch settings"));
    }
    let mut fields = Vec::new();
    for (key, f) in &m.user_config {
        fields.push(field_of(key, f)?);
    }
    let command = launch.command.clone().unwrap_or_default();
    if std::iter::once(&command)
        .chain(args)
        .any(|arg| arg.len() > 4096 || arg.chars().any(crate::registry::unsafe_display_char))
    {
        return Err(err("the bundle contains an invalid command or argument"));
    }
    let mut warnings = BTreeSet::new();
    scan_text(&m.description, &mut warnings);
    if let Some(long) = &m.long_description {
        scan_text(long, &mut warnings);
    }
    for t in &m.tools {
        scan_text(&t.name, &mut warnings);
        scan_text(&t.description, &mut warnings);
    }
    if let Some(a) = &m.author {
        scan_text(&a.name, &mut warnings);
    }
    if runtime == Runtime::Binary {
        warnings.insert(Warning::BundledBinary);
    }
    let inspection = Inspection {
        id,
        display_name: crate::registry::clean_text(m.display_name.as_deref().unwrap_or(&m.name)),
        version: crate::registry::clean_text(&m.version),
        description: crate::registry::clean_text(&m.description),
        author: m
            .author
            .as_ref()
            .map(|a| crate::registry::clean_text(&a.name)),
        runtime,
        command,
        args: args.to_vec(),
        env_names: launch.env.keys().cloned().collect(),
        fields,
        sha256,
        checksum_verified: expected.is_some(),
        signature_verified: signature.is_some(),
        tools: m
            .tools
            .iter()
            .take(100)
            .map(|tool| crate::registry::clean_text(&tool.name))
            .collect(),
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
    let resolved = resolve(
        "mcp/import-probe",
        &launch,
        &m.user_config,
        &probe,
        Path::new("/bundle"),
    )?;
    check_stdio_package_pin(&inspection.id, &resolved.command, &resolved.args)?;
    McpServerConfig::stdio(&inspection.id, resolved.command, resolved.args).validate()?;
    Ok(PreparedBundle {
        zip,
        manifest: m,
        launch,
        inspection,
    })
}

struct Resolved {
    command: String,
    args: Vec<String>,
    secret_env: BTreeMap<String, String>,
    secrets: PendingSecrets,
}

pub(crate) type PendingSecrets = Vec<(String, Zeroizing<String>)>;

pub(crate) fn new_secret_namespace() -> String {
    format!("mcp/import-{}", uuid::Uuid::now_v7())
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
            .ok_or_else(|| err("a template has an unclosed variable"))?;
        let var = &after[..end];
        count += 1;
        if var == "__dirname" {
            out.push_str(dir);
        } else if var == "/" || var == "pathSeparator" {
            out.push(std::path::MAIN_SEPARATOR);
        } else if matches!(var, "HOME" | "DESKTOP" | "DOCUMENTS" | "DOWNLOADS") {
            let dirs = directories::UserDirs::new()
                .ok_or_else(|| err("the requested user directory is unavailable"))?;
            let path = match var {
                "HOME" => Some(dirs.home_dir()),
                "DESKTOP" => dirs.desktop_dir(),
                "DOCUMENTS" => dirs.document_dir(),
                _ => dirs.download_dir(),
            }
            .ok_or_else(|| err("the requested user directory is unavailable"))?;
            out.push_str(&path.to_string_lossy());
        } else if let Some(key) = var.strip_prefix("user_config.") {
            let field = fields
                .get(key)
                .ok_or_else(|| err(format!("the bundle uses an undeclared setting `{key}`")))?;
            if field.multiple {
                return Err(err(
                    "a multiple path setting must be passed as its own argument",
                ));
            }
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
            return Err(err(
                "a launch template uses a variable that is not supported by Praxi",
            ));
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok((out, sensitive))
}

fn resolve(
    namespace: &str,
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
    let template = launch.command.as_deref().unwrap_or_default();
    if template.contains("${user_config.") {
        return Err(err(
            "a bundle cannot select its executable through a setting",
        ));
    }
    let (command, _) = substitute(template, &dir_s, fields, values, false)?;
    no_escape(&command)?;
    let mut args = Vec::new();
    for a in launch.args.iter().flatten() {
        if let Some(key) = a
            .strip_prefix("${user_config.")
            .and_then(|s| s.strip_suffix('}'))
            && fields.get(key).is_some_and(|field| field.multiple)
        {
            args.extend(
                values
                    .get(key)
                    .into_iter()
                    .flat_map(|paths| paths.lines().map(str::to_owned)),
            );
            continue;
        }
        let (v, _) = substitute(a, &dir_s, fields, values, false)?;
        no_escape(&v)?;
        args.push(v);
    }
    let mut secret_env = BTreeMap::new();
    let mut secrets = Vec::new();
    let mut names = BTreeSet::new();
    for (index, (name, tmpl)) in launch.env.iter().enumerate() {
        if !valid_env_name(name) || !names.insert(name.to_ascii_uppercase()) {
            return Err(err(
                "the bundle has an invalid or duplicate environment name",
            ));
        }
        let (v, sensitive) = substitute(tmpl, &dir_s, fields, values, true)?;
        let value = if let Some(key) = sensitive {
            let Some(value) = values.get(&key).filter(|v| !v.is_empty()) else {
                // An optional secret left empty: the variable is simply not set.
                continue;
            };
            value.clone()
        } else {
            v
        };
        if value.contains('\0') || value.len() > 64 * 1024 {
            return Err(err("the bundle environment value is invalid"));
        }
        let reference = format!("{namespace}/{index}");
        secret_env.insert(name.clone(), reference.clone());
        secrets.push((reference, Zeroizing::new(value)));
    }
    Ok(Resolved {
        command,
        args,
        secret_env,
        secrets,
    })
}

/// Variables that change how the launched program, its package manager or its
/// network stack behaves. The review card shows only names, so a publisher
/// cannot set these, and a description cannot talk the owner into doing so.
fn reserved_env_name(name: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "LD_",
        "DYLD_",
        "NPM_CONFIG_",
        "PIP_",
        "UV_",
        "YARN_",
        "PYTHON",
        "NODE_",
    ];
    const NAMES: [&str; 20] = [
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "SYSTEMROOT",
        "BASH_ENV",
        "ENV",
        "PERL5OPT",
        "RUBYOPT",
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "HOME",
        "USERPROFILE",
    ];
    let upper = name.to_ascii_uppercase();
    NAMES.contains(&upper.as_str()) || PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
}

pub(crate) fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !reserved_env_name(name)
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
        let value = if f.multiple {
            value
                .lines()
                .filter(|path| !path.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            value
        };
        match f.kind.as_str() {
            "number"
                if !value.parse::<f64>().is_ok_and(|n| {
                    n.is_finite()
                        && f.min.is_none_or(|min| n >= min)
                        && f.max.is_none_or(|max| n <= max)
                }) =>
            {
                return Err(err(format!(
                    "“{}” must be a number in the allowed range",
                    field.title
                )));
            }
            "boolean" if !matches!(value.as_str(), "true" | "false") => {
                return Err(err(format!("“{}” must be true or false", field.title)));
            }
            "directory" | "file"
                if (f.required && value.is_empty())
                    || if f.multiple {
                        value.lines().count() > 64
                            || value.lines().any(|path| !Path::new(path).is_absolute())
                    } else {
                        !Path::new(&value).is_absolute()
                    } =>
            {
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
    install_with(bytes, approved_sha256, values, data_dir, secrets, Ok)
}

/// Install and persist the result through `publish`. If saving the config
/// fails, the new files and secrets are rolled back.
///
/// # Errors
/// [`install`] errors or an error returned by `publish`.
pub fn install_with<T, E: From<McpError>>(
    bytes: &[u8],
    approved_sha256: &str,
    values: &BTreeMap<String, String>,
    data_dir: &Path,
    secrets: &dyn SecretStore,
    publish: impl FnOnce(McpServerConfig) -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let PreparedBundle {
        mut zip,
        manifest,
        launch,
        inspection,
    } = prepare(
        bytes,
        Some((
            approved_sha256,
            "the file changed after you approved it; please open it again",
        )),
    )?;

    let root = data_dir.join("mcp");
    let io = |e: std::io::Error| McpError::Store(format!("cannot unpack the bundle: {e}"));
    fs::create_dir_all(&root).map_err(io)?;
    check_directory(&root)?;
    let target = root.canonicalize().map_err(io)?.join(&inspection.id);
    if target.symlink_metadata().is_ok() {
        return Err(err(format!(
            "`{}` is already installed; remove it first",
            inspection.id
        ))
        .into());
    }
    let mut values = check_values(&manifest.user_config, values)?;
    let resolved = resolve(
        &new_secret_namespace(),
        &launch,
        &manifest.user_config,
        &values,
        &target,
    );
    values.values_mut().for_each(String::zeroize);
    let resolved = resolved?;
    let mut config = McpServerConfig::stdio(inspection.id, resolved.command, resolved.args);
    let McpTransport::Stdio { cwd, .. } = &mut config.transport else {
        unreachable!()
    };
    *cwd = Some(target.clone());
    config.enabled = false;
    config.secret_env = resolved.secret_env;
    config.import = Some(McpImport {
        bundle_sha256: Some(inspection.sha256.clone()),
        secrets: resolved
            .secrets
            .iter()
            .map(|(name, _)| name.clone())
            .collect(),
    });
    config.validate()?;

    let partial = tempfile::Builder::new()
        .prefix(".bundle-")
        .tempdir_in(&root)
        .map_err(io)?;
    unpack(&mut zip, partial.path())?;
    let pin = serde_json::json!({
        "sha256": inspection.sha256, "name": manifest.name, "version": manifest.version,
    });
    let mut pin_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial.path().join(PIN_FILE))
        .map_err(io)?;
    std::io::Write::write_all(&mut pin_file, pin.to_string().as_bytes()).map_err(io)?;
    drop(pin_file);
    let saved = save_secrets(secrets, &resolved.secrets)?;
    if let Err(error) = fs::rename(partial.path(), &target).map_err(io) {
        saved.undo(secrets);
        return Err(error.into());
    }
    let outcome = publish(config);
    if outcome.is_err() {
        saved.undo(secrets);
        let _ = fs::remove_dir_all(target);
    }
    outcome
}

fn check_directory(path: &Path) -> Result<()> {
    let metadata = path
        .symlink_metadata()
        .map_err(|e| McpError::Store(e.to_string()))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(err("the bundle directory is a reparse point"));
        }
    }
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(err("the bundle directory is not a regular directory"));
    }
    Ok(())
}

/// Remove the files of an installed bundle by its recorded server id.
///
/// # Errors
/// An unsafe or inaccessible bundle directory.
pub fn remove_files(data_dir: &Path, id: &str) -> Result<()> {
    if id_from_name(id)? != id {
        return Err(err("invalid installed bundle id"));
    }
    let root = data_dir.join("mcp");
    if let Err(error) = root.symlink_metadata()
        && error.kind() == std::io::ErrorKind::NotFound
    {
        return Ok(());
    }
    check_directory(&root)?;
    let target = root.join(id);
    if let Err(error) = target.symlink_metadata()
        && error.kind() == std::io::ErrorKind::NotFound
    {
        return Ok(());
    }
    check_directory(&target)?;
    fs::remove_dir_all(target).map_err(|e| McpError::Store(e.to_string()))
}

/// New secrets written by [`save_secrets`].
pub(crate) struct SavedSecrets(Vec<String>);

impl SavedSecrets {
    /// Remove the new secrets if config persistence fails.
    pub(crate) fn undo(self, secrets: &dyn SecretStore) {
        for name in self.0.into_iter().rev() {
            let _ = secrets.delete(&name);
        }
    }
}

/// Write all `(name, value)` pairs; when one fails, the ones already written
/// are rolled back so a failed install leaves no stray secret.
pub(crate) fn save_secrets(
    secrets: &dyn SecretStore,
    list: &PendingSecrets,
) -> Result<SavedSecrets> {
    let mut saved = SavedSecrets(Vec::new());
    for (name, value) in list {
        if !matches!(secrets.get(name), Ok(None)) || secrets.set(name, value).is_err() {
            saved.undo(secrets);
            return Err(McpError::Store("cannot save a secret setting".into()));
        }
        saved.0.push(name.clone());
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
      "description": "Weather lookups", "author": {"name": "Fixture publisher"},
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
        assert_eq!(card.args[0], "${__dirname}/server/index.js");
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
        assert!(!cfg.enabled);
        assert_eq!(
            store
                .get(&cfg.secret_env["API_KEY"])
                .unwrap()
                .unwrap()
                .expose(),
            "sk-live-123"
        );
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("sk-live-123"));
        let McpTransport::Stdio { args, env, .. } = &cfg.transport else {
            panic!()
        };
        assert!(args[0].ends_with("index.js") && args[1] == "metric");
        assert!(env.is_empty());
        assert_eq!(
            store
                .get(&cfg.secret_env["LANG_HINT"])
                .unwrap()
                .unwrap()
                .expose(),
            "en"
        );
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
        let home = MANIFEST.replace("${user_config.units}", "${UNRECOGNIZED}");
        assert!(
            inspect(&bundle(&home, &[("server/index.js", b"//")]))
                .unwrap_err()
                .to_string()
                .contains("not supported")
        );
    }

    #[test]
    fn loader_environment_names_and_unpinned_package_runners_are_refused() {
        let files = [("server/index.js", b"//".as_slice())];
        for name in ["NODE_OPTIONS", "npm_config_registry", "LD_PRELOAD", "PATH"] {
            let m = MANIFEST.replace("\"LANG_HINT\"", &format!("\"{name}\""));
            assert_eq!(
                inspect(&bundle(&m, &files)).unwrap_err().to_string(),
                "invalid MCP server config: the bundle has an invalid or duplicate environment name"
            );
        }
        let unpinned = MANIFEST
            .replace("\"command\": \"node\"", "\"command\": \"npx\"")
            .replace("\"${__dirname}/server/index.js\"", "\"-y\", \"@scope/pkg\"");
        assert!(inspect(&bundle(&unpinned, &files)).is_err());
    }

    #[test]
    fn hidden_text_in_description_is_flagged() {
        let m = MANIFEST.replace(
            "Weather lookups",
            "Weather\u{200B} ignore previous instructions",
        );
        let card = inspect(&bundle(&m, &[("server/index.js", b"//")])).unwrap();
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
        assert_eq!(store.inner.list_names().unwrap(), Vec::<String>::new());
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

    #[test]
    fn fixture_signatures_and_independent_checksums_cover_the_exact_bytes() {
        let unsigned = include_bytes!("../tests/fixtures/weather.mcpb");
        let signed = include_bytes!("../tests/fixtures/weather-signed.mcpb");
        let comment_signature = include_bytes!("../tests/fixtures/weather-signed-zip-comment.mcpb");
        let expected = sha256_hex(signed);
        let card = inspect_with_checksum(signed, Some(&expected.to_ascii_uppercase())).unwrap();
        assert_eq!(card.sha256, expected);
        assert!(card.signature_verified && card.checksum_verified);
        assert!(inspect(comment_signature).unwrap().signature_verified);
        assert_eq!(card.tools, ["forecast"]);
        assert!(!inspect(unsigned).unwrap().signature_verified);
        assert!(inspect_with_checksum(unsigned, Some(&expected)).is_err());

        let mut tampered = signed.to_vec();
        tampered[60] ^= 1;
        assert!(
            inspect(&tampered)
                .unwrap_err()
                .to_string()
                .contains("signature")
        );
        let mut malformed = signed.to_vec();
        *malformed.last_mut().unwrap() ^= 1;
        assert!(inspect(&malformed).is_err());
        let mut malformed_comment = comment_signature.to_vec();
        *malformed_comment.last_mut().unwrap() ^= 1;
        assert!(inspect(&malformed_comment).is_err());
    }

    #[test]
    fn count_size_and_platform_unsafe_paths_are_rejected() {
        let unsafe_names = [
            "server/CON.txt",
            "server/drive:C",
            "server/trailing. ",
            "server\\escape",
            PIN_FILE,
        ];
        for name in unsafe_names {
            let archive = bundle(MANIFEST, &[(name, b"test")]);
            assert!(inspect(&archive).is_err(), "{name}");
        }
        let paths: Vec<_> = (0..MAX_FILES).map(|i| format!("server/{i}")).collect();
        let entries: Vec<_> = paths.iter().map(|name| (name.as_str(), &[][..])).collect();
        assert!(
            inspect(&bundle(MANIFEST, &entries))
                .unwrap_err()
                .to_string()
                .contains("too many files")
        );

        let mut archive = bundle(MANIFEST, &[("server/index.js", b"//")]);
        let offset = archive
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        let declared_size = u32::try_from(MAX_UNPACKED_BYTES + 1).unwrap();
        archive[offset + 24..offset + 28].copy_from_slice(&declared_size.to_le_bytes());
        assert!(
            inspect(&archive)
                .unwrap_err()
                .to_string()
                .contains("400 MB")
        );
    }

    #[test]
    fn supports_multiple_paths_and_rolls_back_a_failed_configuration_write() {
        let manifest = MANIFEST
            .replace(
                r#""type": "string", "default": "metric""#,
                r#""type": "directory", "multiple": true, "required": true"#,
            )
            .replace(
                r#""title": "API key", "required""#,
                r#""title": "API key", "default": "ignored-secret-default", "required""#,
            );
        let archive = bundle(&manifest, &[("server/index.js", b"//")]);
        let inspection = inspect(&archive).unwrap();
        assert!(inspection.fields.iter().any(|f| f.multiple));
        assert!(
            !serde_json::to_string(&inspection)
                .unwrap()
                .contains("ignored-secret-default")
        );
        let dir = tempfile::tempdir().unwrap();
        let paths = [dir.path().join("one"), dir.path().join("two")];
        let values = BTreeMap::from([
            ("api_key".to_owned(), "fixture-input".to_owned()),
            (
                "units".to_owned(),
                format!("{}\n{}", paths[0].display(), paths[1].display()),
            ),
        ]);
        let store = MemoryStore::new();
        let result = install_with(
            &archive,
            &inspection.sha256,
            &values,
            dir.path(),
            &store,
            |cfg| {
                let McpTransport::Stdio { args, .. } = cfg.transport else {
                    panic!()
                };
                assert_eq!(args.len(), 3);
                assert!(
                    args[1..]
                        .iter()
                        .zip(&paths)
                        .all(|(arg, path)| Path::new(arg) == path)
                );
                assert_ne!(store.list_names().unwrap(), Vec::<String>::new());
                Err::<(), _>(McpError::Store("fixture configuration write failed".into()))
            },
        );
        assert!(result.is_err());
        assert_eq!(store.list_names().unwrap(), Vec::<String>::new());
        assert!(!dir.path().join("mcp/weather").exists());
    }
}
