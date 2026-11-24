//! Kernel answers for `owner::verify_with` on Unix. Anything that cannot be
//! read fails closed (no image, no same-user, an error for the listener).
use std::collections::BTreeSet;
use std::path::PathBuf;

pub(super) struct SystemProbe;

impl crate::owner::Probe for SystemProbe {
    fn image_path(&self, pid: u32) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_link(format!("/proc/{pid}/exe")).ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            // macOS `ps -o comm=` prints the full executable path.
            let out = run("ps", &["-o", "comm=", "-p", &pid.to_string()])?;
            let path = out.trim();
            (!path.is_empty()).then(|| PathBuf::from(path))
        }
    }

    fn listener_pids(&self, port: u16) -> Result<BTreeSet<u32>, String> {
        #[cfg(target_os = "linux")]
        {
            linux_listener_pids(port)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let text = run(
                "lsof",
                &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"],
            )
            .ok_or("cannot run lsof")?;
            Ok(text
                .lines()
                .filter_map(|l| l.strip_prefix('p')?.parse().ok())
                .collect())
        }
    }

    fn same_user(&self, pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            match (
                std::fs::metadata(format!("/proc/{pid}")),
                std::fs::metadata("/proc/self"),
            ) {
                (Ok(a), Ok(b)) => a.uid() == b.uid(),
                _ => false,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let theirs = run("ps", &["-o", "uid=", "-p", &pid.to_string()]);
            let mine = run("id", &["-u"]);
            matches!((theirs, mine), (Some(a), Some(b)) if !a.trim().is_empty() && a.trim() == b.trim())
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(target_os = "linux")]
fn linux_listener_pids(port: u16) -> Result<BTreeSet<u32>, String> {
    let table = std::fs::read_to_string("/proc/net/tcp").map_err(|e| e.to_string())?;
    let inodes = listening_inodes(&table, port);
    let mut out = BTreeSet::new();
    let procs = std::fs::read_dir("/proc").map_err(|e| e.to_string())?;
    for ent in procs.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(ent.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(t) = std::fs::read_link(fd.path())
                && let Some(i) = t
                    .to_str()
                    .and_then(|s| s.strip_prefix("socket:["))
                    .and_then(|s| s.strip_suffix(']'))
                && inodes.contains(i)
            {
                out.insert(pid);
            }
        }
    }
    // A listener whose fd table we cannot read (another user) leaves a socket
    // inode without owner: fail closed.
    if !inodes.is_empty() && out.is_empty() {
        return Err("cannot resolve the owner of the listening socket".to_owned());
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
fn listening_inodes(table: &str, port: u16) -> BTreeSet<String> {
    let want = format!(":{port:04X}");
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let local = f.get(1)?;
            let (addr, _) = local.split_once(':')?;
            let ok = f.get(3) == Some(&"0A")
                && local.ends_with(&want)
                && (addr == "0100007F" || addr == "00000000");
            ok.then(|| f.get(9).map(|s| (*s).to_owned())).flatten()
        })
        .collect()
}
