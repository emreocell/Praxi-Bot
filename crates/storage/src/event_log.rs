//! Per-run append-only NDJSON log of [`protocol::Envelope`]s.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use protocol::{Envelope, RunId};

use crate::error::Result;

/// Path of the event log for a run: `<data_dir>/runs/<run_id>.ndjson`.
#[must_use]
pub fn event_log_path(data_dir: &Path, run_id: RunId) -> PathBuf {
    data_dir.join("runs").join(format!("{run_id}.ndjson"))
}

/// Append-only writer.  Each event is one JSON line, flushed and synced.
#[derive(Debug)]
pub struct EventLogWriter {
    file: File,
}

impl EventLogWriter {
    /// Open (creating directories/file as needed) the log for appending.
    ///
    /// # Errors
    /// Fails on filesystem errors.
    pub fn open(data_dir: &Path, run_id: RunId) -> Result<Self> {
        let path = event_log_path(data_dir, run_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    /// Append one envelope durably.
    ///
    /// # Errors
    /// Fails on serialisation or I/O errors.
    pub fn append(&mut self, envelope: &Envelope) -> Result<()> {
        let mut line = serde_json::to_vec(envelope)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()?;
        Ok(())
    }
}

/// Replay a run's log in order.  A missing file yields an empty vec; a torn
/// trailing line (crash mid-write) is ignored, but corruption elsewhere is an
/// error.
///
/// # Errors
/// Fails on I/O errors or a malformed non-final line.
pub fn read_event_log(data_dir: &Path, run_id: RunId) -> Result<Vec<Envelope>> {
    let path = event_log_path(data_dir, run_id);
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let lines: Vec<String> = BufReader::new(file)
        .lines()
        .collect::<std::io::Result<_>>()?;
    let last = lines.len().saturating_sub(1);
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Envelope>(line) {
            Ok(env) => out.push(env),
            Err(_) if i == last => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Event;
    use protocol::event::MessageDelta;

    fn env(run_id: RunId, seq: u64, text: &str) -> Envelope {
        Envelope::new(
            seq,
            Event::MessageDelta(MessageDelta {
                run_id,
                text: text.into(),
            }),
        )
    }

    #[test]
    fn round_trip_and_reopen_appends() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunId::new();
        assert_eq!(read_event_log(dir.path(), run).unwrap().len(), 0);
        let a = env(run, 0, "a");
        let b = env(run, 1, "b");
        let c = env(run, 2, "ü\nmultiline");
        {
            let mut w = EventLogWriter::open(dir.path(), run).unwrap();
            w.append(&a).unwrap();
            w.append(&b).unwrap();
        }
        EventLogWriter::open(dir.path(), run)
            .unwrap()
            .append(&c)
            .unwrap();
        assert_eq!(read_event_log(dir.path(), run).unwrap(), vec![a, b, c]);
    }

    #[test]
    fn torn_last_line_is_ignored_but_mid_corruption_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunId::new();
        let a = env(run, 0, "a");
        let mut w = EventLogWriter::open(dir.path(), run).unwrap();
        w.append(&a).unwrap();
        let path = event_log_path(dir.path(), run);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"v\":1,\"seq\"").unwrap();
        assert_eq!(read_event_log(dir.path(), run).unwrap(), vec![a]);
        f.write_all(b"\n").unwrap();
        w.append(&env(run, 1, "b")).unwrap();
        assert!(read_event_log(dir.path(), run).is_err());
    }
}
