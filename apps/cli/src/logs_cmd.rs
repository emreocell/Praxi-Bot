//! Read only the bounded structured log store; never legacy stderr files.
use std::{collections::HashSet, io::Write, path::Path, time::Duration};

#[derive(clap::Args)]
pub struct LogsCmd {
    /// Keep showing new records until Ctrl-C.
    #[arg(long)]
    follow: bool,
    /// Show only this level.
    #[arg(long, value_parser = ["trace", "debug", "info", "warn", "error"])]
    level: Option<String>,
    /// Show records since this UTC Unix millisecond timestamp, or age (e.g. 15m, 2h, 1d).
    #[arg(long, value_parser = parse_since)]
    since: Option<u64>,
}

fn parse_since(input: &str) -> Result<u64, String> {
    let suffix = input.chars().last().unwrap_or(' ');
    let multiplier = match suffix {
        's' => 1000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => 0,
    };
    if multiplier == 0 {
        return input
            .parse()
            .map_err(|_| "use a Unix millisecond timestamp or an age such as 15m, 2h, 1d".into());
    }
    let count: u64 = input[..input.len() - 1]
        .parse()
        .map_err(|_| "use an age such as 15m, 2h, 1d")?;
    let age = count
        .checked_mul(multiplier)
        .ok_or("the requested age is too large")?;
    Ok(support_logs::now_ms().saturating_sub(age))
}

pub fn run(cmd: &LogsCmd, data_dir: &Path) -> crate::Result<()> {
    let rt = crate::current_thread_runtime()?;
    rt.block_on(async {
        let mut seen = HashSet::new();
        let mut output = std::io::stdout().lock();
        loop {
            let entries = support_logs::recent(data_dir, cmd.level.as_deref(), cmd.since)
                .map_err(|_| "Recent logs could not be read. Check access to the data folder.")?;
            let current: HashSet<_> = entries.iter().map(|e| e.id.clone()).collect();
            for entry in entries {
                if !seen.contains(&entry.id) {
                    writeln!(output, "{}", serde_json::to_string(&entry)?)?;
                }
            }
            output.flush()?;
            seen = current;
            if !cmd.follow {
                return Ok(());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                result = tokio::signal::ctrl_c() => { result?; return Ok(()); }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn logs_options_and_since_reject_mistakes() {
        assert!(
            crate::Cli::try_parse_from([
                "bot", "logs", "--follow", "--level", "warn", "--since", "15m"
            ])
            .is_ok()
        );
        assert!(crate::Cli::try_parse_from(["bot", "logs", "--level", "warning"]).is_err());
        assert_eq!(parse_since("1234").unwrap(), 1234);
        assert!(parse_since("yesterday").is_err());
        assert!(parse_since("18446744073709551615d").is_err());
    }
}
