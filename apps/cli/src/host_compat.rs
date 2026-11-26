//! Whether this computer may send work to a paired server, from the release each one reports.
//!
//! The desktop shows the same verdict (`apps/desktop/ui/src/lib/hostState.ts`, `hostMismatch`); this
//! is the one that is enforced where the traffic leaves: the loopback proxy of every server link
//! ([`remote_api::ApiProxy::start_checked`]) and the bot RPC of [`crate::host_links`]. A change of
//! rules must be made in both places.
use remote_api::{Compatibility, RemoteHealth};
use semver::Version;
use std::sync::Arc;

/// What this build reports about itself.
pub struct LocalRelease<'a> {
    pub version: &'a str,
    pub api_version: &'a str,
    /// The published tag this binary was built from; `None` for a local build.
    pub release: Option<&'a str>,
}

impl LocalRelease<'static> {
    pub fn this_build() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            api_version: daemon_api::API_VERSION,
            release: daemon_api::release_tag(),
        }
    }
}

/// The policy handed to the proxy: judged against this build.
pub fn compatibility() -> Compatibility {
    Arc::new(|remote| check(&LocalRelease::this_build(), remote))
}

/// A published release tag: `v` and a semantic version.
fn tag_version(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

/// Every published build reports the same cargo version, so only a tag identifies a release. An
/// older server may carry the tag as its build id.
fn reported_release(remote: &RemoteHealth) -> Option<&str> {
    [remote.release_tag.as_deref(), remote.build_id.as_deref()]
        .into_iter()
        .flatten()
        .find(|tag| tag_version(tag).is_some())
}

/// `major.minor` only: a patch difference never blocks.
fn minor(version: &str) -> Vec<&str> {
    version
        .strip_prefix('v')
        .unwrap_or(version)
        .split('.')
        .take(2)
        .collect()
}

/// Both builds are known to be the same release.
fn same_build(local: &LocalRelease<'_>, remote: &RemoteHealth, host_release: Option<&str>) -> bool {
    let precedence_equal = |a: &Version, b: &Version| a.cmp_precedence(b).is_eq();
    match (
        host_release.and_then(tag_version),
        local.release.and_then(tag_version),
    ) {
        (Some(host), Some(mine)) => precedence_equal(&host, &mine),
        // Without a release tag of its own, this build can only compare version numbers.
        _ if local.release.is_none() => Version::parse(&remote.version)
            .ok()
            .zip(Version::parse(local.version).ok())
            .is_some_and(|(host, mine)| precedence_equal(&host, &mine)),
        _ => false,
    }
}

/// `Ok` when ordinary traffic may flow; `Err` says why not, in plain words.
pub fn check(local: &LocalRelease<'_>, remote: &RemoteHealth) -> Result<(), String> {
    let host_release = reported_release(remote);
    let mine = local.release.unwrap_or(local.version);
    let theirs = host_release.unwrap_or(&remote.version);
    if remote.api_version != local.api_version {
        return Err(format!(
            "the server speaks API {} but this computer speaks API {}; install the same Praxi release on both",
            or_unknown(&remote.api_version),
            local.api_version
        ));
    }
    if minor(theirs) != minor(mine) {
        return Err(format!(
            "the server runs Praxi {} and this computer runs {mine}; install the same release on both",
            or_unknown(theirs)
        ));
    }
    // This build knows its release: a server that names none (an older build, or a pre-release
    // answering with the cargo version of a stable one) is not accepted as the same.
    if local.release.is_some() && host_release.is_none() {
        return Err(format!(
            "the server does not report which Praxi release it runs; update it to {mine}"
        ));
    }
    let preview = local.release.is_some_and(|tag| tag.contains('-'))
        || host_release.is_some_and(|tag| tag.contains('-'));
    if local.release.is_some() && preview && !same_build(local, remote, host_release) {
        return Err(format!(
            "the server runs {theirs} and this computer runs {mine}; pre-releases only work with the same release on both"
        ));
    }
    Ok(())
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() { "(unknown)" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(
        version: &str,
        api: &str,
        release: Option<&str>,
        build: Option<&str>,
    ) -> RemoteHealth {
        RemoteHealth {
            version: version.into(),
            api_version: api.into(),
            release_tag: release.map(str::to_owned),
            build_id: build.map(str::to_owned),
        }
    }

    fn local(release: Option<&str>) -> LocalRelease<'_> {
        LocalRelease {
            version: "0.1.0",
            api_version: "1.0",
            release,
        }
    }

    #[test]
    fn a_stable_build_does_not_take_a_server_that_names_no_release() {
        let stable = local(Some("v0.1.0"));
        // An unidentified legacy pre-release answers with the cargo version 0.1.0.
        for unidentified in [
            remote("0.1.0", "1.0", None, None),
            remote("0.1.0", "1.0", None, Some("0.1.0+gabc1234")),
            remote("0.1.0", "1.0", Some(""), Some("")),
        ] {
            let reason = check(&stable, &unidentified).unwrap_err();
            assert!(
                reason.contains("does not report which Praxi release"),
                "{reason}"
            );
        }
        // The same release, reported either way, is accepted.
        check(&stable, &remote("0.1.0", "1.0", Some("v0.1.0"), None)).unwrap();
        check(&stable, &remote("0.1.0", "1.0", None, Some("v0.1.0"))).unwrap();
        // A different patch of the same minor is only a warning in the desktop: not blocked here.
        check(&stable, &remote("0.1.1", "1.0", Some("v0.1.1"), None)).unwrap();
    }

    #[test]
    fn pre_releases_only_work_with_the_same_release() {
        let alpha10 = local(Some("v0.1.0-alpha.10"));
        check(
            &alpha10,
            &remote("0.1.0", "1.0", Some("v0.1.0-alpha.10"), None),
        )
        .unwrap();
        // Older and newer pre-releases, a stable server, and one that names nothing.
        for other in [
            remote("0.1.0", "1.0", None, Some("v0.1.0-alpha.5")),
            remote("0.1.0", "1.0", Some("v0.1.0-alpha.11"), None),
            remote("0.1.0", "1.0", Some("v0.1.0"), None),
            remote("0.1.0", "1.0", None, None),
        ] {
            assert!(check(&alpha10, &other).is_err(), "{other:?}");
        }
        // A stable build facing a pre-release server.
        assert!(
            check(
                &local(Some("v0.1.0")),
                &remote("0.1.0", "1.0", Some("v0.1.0-alpha.10"), None)
            )
            .is_err()
        );
        // Build metadata is not a different release.
        check(
            &alpha10,
            &remote("0.1.0", "1.0", Some("v0.1.0-alpha.10+other"), None),
        )
        .unwrap();
    }

    #[test]
    fn another_api_or_minor_version_always_blocks() {
        let stable = local(Some("v0.1.0"));
        assert!(check(&stable, &remote("0.1.0", "2.0", Some("v0.1.0"), None)).is_err());
        assert!(check(&stable, &remote("0.1.0", "", Some("v0.1.0"), None)).is_err());
        assert!(check(&stable, &remote("0.2.0", "1.0", Some("v0.2.0"), None)).is_err());
        let reason = check(&stable, &remote("0.2.0", "1.0", Some("v0.2.0"), None)).unwrap_err();
        assert!(
            reason.contains("v0.2.0") && reason.contains("v0.1.0"),
            "{reason}"
        );
    }

    #[test]
    fn a_local_build_compares_version_numbers_only() {
        let dev = local(None);
        check(&dev, &remote("0.1.0", "1.0", None, None)).unwrap();
        // A tag on the server does not make a development build refuse it (patch skew is a warning).
        check(&dev, &remote("0.1.0", "1.0", Some("v0.1.0-alpha.10"), None)).unwrap();
        assert!(check(&dev, &remote("0.2.0", "1.0", None, None)).is_err());
        assert!(check(&dev, &remote("0.1.0", "2.0", None, None)).is_err());
        // The development build still refuses a server of another API, whatever the tag says.
        assert!(check(&dev, &remote("0.1.0", "", Some("v0.1.0"), None)).is_err());
    }

    #[test]
    fn this_build_is_judged_by_its_own_identity() {
        let me = LocalRelease::this_build();
        assert_eq!(me.api_version, daemon_api::API_VERSION);
        let same = remote(me.version, me.api_version, me.release, None);
        // A server that is this very build is always compatible.
        check(&me, &same).unwrap();
        compatibility()(&same).unwrap();
    }
}
