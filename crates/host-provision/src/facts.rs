//! Preflight facts (parsed from `provision.sh preflight`) and the pure rules
//! that turn them into pass / warn / fail results.
use std::{cmp::Ordering, collections::HashMap};

use crate::result::{Status, StepResult};

#[derive(Debug, Default, Clone)]
pub struct Facts(HashMap<String, String>);

impl Facts {
    /// Parse `key=value` lines; anything else is ignored (server text is data).
    pub fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|l| l.split_once('='))
                .filter(|(k, _)| {
                    !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                })
                .map(|(k, v)| (k.to_owned(), v.trim().to_owned()))
                .collect(),
        )
    }

    pub fn get(&self, k: &str) -> &str {
        self.0.get(k).map_or("", String::as_str)
    }

    pub fn flag(&self, k: &str) -> bool {
        self.get(k) == "1"
    }

    pub fn num(&self, k: &str) -> u64 {
        self.get(k).parse().unwrap_or(0)
    }

    pub fn installed_version(&self) -> Option<&str> {
        Some(self.get("praxi_version")).filter(|v| !v.is_empty())
    }

    /// The server's build id from `bot --version` (`v0.1.0-alpha.3` for a release build,
    /// `0.1.0+g1a2b3c4` for an untagged one); empty for servers that predate it.
    pub fn installed_build(&self) -> Option<&str> {
        Some(self.get("praxi_build")).filter(|v| !v.is_empty())
    }

    pub fn service_active(&self) -> bool {
        self.get("unit_active") == "active"
    }
}

/// `1.2.3`, `v1.2.3`, `1.2.3-rc.1` -> (1,2,3).
pub fn semver(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let core = v.split(['-', '+']).next()?;
    let mut p = core.split('.').map(|x| x.parse::<u64>().ok());
    Some((p.next()??, p.next()??, p.next()??))
}

/// Orders two release tags including the pre-release part (`alpha.4` > `alpha.3`, and a final
/// release is newer than any pre-release of the same core). `None` when either is not a tag.
pub fn release_cmp(a: &str, b: &str) -> Option<Ordering> {
    // `0.1.0+g1a2b3c4` is an untagged local build: it has no place in release ordering.
    if a.contains('+') || b.contains('+') {
        return None;
    }
    let core = semver(a)?.cmp(&semver(b)?);
    if core != Ordering::Equal {
        return Some(core);
    }
    let pre = |v: &str| {
        v.strip_prefix('v')
            .unwrap_or(v)
            .split_once('-')
            .map(|(_, p)| p.to_owned())
    };
    Some(match (pre(a), pre(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => cmp_pre(&x, &y),
    })
}

/// Semver pre-release precedence: numeric parts by value, numeric < alphanumeric, shorter first.
fn cmp_pre(x: &str, y: &str) -> Ordering {
    let (mut xs, mut ys) = (x.split('.'), y.split('.'));
    loop {
        let o = match (xs.next(), ys.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) => match (p.parse::<u64>(), q.parse::<u64>()) {
                (Ok(p), Ok(q)) => p.cmp(&q),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => p.cmp(q),
            },
        };
        if o != Ordering::Equal {
            return o;
        }
    }
}

fn major_minor(v: &str) -> (u64, u64) {
    let mut p = v.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
    (p.next().unwrap_or(0), p.next().unwrap_or(0))
}

fn r(
    id: &'static str,
    title: &str,
    status: Status,
    detail: String,
    hint: Option<&str>,
) -> StepResult {
    StepResult::new(id, title, status, detail, hint.map(str::to_owned))
}

/// What the server already has, used by the install step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    Fresh,
    Installed {
        version: String,
        /// Build id the server reports (release tag or `<version>+g<hash>`), when it has one.
        build: Option<String>,
        host_mode: bool,
        active: bool,
    },
}

pub fn existing(f: &Facts) -> Existing {
    match f.installed_version() {
        None => Existing::Fresh,
        Some(v) => Existing::Installed {
            version: v.to_owned(),
            build: f.installed_build().map(str::to_owned),
            host_mode: f.flag("unit_hostmode"),
            active: f.service_active(),
        },
    }
}

