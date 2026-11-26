//! The scripts that ship inside the app and are uploaded to the server.
//! Line endings are normalised to LF so a Windows checkout cannot break them.

fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

pub fn provision_sh() -> String {
    lf(include_str!("../scripts/provision.sh"))
}

/// The capability pack catalog (also parsed by [`crate::packs`]).
pub fn packs_txt() -> String {
    lf(include_str!("../scripts/packs.txt"))
}

/// Licence facts of every pack (chip, source link, acknowledgement rule).
pub fn pack_licenses_txt() -> String {
    lf(include_str!("../scripts/pack-licenses.txt"))
}

/// The web scraping add-on's files (`packs/scrape/`): uploaded only for that pack.
pub fn scrape_service_py() -> String {
    lf(include_str!("../../../packs/scrape/praxi_scrape.py"))
}

pub fn scrape_lock() -> String {
    lf(include_str!("../../../packs/scrape/requirements.lock"))
}

pub fn scrape_unit() -> String {
    lf(include_str!("../../../packs/scrape/praxi-scrape.service"))
}

pub fn installer_sh() -> String {
    lf(include_str!("../../../scripts/install-linux.sh"))
}

pub fn setup_headless_sh() -> String {
    lf(include_str!("../../../scripts/linux/setup-headless.sh"))
}

/// The one command a person pastes into a server's web console (or any SSH
/// session) to install the release `tag` and get a pairing code back.
///
/// The text is the readable script `scripts/bootstrap.sh` with the validated
/// release tag filled in; it carries no secret of any kind. It checks the
/// installer's signed provenance before running it, so the command's trust does
/// not rest on this text being unchanged.
pub fn bootstrap_command(tag: &str) -> Result<String, crate::Error> {
    let tag = crate::validate::version_tag(tag)?;
    let script = lf(include_str!("../scripts/bootstrap.sh"));
    Ok(format!(
        "S=; [ \"$(id -u)\" -eq 0 ] || S=sudo; $S bash -s <<'PRAXI_SETUP'\nPRAXI_TAG={tag}\n{script}praxi_main </dev/null\nPRAXI_SETUP\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::bootstrap_command;

    #[test]
    fn command_pins_the_tag_verifies_provenance_and_holds_no_secret() {
        let c = bootstrap_command("0.1.0-beta.2").unwrap();
        assert!(c.contains("PRAXI_TAG=v0.1.0-beta.2\n"));
        assert!(c.contains("gh attestation verify"));
        assert!(c.contains("--source-ref \"refs/tags/${PRAXI_TAG}\""));
        assert!(c.contains("--headless"));
        assert!(c.contains("host invite --role desktop --ttl-secs 1800"));
        // the attestation check must come before the installer runs, and both run with a clean
        // environment (fixed PATH, isolated HOME), the check with the system's /usr/bin/gh
        assert!(c.contains("praxi_clean_env \"${work}\" \\\n    /usr/bin/gh attestation verify"));
        assert!(
            c.find("/usr/bin/gh attestation verify").unwrap()
                < c.find("praxi_run_installer \"${work}/install-linux.sh\"")
                    .unwrap()
        );
        assert!(!c.contains("bash \"${work}/install-linux.sh\""));
        assert!(c.contains("PATH=/usr/sbin:/usr/bin:/sbin:/bin"));
        // the package key is pinned by its full fingerprint and every https download refuses redirects
        assert!(c.contains("praxi_gh_key_fpr=7F38BBB59D064DBCB3D84D725612B36462313325"));
        assert!(c.contains("--proto-redir '=https' --tlsv1.2 https://github.com"));
        assert!(c.trim_end().ends_with("PRAXI_SETUP"));
        // only the heredoc terminator line may be at column 0 with that word
        assert_eq!(c.matches("PRAXI_SETUP").count(), 2);
        for word in ["password", "secret", "api_key", "BEGIN"] {
            assert!(!c.to_lowercase().contains(&word.to_lowercase()), "{word}");
        }
    }

    #[test]
    fn a_tag_that_could_inject_shell_is_refused() {
        for bad in [
            "v1.0.0; rm -rf /",
            "v1.0.0\nPRAXI_SETUP",
            "$(id)",
            "",
            "latest",
        ] {
            assert!(bootstrap_command(bad).is_err(), "{bad:?}");
        }
    }
}
