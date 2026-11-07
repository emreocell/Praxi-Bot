//! The skill model, parsing and validation.

use std::path::PathBuf;

use crate::frontmatter::{self, Value};

/// Maximum length of a skill name.
pub const MAX_NAME_LEN: usize = 64;
/// Maximum length of a skill description (one line).
pub const MAX_DESCRIPTION_LEN: usize = 200;
/// Maximum size of a `SKILL.md` file.
pub const MAX_SKILL_BYTES: usize = 64 * 1024;

/// Where a skill came from. Later variants override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Shipped with the app (repo `skills/`).
    Bundled,
    /// `<data_dir>/skills/`.
    User,
    /// `<data_dir>/bots/<id>/skills/`.
    Bot,
}

impl Source {
    /// Lowercase label for display.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Source::Bundled => "bundled",
            Source::User => "user",
            Source::Bot => "bot",
        }
    }
}

/// Optional triggers that make a skill relevant to the current situation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct When {
    /// Domains such as `github.com` (also matches subdomains).
    pub domains: Vec<String>,
    /// Application names such as `SystemSettings` (case-insensitive, `.exe` ignored).
    pub apps: Vec<String>,
    /// Free-text keywords matched against the task text.
    pub keywords: Vec<String>,
}

impl When {
    /// True when no trigger is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty() && self.apps.is_empty() && self.keywords.is_empty()
    }
}

/// A validated skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Unique name (override key).
    pub name: String,
    /// One-line description used for selection.
    pub description: String,
    /// Skill version string.
    pub version: String,
    /// Triggers.
    pub when: When,
    /// Markdown body (without frontmatter).
    pub body: String,
    /// Source layer.
    pub source: Source,
    /// Skill directory on disk; `None` for embedded bundled skills.
    pub dir: Option<PathBuf>,
}

/// Validate a skill name: `[a-z0-9._-]`, starts with alphanumeric, no `..`,
/// no trailing dot, at most [`MAX_NAME_LEN`] characters.
///
/// # Errors
/// Returns a message describing the first violated rule.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!("name must be 1-{MAX_NAME_LEN} characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
    {
        return Err("name may only contain a-z, 0-9, '-', '_' and '.'".into());
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("name must start with a letter or digit".into());
    }
    if name.contains("..") || name.ends_with('.') {
        return Err("name must not contain '..' or end with '.'".into());
    }
    Ok(())
}

fn scalar(key: &str, v: &Value) -> Result<String, String> {
    match v {
        Value::Scalar(s) => Ok(s.clone()),
        Value::List(_) => Err(format!("`{key}` must be a single value, not a list")),
    }
}

fn list(key: &str, v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::List(l) => Ok(l.clone()),
        Value::Scalar(_) => Err(format!("`{key}` must be a list like [a, b]")),
    }
}

/// Parse and validate a `SKILL.md` text.
///
/// # Errors
/// Returns a human-readable message when the file is invalid.
pub fn parse_skill(content: &str, source: Source, dir: Option<PathBuf>) -> Result<Skill, String> {
    if content.len() > MAX_SKILL_BYTES {
        return Err(format!("file is larger than {MAX_SKILL_BYTES} bytes"));
    }
    let (front_text, body) = frontmatter::split(content)?;
    let front = frontmatter::parse(&front_text)?;

    let mut name = None;
    let mut description = None;
    let mut version = None;
    for (k, v) in &front.top {
        match k.as_str() {
            "name" => name = Some(scalar(k, v)?),
            "description" => description = Some(scalar(k, v)?),
            "version" => version = Some(scalar(k, v)?),
            "when" => return Err("`when` must be a block with indented keys".into()),
            other => return Err(format!("unknown frontmatter key `{other}`")),
        }
    }
    let name = name.ok_or("missing required key `name`")?;
    validate_name(&name)?;
    let description = description.ok_or("missing required key `description`")?;
    if description.trim().is_empty() {
        return Err("`description` must not be empty".into());
    }
    if description.chars().count() > MAX_DESCRIPTION_LEN {
        return Err(format!(
            "`description` is longer than {MAX_DESCRIPTION_LEN} characters"
        ));
    }
    let version = version.unwrap_or_else(|| "0.1.0".to_string());
    if version.trim().is_empty() {
        return Err("`version` must not be empty".into());
    }

    let mut when = When::default();
    for (k, v) in &front.when {
        match k.as_str() {
            "domains" => when.domains = list(k, v)?.iter().map(|s| s.to_lowercase()).collect(),
            "apps" => when.apps = list(k, v)?,
            "keywords" => when.keywords = list(k, v)?.iter().map(|s| s.to_lowercase()).collect(),
            other => return Err(format!("unknown `when` key `{other}`")),
        }
    }
    if body.is_empty() {
        return Err("skill body is empty".into());
    }
    Ok(Skill {
        name,
        description: description.trim().to_string(),
        version,
        when,
        body,
        source,
        dir,
    })
}

/// Context used for auto-suggestion.
#[derive(Debug, Clone, Default)]
pub struct SkillContext {
    /// Domain of the current browser page (host, e.g. `github.com`).
    pub domain: Option<String>,
    /// Foreground application name or executable.
    pub app: Option<String>,
    /// Free text (task description) matched against keywords.
    pub text: Option<String>,
}

fn norm_app(s: &str) -> String {
    let s = s.trim().to_lowercase();
    s.strip_suffix(".exe").unwrap_or(&s).to_string()
}

impl Skill {
    /// True when the skill's triggers match `ctx`.
    #[must_use]
    pub fn matches(&self, ctx: &SkillContext) -> bool {
        if let Some(host) = ctx.domain.as_deref() {
            let host = host.trim().trim_end_matches('.').to_lowercase();
            if self
                .when
                .domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")))
            {
                return true;
            }
        }
        if let Some(app) = ctx.app.as_deref() {
            let app = norm_app(app);
            if !app.is_empty() && self.when.apps.iter().any(|a| norm_app(a) == app) {
                return true;
            }
        }
        if let Some(text) = ctx.text.as_deref() {
            let text = text.to_lowercase();
            if self.when.keywords.iter().any(|k| text.contains(k.as_str())) {
                return true;
            }
        }
        false
    }
}
