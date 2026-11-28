//! Only operational metadata reaches disk or stderr (captured by journald on
//! systemd hosts). Arbitrary tracing messages and fields are discarded, even
//! at trace level. Files are size bounded before every write, not just startup.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

pub use tracing;

/// Includes the current file: at most 10 MiB per process type.
pub const FILE_COUNT: usize = 5;
pub const FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINE: u64 = 4096;
pub const RECENT_LIMIT: usize = 500;

/// A log line contains no free-form error messages, identities or request data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub timestamp_ms: u64,
    pub id: String,
    pub level: String,
    pub component: String,
    pub fields: BTreeMap<String, String>,
}

/// Current UTC timestamp, also used by CLI `--since`.
#[must_use]
pub fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// The fixed phases of the daemon's start, run and stop that a log line may name.
fn safe_phase(value: &str) -> bool {
    matches!(
        value,
        "begin"
            | "process_guard"
            | "providers"
            | "database open"
            | "api token ready"
            | "desktop backend capabilities read"
            | "skill index built"
            | "agent built"
            | "web tools registered"
            | "interrupted runs recovered"
            | "app state built"
            | "schedule tools registered"
            | "runtime built"
            | "peer stack starting"
            | "peer stack handled"
            | "screen privileges loaded"
            | "host links installed"
            | "computer identity read"
            | "computer grants reconciled"
            | "mcp host loading configuration"
            | "mcp host ready, server connections start in background"
            | "components and apps wired"
            | "port bound"
            | "listening"
            | "telegram configuration loading"
            | "telegram configuration loaded"
            | "leader_starting"
            | "leader_ready"
            | "leader_idle"
            | "spawning"
            | "ready"
            | "stopping"
            | "stopped"
            | "failed"
            | "notify_ready"
            | "watchdog_started"
            | "watchdog_probe_failed"
            | "watchdog_probe_recovered"
    )
}

/// The fixed reasons a log line may give.
fn safe_reason(value: &str) -> bool {
    matches!(
        value,
        "retry"
            | "safe_mode"
            | "providers_changed"
            | "owner_request"
            | "shutdown_signal"
            | "startup_failed"
            | "port_in_use"
            | "data_access_denied"
            | "file_missing"
            | "child_exited"
            | "restart_skipped"
            | "component_warning"
            | "no usable provider"
            | "loop_panicked"
            | "loop_ended"
            | "loop_gave_up"
    )
}

fn safe_value(name: &str, value: &str) -> bool {
    match name {
        "method" => matches!(
            value,
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "OTHER"
        ),
        // Only MatchedPath from the router is supplied here, never req.uri().
        "route" => {
            value == "unmatched"
                || (value.starts_with("/v1/")
                    && value.len() <= 180
                    && value.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"/{}*_-".contains(&b)
                    }))
        }
        "status" => value.parse::<u16>().is_ok_and(|n| (400..600).contains(&n)),
        // Codes originate from ApiError's &'static str, never its message.
        "error_code" => {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        }
        "request_id" => uuid::Uuid::parse_str(value).is_ok(),
        "phase" => safe_phase(value),
        "reason" => safe_reason(value),
        // The name of a supervised background loop: a constant of the code, never user data.
        "loop_name" => {
            !value.is_empty()
                && value.len() <= 32
                && value.as_bytes()[0].is_ascii_lowercase()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        }
        "site" => {
            (value.starts_with("apps/") || value.starts_with("crates/"))
                && value.len() < 180
                && !value.contains("..")
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_-.:".contains(&b))
        }
        "elapsed_ms" | "interval_ms" | "restart_in_ms" | "crashes" | "failures" => {
            value.parse::<u64>().is_ok()
        }
        "exit_code" => value == "unknown" || value.parse::<i32>().is_ok(),
        _ => false,
    }
}