/// All preflight checks as results (never panics on odd input).
pub fn evaluate(f: &Facts) -> Vec<StepResult> {
    let mut out = Vec::new();

    let (id, ver) = (f.get("os_id"), f.get("os_version"));
    let like = f.get("os_like");
    let (status, detail, hint) = match id {
        "ubuntu" if major_minor(ver) >= (22, 4) => (Status::Pass, format!("Ubuntu {ver}"), None),
        "debian" if major_minor(ver).0 >= 12 => (Status::Pass, format!("Debian {ver}"), None),
        "ubuntu" | "debian" => (
            Status::Fail,
            format!("{id} {ver} is too old"),
            Some("Use Ubuntu 22.04 or newer, or Debian 12 or newer."),
        ),
        "ol" | "rhel" | "fedora" | "rocky" | "almalinux" | "centos" => (
            Status::Pass,
            format!(
                "{} (RHEL family, less tested than Ubuntu/Debian)",
                f.get("os_name")
            ),
            None,
        ),
        _ if like.contains("debian") || like.contains("rhel") || like.contains("fedora") => (
            Status::Warn,
            format!("{id} {ver} is derived from a supported family; not tested"),
            None,
        ),
        _ => (
            Status::Fail,
            format!("unsupported system '{id}'"),
            Some("Praxi hosts support Ubuntu 22.04+, Debian 12+ and the RHEL family."),
        ),
    };
    out.push(r("preflight.os", "Operating system", status, detail, hint));

    let arch = f.get("arch");
    let ok = matches!(arch, "x86_64" | "aarch64");
    out.push(r(
        "preflight.arch",
        "CPU architecture",
        if ok { Status::Pass } else { Status::Fail },
        arch.to_owned(),
        (!ok).then_some("Only x86_64 and aarch64 (for example Oracle Ampere) are supported."),
    ));

    out.push(r(
        "preflight.systemd",
        "systemd",
        if f.flag("systemd") {
            Status::Pass
        } else {
            Status::Fail
        },
        if f.flag("systemd") {
            "running".into()
        } else {
            "systemd is not PID 1".into()
        },
        (!f.flag("systemd")).then_some(
            "Praxi runs as a systemd service; containers without systemd are not supported.",
        ),
    ));

    out.push(r(
        "preflight.sudo",
        "Root access without a password",
        if f.flag("sudo_n") { Status::Pass } else { Status::Fail },
        if f.num("uid") == 0 { "logged in as root".into() } else { "sudo -n works".into() },
        (!f.flag("sudo_n")).then_some("The user needs passwordless sudo (Oracle's ubuntu and opc users have it) or log in as root."),
    ));

    let ram = f.num("ram_mb");
    let (status, hint) = if ram < 2000 {
        (
            Status::Fail,
            Some("At least 2 GB RAM is required (6 GB recommended with a browser)."),
        )
    } else if ram < 6000 {
        (
            Status::Warn,
            Some("6 GB or more is recommended when the host also runs a browser."),
        )
    } else {
        (Status::Pass, None)
    };
    out.push(r(
        "preflight.ram",
        "Memory",
        status,
        format!("{ram} MB"),
        hint,
    ));

    let disk = f.num("disk_mb");
    out.push(r(
        "preflight.disk",
        "Free disk",
        if disk >= 8000 {
            Status::Pass
        } else {
            Status::Fail
        },
        format!("{disk} MB free"),
        (disk < 8000).then_some("At least 8 GB of free disk space is required."),
    ));

    out.push(r(
        "preflight.https",
        "Outbound HTTPS (github.com)",
        if f.flag("https") { Status::Pass } else { Status::Fail },
        if f.flag("https") { "reachable".into() } else if f.flag("curl") { "blocked".into() } else { "curl is not installed".into() },
        (!f.flag("https")).then_some("The server downloads the verified release from GitHub; allow outbound HTTPS and make sure curl is installed."),
    ));

    let gh = f.get("gh_version");
    out.push(r(
        "preflight.gh",
        "GitHub CLI (for release verification)",
        Status::Pass,
        if gh.is_empty() {
            "not installed; the installer step will add it".into()
        } else {
            format!("{gh} (checked during install)")
        },
        None,
    ));

    let busy_foreign = f.flag("port_busy") && !(f.flag("unit_present") && f.service_active());
    out.push(r(
        "preflight.port",
        "Local port",
        if busy_foreign {
            Status::Fail
        } else {
            Status::Pass
        },
        if busy_foreign {
            format!("something else listens on 127.0.0.1:{}", f.get("port"))
        } else {
            format!("{} is free or ours", f.get("port"))
        },
        busy_foreign.then_some("Stop the other service or reinstall with another port."),
    ));

    let (status, detail) = match existing(f) {
        Existing::Fresh => (Status::Pass, "no Praxi install found".to_owned()),
        Existing::Installed {
            version,
            host_mode,
            active,
            ..
        } => (
            Status::Pass,
            format!(
                "Praxi {version} found ({}, {})",
                if host_mode {
                    "host mode"
                } else {
                    "plain `bot serve`"
                },
                if active { "running" } else { "stopped" }
            ),
        ),
    };
    out.push(r(
        "preflight.existing",
        "Existing Praxi install",
        status,
        detail,
        None,
    ));
    out
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering::{Equal, Greater, Less};

    use super::*;

    pub fn good() -> &'static str {
        "uid=1001\nos_id=ubuntu\nos_version=24.04\nos_like=debian\nos_name=Ubuntu24.04\narch=aarch64\nsystemd=1\nsudo_n=1\nram_mb=24000\ndisk_mb=150000\ncurl=1\nhttps=1\ngh_version=\npraxi_version=\nunit_present=0\nunit_hostmode=0\nunit_active=inactive\ndata_dir=0\nport=47821\nport_busy=0\n"
    }

    #[test]
    fn good_server_passes_and_bad_ones_fail_with_hints() {
        assert!(
            evaluate(&Facts::parse(good()))
                .iter()
                .all(|c| c.status != Status::Fail)
        );
        let low = good()
            .replace("ram_mb=24000", "ram_mb=1000")
            .replace("systemd=1", "systemd=0");
        let bad: Vec<_> = evaluate(&Facts::parse(&low))
            .into_iter()
            .filter(|c| c.status == Status::Fail)
            .collect();
        assert_eq!(bad.len(), 2);
        assert!(bad.iter().all(|c| c.remediation.is_some()));
        assert_eq!(
            release_cmp("v0.1.0-alpha.4", "v0.1.0-alpha.3"),
            Some(Greater)
        );
        assert_eq!(
            release_cmp("v0.1.0-alpha.10", "v0.1.0-alpha.9"),
            Some(Greater)
        );
        assert_eq!(release_cmp("v0.1.0", "v0.1.0-alpha.9"), Some(Greater));
        assert_eq!(release_cmp("v0.1.0-alpha.4", "0.1.0-alpha.4"), Some(Equal));
        assert_eq!(release_cmp("v0.2.0-alpha.1", "v0.1.9"), Some(Greater));
        assert_eq!(release_cmp("0.1.0-alpha.3", "v0.1.0-alpha.4"), Some(Less));
        assert_eq!(release_cmp("0.1.0+g1a2b3c4", "v0.1.0-alpha.4"), None);
        assert_eq!(semver("v0.1.0"), Some((0, 1, 0)));
    }
}
