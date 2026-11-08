//! Per-run append-only NDJSON log of [`protocol::Envelope`]s.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use protocol::{Envelope, RunId};

use crate::error::Result;

/// Path of the event log for a run: `<data_dir>/runs/<run_id>.ndjson`.
#[must_use]
pub fn event_log_path(data_dir: &Path, run_id: RunId) -> PathBuf {
    data_dir.join("runs").join(format!("{run_id}.ndjson"))
}

/// If the file does not end in `\n` (crash mid-write), truncate it back to the
/// last newline so new appends never glue onto a torn line.
fn trim_torn_tail(path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    f.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    f.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    let mut keep = 0u64;
    let mut pos = len;
    let mut buf = [0u8; 8192];
    while pos > 0 {
        let start = pos.saturating_sub(buf.len() as u64);
        let n = usize::try_from(pos - start).unwrap_or(buf.len());
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|b| *b == b'\n') {
            keep = start + i as u64 + 1;
            break;
        }
        pos = start;
    }
    f.set_len(keep)?;
    f.sync_data()?;
    Ok(())
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
        trim_torn_tail(&path)?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    /// Append one envelope durably.
    ///
    /// # Errors
    /// Fails on serialisation or I/O errors.
    pub fn append(&mut self, envelope: &Envelope) -> Result<()> {
        self.append_buffered(envelope)?;
        self.sync()
    }

    /// Append one envelope without forcing it to disk; call [`Self::sync`]
    /// to make a batch durable with a single `fsync`.
    ///
    /// # Errors
    /// Fails on serialisation or I/O errors.
    pub fn append_buffered(&mut self, envelope: &Envelope) -> Result<()> {
        let mut line = serde_json::to_vec(envelope)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        Ok(())
    }

    /// Flush everything appended so far to stable storage.
    ///
    /// # Errors
    /// Fails on I/O errors.
    pub fn sync(&mut self) -> Result<()> {
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
    let mut bytes = Vec::new();
    BufReader::new(file).read_to_end(&mut bytes)?;
    let ends_with_newline = bytes.last() == Some(&b'\n');
    let lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    let last = lines.len().saturating_sub(1);
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // Only a final fragment without a trailing newline may be torn.
        let torn_ok = i == last && !ends_with_newline;
        match serde_json::from_slice::<Envelope>(line) {
            Ok(env) => out.push(env),
            Err(_) if torn_ok => {}
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

    #[test]
    fn torn_multibyte_tail_is_ignored_and_reopen_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunId::new();
        let a = env(run, 0, "a");
        EventLogWriter::open(dir.path(), run)
            .unwrap()
            .append(&a)
            .unwrap();
        let path = event_log_path(dir.path(), run);
        // Half of a two-byte char 'ü' (0xC3 0xBC) at the end.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"v\":1,\"text\":\"\xC3").unwrap();
        drop(f);
        assert_eq!(read_event_log(dir.path(), run).unwrap(), vec![a.clone()]);
        let b = env(run, 1, "b");
        EventLogWriter::open(dir.path(), run)
            .unwrap()
            .append(&b)
            .unwrap();
        assert_eq!(read_event_log(dir.path(), run).unwrap(), vec![a, b]);
    }
}
