//! Release selection, service activation and rollback for installer-managed Linux hosts.
use daemon_api::host::HostUpdateState;
use semver::Version;

fn release_version(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

fn newer_release(candidate: &str, installed: &str) -> bool {
    release_version(candidate)
        .zip(release_version(installed))
        .is_some_and(|(candidate, installed)| candidate.cmp_precedence(&installed).is_gt())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
async fn check_activation<E, F, G>(
    restart_and_check: impl Fn() -> F,
    restore: impl FnOnce() -> Result<(), E>,
) -> HostUpdateState
where
    F: std::future::Future<Output = Result<(), G>>,
{
    if restart_and_check().await.is_ok() {
        return HostUpdateState::Updated;
    }
    if restore().is_ok() && restart_and_check().await.is_ok() {
        HostUpdateState::RolledBack
    } else {
        HostUpdateState::RollbackFailed
    }
}

#[cfg(target_os = "linux")]
pub(super) use systemd::run;

#[cfg(target_os = "linux")]
mod systemd {
    use super::{HostUpdateState, check_activation, newer_release, release_version};
    use crate::Result;
    use daemon_api::host::{HostUpdateStatus, UPDATE_STATUS};
    use std::{
        io::{BufRead, Read, Write},
        path::Path,
        process::Stdio,
        time::Duration,
    };

    const UNIT: &str = "praxi.service";
    const PREFIX: &str = "/opt/praxi";
    const CONFIG: &str = "/etc/praxi/updates.toml";

    #[derive(Default, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Mode {
        #[default]
        Off,
        Notify,
        Auto,
    }

    #[derive(Default, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    enum Channel {
        #[default]
        Stable,
        PreRelease,
    }

    #[derive(Default, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Settings {
        mode: Mode,
        channel: Channel,
    }

    fn settings() -> Result<Settings> {
        let path = Path::new(CONFIG);
        if !path.try_exists()? {
            return Ok(Settings::default());
        }
        super::super::trusted_root_path(path)?;
        toml::from_str(&std::fs::read_to_string(path)?)
            .map_err(|_| "invalid /etc/praxi/updates.toml; check the mode and channel".into())
    }

    fn publish(state: HostUpdateState, tag: Option<&str>) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let path = Path::new(UPDATE_STATUS);
        let file = tempfile::NamedTempFile::new_in(path.parent().ok_or("invalid status path")?)?;
        serde_json::to_writer(
            &file,
            &HostUpdateStatus {
                state,
                tag: tag.map(str::to_owned),
            },
        )?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }

    async fn main_pid() -> Result<u32> {
        let output = tokio::process::Command::new("/usr/bin/systemctl")
            .args(["show", "--property=MainPID", "--value", UNIT])
            .output()
            .await?;
        let pid: u32 = std::str::from_utf8(&output.stdout)?.trim().parse()?;
        if !output.status.success() || pid == 0 {
            return Err("the managed daemon is not running".into());
        }
        Ok(pid)
    }

    async fn requested_tag() -> Result<String> {
        // systemd passes the accepted Unix socket as stdin. A shared service UID
        // alone would also authorize subprocesses such as shell tools.
        let stdin = std::io::stdin();
        let peer = rustix::net::sockopt::socket_peercred(&stdin)?;
        if u32::try_from(peer.pid.as_raw_pid())? != main_pid().await? {
            return Err("update requests must come from the managed daemon process".into());
        }
        let mut tag = String::new();
        stdin.lock().take(102).read_line(&mut tag)?;
        if tag.len() > 101 || !tag.ends_with('\n') {
            return Err("invalid update request".into());
        }
        tag.pop();
        release_version(&tag).ok_or("invalid release tag")?;
        Ok(tag)
    }

    async fn latest(
        client: &reqwest::Client,
        channel: Channel,
    ) -> Result<Option<super::super::Release>> {
        use super::super::{API, Release, assets, bounded_body};
        let preview = matches!(channel, Channel::PreRelease);
        let endpoint = if preview {
            "releases?per_page=30"
        } else {
            "releases/latest"
        };
        let response = client.get(format!("{API}/{endpoint}")).send().await?;
        if !preview && response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = bounded_body(response.error_for_status()?, 4 * 1024 * 1024).await?;
        let releases: Vec<Release> = if preview {
            serde_json::from_slice(&bytes)?
        } else {
            vec![serde_json::from_slice(&bytes)?]
        };
        Ok(releases
            .into_iter()
            .filter_map(|release| {
                let version = release_version(&release.tag_name)?;
                (!release.draft
                    && (preview || (!release.prerelease && version.pre.is_empty()))
                    && assets(&release).is_ok())
                .then_some((version, release))
            })
            .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
            .map(|(_, release)| release))
    }

    async fn restart_and_check(prefix: &Path, port: u16) -> Result<()> {
        let executable = prefix.join("bin/bot").canonicalize()?;
        // A bad release may have exhausted Restart=on-failure before rollback.
        for action in ["reset-failed", "restart"] {
            let status = tokio::process::Command::new("/usr/bin/systemctl")
                .args([action, UNIT])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .await?;
            if !status.success() {
                return Err(format!("systemctl {action} {UNIT} failed").into());
            }
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()?;
        let url = format!("http://127.0.0.1:{port}/v1/health");
        let mut healthy_pid = None;
        for _ in 0..60 {
            if let Ok(pid) = main_pid().await
                && std::fs::read_link(format!("/proc/{pid}/exe"))
                    .as_deref()
                    .is_ok_and(|exe| exe == executable)
                && health_ok(&client, &url).await
            {
                if healthy_pid == Some(pid) {
                    return Ok(());
                }
                healthy_pid = Some(pid);
            } else {
                healthy_pid = None;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err("daemon failed its readiness check".into())
    }

    async fn health_ok(client: &reqwest::Client, url: &str) -> bool {
        async fn health(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
            let response = client.get(url).send().await?.error_for_status()?;
            Ok(serde_json::from_slice(
                &super::super::bounded_body(response, 64 * 1024).await?,
            )?)
        }
        health(client, url).await.is_ok_and(|body| {
            matches!(body["status"].as_str(), Some("ok" | "setup_required"))
                && body["host"]["mode"] == "host"
                && body["startup"] == "ready"
        })
    }

    async fn activate(prefix: &Path, target: &Path, port: u16) -> Result<HostUpdateState> {
        use super::super::{read_version_link, replace_link, switch};
        let current = read_version_link(prefix, "current")?.ok_or("missing rollback baseline")?;
        let previous = read_version_link(prefix, "previous")?;
        let restore = || -> Result<()> {
            replace_link(prefix, "current", &current)?;
            let history = match previous.as_deref() {
                Some(previous) => replace_link(prefix, "previous", previous),
                None => match std::fs::remove_file(prefix.join("previous")) {
                    Ok(()) => std::fs::File::open(prefix)
                        .and_then(|dir| dir.sync_all())
                        .map_err(Into::into),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e.into()),
                },
            };
            // If history cannot be restored, still try starting the restored daemon.
            if let Err(error) = history {
                eprintln!("Praxi rollback: could not restore version history: {error}");
            }
            Ok(())
        };
        if let Err(error) = switch(prefix, target) {
            // A rename can have completed even if the following fsync failed.
            restore()?;
            return Err(error);
        }
        Ok(check_activation(
            || async {
                restart_and_check(prefix, port)
                    .await
                    .inspect_err(|error| eprintln!("Praxi update: {error}"))
            },
            || restore().inspect_err(|error| eprintln!("Praxi rollback: {error}")),
        )
        .await)
    }

    pub(in crate::update) async fn run(port: u16, scheduled: bool) -> Result<()> {
        use super::super::{
            API, installed_tag, lock, maintenance_lock, prepare, release, release_client,
            trusted_root_path,
        };
        let prefix = Path::new(PREFIX);
        trusted_root_path(prefix)?;
        let requested = if scheduled {
            None
        } else {
            Some(requested_tag().await?)
        };
        let _maintenance = maintenance_lock()?;
        let _lock = lock(prefix)?;
        let installed = installed_tag(prefix)?;
        let client = release_client()?;
        if let Some(tag) = requested.as_deref() {
            if tag != installed && !newer_release(tag, &installed) {
                return Err("managed updates cannot downgrade the server".into());
            }
            publish(HostUpdateState::Running, Some(tag))?;
            let mut reply = std::io::stdout().lock();
            if let Err(error) = reply.write_all(b"1").and_then(|()| reply.flush()) {
                publish(HostUpdateState::Failed, Some(tag))?;
                return Err(error.into());
            }
        }
        let mut attempted = requested.clone();
        let result: Result<()> = async {
            let release = if let Some(tag) = requested.as_deref() {
                release(&client, API, Some(tag)).await?
            } else {
                let settings = settings()?;
                let status = std::fs::read(UPDATE_STATUS)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<HostUpdateStatus>(&bytes).ok());
                if matches!(settings.mode, Mode::Off) || main_pid().await.is_err() {
                    if status
                        .is_some_and(|status| matches!(status.state, HostUpdateState::Available))
                    {
                        publish(HostUpdateState::Idle, None)?;
                    }
                    return Ok(());
                }
                let Some(candidate) = latest(&client, settings.channel)
                    .await?
                    .filter(|candidate| newer_release(&candidate.tag_name, &installed))
                else {
                    if status
                        .is_some_and(|status| matches!(status.state, HostUpdateState::Available))
                    {
                        publish(HostUpdateState::Idle, None)?;
                    }
                    return Ok(());
                };
                if matches!(settings.mode, Mode::Notify) {
                    return publish(HostUpdateState::Available, Some(&candidate.tag_name));
                }
                if status.is_some_and(|status| {
                    matches!(status.state, HostUpdateState::RollbackFailed)
                        || (matches!(status.state, HostUpdateState::RolledBack)
                            && status.tag.as_deref() == Some(&candidate.tag_name))
                }) {
                    return Ok(());
                }
                attempted = Some(candidate.tag_name.clone());
                publish(HostUpdateState::Running, attempted.as_deref())?;
                candidate
            };
            let target = prepare(prefix, &client, &release).await?;
            main_pid().await?;
            let state = activate(prefix, &target, port).await?;
            publish(state, Some(&release.tag_name))?;
            Ok(())
        }
        .await;
        if result.is_err() && attempted.is_some() {
            publish(HostUpdateState::Failed, attempted.as_deref())?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn managed_update_accepts_only_newer_release_precedence() {
        assert!(newer_release("v0.1.0-alpha.10", "v0.1.0-alpha.5"));
        assert!(newer_release("v0.1.0", "v0.1.0-alpha.10"));
        for tag in [
            "v0.1.0-alpha.4",
            "v0.1.0-alpha.5",
            "v0.1.0-alpha.5+other",
            "latest",
            "v0.1.0/../",
        ] {
            assert!(!newer_release(tag, "v0.1.0-alpha.5"));
        }
    }

    #[tokio::test]
    async fn failed_activation_restores_the_previous_binary_and_checks_it() {
        let running = Cell::new("new");
        let restarted = Cell::new(vec![]);
        let state = check_activation(
            || {
                let mut starts = restarted.take();
                starts.push(running.get());
                restarted.set(starts);
                std::future::ready(if running.get() == "new" {
                    Err(())
                } else {
                    Ok(())
                })
            },
            || {
                running.set("previous");
                Ok::<_, ()>(())
            },
        )
        .await;
        assert!(matches!(state, HostUpdateState::RolledBack));
        assert_eq!(restarted.into_inner(), ["new", "previous"]);
    }

    #[tokio::test]
    async fn reports_failed_recovery_and_does_not_restore_a_healthy_update() {
        let status =
            check_activation(|| std::future::ready(Err::<(), _>(())), || Ok::<_, ()>(())).await;
        assert!(matches!(status, HostUpdateState::RollbackFailed));
        let status = check_activation(
            || std::future::ready(Ok::<_, ()>(())),
            || -> Result<(), ()> { panic!("healthy update must not roll back") },
        )
        .await;
        assert!(matches!(status, HostUpdateState::Updated));
    }
}