/// The privacy boundary: reject unknown fields, including message, error,
/// headers, tokens, paths and span data. No best-effort regex is needed for
/// arbitrary prose because arbitrary prose is never written.
#[derive(Default)]
struct Metadata(BTreeMap<String, String>);
impl Visit for Metadata {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if safe_value(field.name(), value) {
            self.0.insert(field.name().into(), value.into());
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record_str(field, &value.to_string());
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

struct Sink {
    dir: PathBuf,
    component: &'static str,
    limit: u64,
    stderr: bool,
    lock: Mutex<()>,
}

impl Sink {
    fn write(&self, entry: &Entry) -> io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("log lock unavailable"))?;
        let mut bytes = serde_json::to_vec(entry)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_LINE {
            return Ok(());
        }
        if self.stderr {
            let _ = io::stderr().lock().write_all(&bytes);
        }
        let path = self.dir.join(format!("{}.jsonl", self.component));
        if fs::metadata(&path).is_ok_and(|m| m.len() + bytes.len() as u64 > self.limit) {
            let last = self
                .dir
                .join(format!("{}.jsonl.{}", self.component, FILE_COUNT - 1));
            if let Err(e) = fs::remove_file(last)
                && e.kind() != io::ErrorKind::NotFound
            {
                return Err(e);
            }
            for n in (1..FILE_COUNT).rev() {
                let from = if n == 1 {
                    path.clone()
                } else {
                    self.dir.join(format!("{}.jsonl.{}", self.component, n - 1))
                };
                let to = self.dir.join(format!("{}.jsonl.{n}", self.component));
                if let Err(e) = fs::rename(from, to)
                    && e.kind() != io::ErrorKind::NotFound
                {
                    return Err(e);
                }
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?.write_all(&bytes)
    }
}

impl<S: Subscriber> Layer<S> for Sink {
    fn on_event(&self, event: &Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "praxi_support" {
            return;
        }
        let mut fields = Metadata::default();
        if let (Some(file), Some(line)) = (event.metadata().file(), event.metadata().line()) {
            let file = file.replace('\\', "/");
            if (file.starts_with("apps/") || file.starts_with("crates/")) && !file.contains("..") {
                fields.0.insert("site".into(), format!("{file}:{line}"));
            }
        }
        event.record(&mut fields);
        let entry = Entry {
            timestamp_ms: now_ms(),
            id: uuid::Uuid::now_v7().to_string(),
            level: event.metadata().level().to_string().to_ascii_lowercase(),
            component: self.component.into(),
            fields: fields.0,
        };
        if self.write(&entry).is_err() && self.stderr {
            // No OS error text: that can contain a profile path.
            let _ = writeln!(
                io::stderr(),
                "{{\"level\":\"error\",\"reason\":\"log_write_failed\"}}"
            );
        }
    }
}

fn sink(data_dir: &Path, component: &'static str, limit: u64, stderr: bool) -> io::Result<Sink> {
    if !matches!(component, "daemon" | "sidecar") {
        return Err(io::Error::other("unknown log component"));
    }
    let dir = data_dir.join("logs");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(Sink {
        dir,
        component,
        limit,
        stderr,
        lock: Mutex::new(()),
    })
}

/// Install once per executable. `RUST_LOG` is an `EnvFilter`, default info;
/// Unapproved fields are discarded before output, at every verbosity.
/// # Errors
/// The log directory or subscriber could not be initialized.
pub fn init(data_dir: &Path, component: &'static str) -> io::Result<()> {
    subscriber(
        data_dir,
        component,
        &std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    )?
    .try_init()
    .map_err(|_| io::Error::other("logging already initialized"))
}

/// Build an isolated subscriber (also used by embedders and hermetic tests).
/// # Errors
/// The log directory could not be initialized.
pub fn subscriber(
    data_dir: &Path,
    component: &'static str,
    filter: &str,
) -> io::Result<impl Subscriber + Send + Sync> {
    let filter = EnvFilter::builder()
        .with_regex(false)
        .parse(filter)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    Ok(tracing_subscriber::registry()
        .with(filter)
        .with(sink(data_dir, component, FILE_BYTES, true)?))
}

/// Revalidate disk content before exposing it. Old/unstructured log files are
/// never imported. Limits bound reads even if a file was externally replaced.
/// # Errors
/// Existing files could not be read.
pub fn recent(
    data_dir: &Path,
    level: Option<&str>,
    since_ms: Option<u64>,
) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for component in ["daemon", "sidecar"] {
        let mut recent = VecDeque::new();
        for n in (0..FILE_COUNT).rev() {
            let name = if n == 0 {
                format!("{component}.jsonl")
            } else {
                format!("{component}.jsonl.{n}")
            };
            let path = data_dir.join("logs").join(name);
            // Refuse symlinks so exports cannot follow an owner-created link to unrelated data.
            match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_symlink() || !m.is_file() => continue,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
            let file = match fs::File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let reader = io::BufReader::new(file.take(FILE_BYTES));
            for line in reader.split(b'\n') {
                let line = line?;
                if line.len() as u64 > MAX_LINE {
                    continue;
                }
                let Ok(mut entry) = serde_json::from_slice::<Entry>(&line) else {
                    continue;
                };
                if entry.component != component
                    || uuid::Uuid::parse_str(&entry.id).is_err()
                    || !matches!(
                        entry.level.as_str(),
                        "trace" | "debug" | "info" | "warn" | "error"
                    )
                {
                    continue;
                }
                entry.fields.retain(|k, v| safe_value(k, v));
                if level.is_some_and(|l| entry.level != l)
                    || since_ms.is_some_and(|s| entry.timestamp_ms < s)
                {
                    continue;
                }
                if recent.len() == RECENT_LIMIT {
                    recent.pop_front();
                }
                recent.push_back(entry);
            }
        }
        entries.extend(recent);
    }
    entries.sort_by(|a, b| (a.timestamp_ms, &a.id).cmp(&(b.timestamp_ms, &b.id)));
    if entries.len() > RECENT_LIMIT {
        entries.drain(..entries.len() - RECENT_LIMIT);
    }
    Ok(entries)
}

/// Sanitize sidecar stderr before keeping a tail. Legacy or dependency prose
/// may contain secrets; preserve only fixed startup hints and numeric exit codes.
#[must_use]
pub fn safe_stderr(line: &str) -> String {
    if let Ok(mut entry) = serde_json::from_str::<Entry>(line)
        && matches!(entry.component.as_str(), "daemon" | "sidecar")
        && matches!(
            entry.level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        )
        && uuid::Uuid::parse_str(&entry.id).is_ok()
    {
        entry.fields.retain(|k, v| safe_value(k, v));
        return serde_json::to_string(&entry).unwrap_or_default();
    }
    if let Some(code) = line.strip_prefix("daemon exited with code ")
        && (code == "unknown" || code.parse::<i32>().is_ok())
    {
        return line.into();
    }
    if line.contains("no usable provider") {
        return "no usable provider".into();
    }
    if [
        "address already in use",
        "port_in_use",
        "os error 10048",
        "os error 98",
    ]
    .iter()
    .any(|s| line.to_ascii_lowercase().contains(s))
    {
        return "address already in use".into();
    }
    "Service diagnostic details withheld; run bot doctor for a safe report.".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_boundary_discards_canaries_and_untrusted_fields() {
        let dir = tempfile::tempdir().unwrap();
        let subscriber = tracing_subscriber::registry()
            .with(sink(dir.path(), "daemon", FILE_BYTES, false).unwrap());
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "praxi_support", method = "POST", route = "/v1/chats/{id}/messages", status = 409u64, error_code = "setup_required", request_id = uuid::Uuid::now_v7().to_string().as_str(),
                api_key = "sk-canary-12345678901234567890", token = "Bearer canary-token-67890", email = "canary@example.test", path = r"C:\Users\canary\private", "/home/canary/private /Users/canary/private canary@example.test");
            tracing::error!(target: "praxi_support", phase = "sk-canary-12345678901234567890", reason = "canary@example.test", route = r"C:\Users\canary\private", error_code = "Bearer canary-token-67890", "ignored");
            tracing::error!(target: "dependency", "sk-canary-12345678901234567890");
        });
        let text = fs::read_to_string(dir.path().join("logs/daemon.jsonl")).unwrap();
        for canary in [
            "sk-canary",
            "canary-token",
            "canary@example",
            "canary\\",
            "/home/canary",
            "/Users/canary",
        ] {
            assert!(!text.contains(canary), "leaked {canary}");
        }
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("setup_required"));
        assert!(text.contains("/v1/chats/{id}/messages"));
        for canary in [
            "sk-canary-12345678901234567890",
            "Bearer canary-token-67890",
            "canary@example.test",
            r"C:\Users\canary\private",
            "/home/canary/private",
        ] {
            assert!(!safe_stderr(canary).contains(canary));
        }
    }

    #[test]
    fn supervision_events_keep_only_their_fixed_fields() {
        let dir = tempfile::tempdir().unwrap();
        let subscriber = tracing_subscriber::registry()
            .with(sink(dir.path(), "daemon", FILE_BYTES, false).unwrap());
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "praxi_support", reason = "loop_panicked", loop_name = "scheduler", crashes = 2u64, restart_in_ms = 2000u64, "sk-canary-12345678901234567890");
            tracing::error!(target: "praxi_support", reason = "loop_gave_up", loop_name = "Mission wakes: /home/canary", crashes = 5u64);
            tracing::info!(target: "praxi_support", phase = "watchdog_started", interval_ms = 45_000u64);
            tracing::info!(target: "praxi_support", phase = "notify_ready");
            tracing::warn!(target: "praxi_support", phase = "watchdog_probe_failed", failures = 1u64);
            tracing::warn!(target: "praxi_support", reason = "loop_panicked", loop_name = "x", interval_ms = "sk-canary-1");
        });
        let entries = recent(dir.path(), None, None).unwrap();
        let text = serde_json::to_string(&entries).unwrap();
        assert!(!text.contains("canary"), "{text}");
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].fields["loop_name"], "scheduler");
        assert_eq!(entries[0].fields["restart_in_ms"], "2000");
        assert_eq!(entries[1].fields["reason"], "loop_gave_up");
        assert!(
            !entries[1].fields.contains_key("loop_name"),
            "a name that is not a plain identifier is dropped"
        );
        assert_eq!(entries[2].fields["interval_ms"], "45000");
        assert_eq!(entries[3].fields["phase"], "notify_ready");
        assert_eq!(entries[4].fields["failures"], "1");
        assert!(!entries[5].fields.contains_key("interval_ms"));
    }

    #[test]
    fn level_filter_keeps_warnings_without_enabling_free_form_messages() {
        let dir = tempfile::tempdir().unwrap();
        let subscriber = subscriber(dir.path(), "daemon", "warn").unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "praxi_support", phase = "begin");
            tracing::debug!(target: "praxi_support", "canary@example.test");
            tracing::warn!(target: "praxi_support", reason = "component_warning", "canary@example.test");
        });
        let entries = recent(dir.path(), None, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "warn");
        assert!(!serde_json::to_string(&entries).unwrap().contains("canary"));
    }

    #[test]
    fn rotation_and_reader_are_bounded_and_filterable() {
        let dir = tempfile::tempdir().unwrap();
        let subscriber =
            tracing_subscriber::registry().with(sink(dir.path(), "daemon", 250, false).unwrap());
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..30 {
                tracing::warn!(target: "praxi_support", phase = "begin");
            }
        });
        let files: Vec<_> = fs::read_dir(dir.path().join("logs"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), FILE_COUNT);
        assert!(files.iter().all(|f| f.metadata().unwrap().len() <= 250));
        assert_eq!(
            recent(dir.path(), Some("warn"), None).unwrap().len(),
            FILE_COUNT
        );
        assert!(recent(dir.path(), Some("error"), None).unwrap().is_empty());
        assert!(recent(dir.path(), None, Some(u64::MAX)).unwrap().is_empty());
    }
}
