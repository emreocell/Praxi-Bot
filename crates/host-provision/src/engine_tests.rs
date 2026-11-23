//! Engine behaviour against a fake `Transport` (no ssh, no network).
use std::{cell::RefCell, collections::HashMap, time::Duration};

use super::*;
use crate::known_hosts::ScannedKey;

const ED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

fn good_facts() -> String {
    "uid=1001\nos_id=ubuntu\nos_version=24.04\nos_like=debian\nos_name=Ubuntu\narch=aarch64\nsystemd=1\nsudo_n=1\nram_mb=24000\ndisk_mb=150000\ncurl=1\nhttps=1\ngh_version=\npraxi_version=\nunit_present=0\nunit_hostmode=0\nunit_active=inactive\ndata_dir=0\nport=47821\nport_busy=0\n".into()
}

fn installed_facts() -> String {
    good_facts()
        .replace("praxi_version=", "praxi_version=0.1.0")
        .replace("unit_present=0", "unit_present=1")
        .replace("unit_hostmode=0", "unit_hostmode=1")
        .replace("unit_active=inactive", "unit_active=active")
        .replace("data_dir=0", "data_dir=1")
}

struct Fake {
    keys: Vec<ScannedKey>,
    preflight: RefCell<String>,
    fail: RefCell<HashMap<&'static str, i32>>,
    fail_stderr: RefCell<String>,
    desktops: RefCell<Vec<String>>,
    calls: RefCell<Vec<(RemoteCmd, String)>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            keys: vec![ScannedKey {
                key_type: "ssh-ed25519".into(),
                key_b64: ED.into(),
            }],
            preflight: RefCell::new(good_facts()),
            fail: RefCell::new(HashMap::new()),
            fail_stderr: RefCell::new("boom".into()),
            desktops: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn ran(&self, cmd: RemoteCmd) -> bool {
        self.calls.borrow().iter().any(|(c, _)| *c == cmd)
    }
}

impl Transport for Fake {
    fn scan_host_keys(&self) -> Result<Vec<ScannedKey>, TransportError> {
        Ok(self.keys.clone())
    }

    fn run(&self, cmd: RemoteCmd, stdin: &[u8], _t: Duration) -> Result<Output, TransportError> {
        self.calls
            .borrow_mut()
            .push((cmd, String::from_utf8_lossy(stdin).into_owned()));
        let name = match cmd {
            RemoteCmd::Provision(Sub::Install) => "install",
            RemoteCmd::Provision(Sub::Status) => "status",
            RemoteCmd::Provision(Sub::Screen) => "screen",
            _ => "",
        };
        if let Some(code) = self.fail.borrow().get(name) {
            return Ok(Output {
                code: *code,
                stderr: self.fail_stderr.borrow().clone(),
                ..Output::default()
            });
        }
        let stdout = match cmd {
            RemoteCmd::Probe => "praxi-ok\n".to_owned(),
            RemoteCmd::Provision(Sub::Preflight) => self.preflight.borrow().clone(),
            RemoteCmd::Provision(Sub::Status) => {
                "version=0.1.0\nunit_active=active\nunit_hostmode=1\nxvfb=active\ndesktop=active\nbrowser=1\n"
                    .into()
            }
            RemoteCmd::Provision(Sub::Devices) => {
                let peers: Vec<_> = self
                    .desktops
                    .borrow()
                    .iter()
                    .map(|k| serde_json::json!({"key": k, "role": "desktop"}))
                    .collect();
                serde_json::json!({ "peers": peers }).to_string()
            }
            RemoteCmd::Provision(Sub::Invite) => {
                // the pairing consumes the ticket: our desktop key appears on the host
                self.desktops.borrow_mut().push("deskkey1".into());
                r#"{"ticket":"TICKET","code":"123456"}"#.into()
            }
            _ => String::new(),
        };
        Ok(Output {
            code: 0,
            stdout,
            ..Output::default()
        })
    }
}

#[derive(Default)]
struct Local {
    /// This computer was paired with the host before the run (by ticket).
    preexisting: bool,
    paired: RefCell<bool>,
    forgotten: RefCell<Vec<String>>,
}

impl LocalHost for Local {
    fn pair(&self, ticket: &str) -> Result<PairedHost, String> {
        assert_eq!(ticket, "TICKET");
        *self.paired.borrow_mut() = true;
        Ok(PairedHost {
            id: "hostid1".into(),
            name: "oracle".into(),
        })
    }
    fn knows_host(&self, id: &str) -> bool {
        *self.paired.borrow() && id == "hostid1"
    }
    fn health(&self, _id: &str) -> Result<HostHealth, String> {
        Ok(HostHealth {
            mode: "host".into(),
            version: "0.1.0".into(),
            has_display: true,
            has_browser: true,
            view_targets: Some(1),
        })
    }
    fn revoke_host(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }
    fn host_ids(&self) -> Vec<String> {
        if self.preexisting {
            vec!["hostid1".into()]
        } else {
            Vec::new()
        }
    }
    fn forget_host(&self, id: &str) -> Result<(), String> {
        self.forgotten.borrow_mut().push(id.into());
        *self.paired.borrow_mut() = false;
        Ok(())
    }
}

#[derive(Default)]
struct TestUi {
    accept: bool,
    asked: usize,
    cancel_after_pair: Option<std::rc::Rc<std::cell::Cell<bool>>>,
}

impl Ui for TestUi {
    fn progress(&mut self, _r: &StepResult) {}
    fn note(&mut self, _l: &str) {}
    fn cancelled(&self) -> bool {
        self.cancel_after_pair.as_ref().is_some_and(|c| c.get())
    }
    fn confirm_host_key(&mut self, _h: &str, _p: u16, _fp: &str) -> bool {
        self.asked += 1;
        self.accept
    }
}

fn record() -> HostRecord {
    HostRecord {
        name: "oracle".into(),
        user: "ubuntu".into(),
        host: "203.0.113.7".into(),
        port: 22,
        key_path: None,
        fingerprint: None,
        paired_host_id: None,
        desktop_key: None,
        installed_version: None,
    }
}

fn run(dir: &std::path::Path, fake: &Fake, local: &Local, ui: &mut TestUi, mode: Mode) -> Report {
    run_with(dir, fake, local, ui, mode, true)
}

fn run_with(
    dir: &std::path::Path,
    fake: &Fake,
    local: &Local,
    ui: &mut TestUi,
    mode: Mode,
    allow_install: bool,
) -> Report {
    run_full(dir, fake, local, ui, mode, allow_install, None)
}

fn run_full(
    dir: &std::path::Path,
    fake: &Fake,
    local: &Local,
    ui: &mut TestUi,
    mode: Mode,
    allow_install: bool,
    release_tag: Option<&str>,
) -> Report {
    let store = Store::new(dir.join("hosts"));
    let kh = KnownHosts::new(store.known_hosts_path());
    let record = store.load("oracle").unwrap_or_else(record);
    let mut engine = Engine {
        transport: fake,
        local,
        ui,
        store: &store,
        known_hosts: &kh,
        params: Params {
            record,
            mode,
            app_version: "0.1.0".into(),
            release_tag: release_tag.map(str::to_owned),
            desktop: true,
            retry_delay: Duration::ZERO,
            allow_install,
        },
    };
    engine.run()
}

fn status_of(r: &Report, id: &str) -> Status {
    r.results
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("no step {id}"))
        .status
}

#[test]
fn happy_path_installs_pairs_and_checks_health() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, local, mut ui) = (
        Fake::new(),
        Local::default(),
        TestUi {
            accept: true,
            asked: 0,
            cancel_after_pair: None,
        },
    );
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert!(r.ok(), "{}", r.table());
    for id in [
        "hostkey",
        "connect",
        "install.run",
        "pair",
        "health.browser",
        "health.version",
        "health.live_view",
    ] {
        assert_eq!(status_of(&r, id), Status::Pass, "{id}");
    }
    let calls = fake.calls.borrow();
    let install = calls
        .iter()
        .find(|(c, _)| *c == RemoteCmd::Provision(Sub::Install))
        .unwrap();
    assert_eq!(
        install.1, "version=v0.1.0\ndesktop=1\n",
        "parameters go on stdin, not in a command"
    );
    let screen = calls
        .iter()
        .find(|(c, _)| *c == RemoteCmd::Provision(Sub::Screen))
        .unwrap();
    assert_eq!(screen.1, "key=deskkey1\n");
    // the ticket never reaches the log file
    let log = std::fs::read_to_string(dir.path().join("hosts/oracle/provision.log")).unwrap();
    assert!(!log.contains("TICKET") && !log.contains("123456"));
}

#[test]
fn failing_preflight_stops_before_anything_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, local, mut ui) = (
        Fake::new(),
        Local::default(),
        TestUi {
            accept: true,
            asked: 0,
            cancel_after_pair: None,
        },
    );
    *fake.preflight.borrow_mut() = good_facts().replace("ram_mb=24000", "ram_mb=900");
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert!(!r.ok());
    assert_eq!(status_of(&r, "preflight.ram"), Status::Fail);
    assert!(
        !fake.ran(RemoteCmd::Provision(Sub::Install))
            && !fake.ran(RemoteCmd::Stage(StageFile::Installer))
    );
}

#[test]
fn host_key_mismatch_is_a_hard_stop_that_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("hosts"));
    let kh = KnownHosts::new(store.known_hosts_path());
    let other = ScannedKey {
        key_type: "ssh-ed25519".into(),
        key_b64: "AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
    };
    kh.pin("203.0.113.7", 22, &other).unwrap();
    let (fake, local, mut ui) = (
        Fake::new(),
        Local::default(),
        TestUi {
            accept: true,
            asked: 0,
            cancel_after_pair: None,
        },
    );
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert!(r.hard_stop && !r.ok());
    assert_eq!(ui.asked, 0, "a mismatch never offers to accept");
    assert!(
        fake.calls
            .borrow()
            .iter()
            .all(|(c, _)| *c == RemoteCmd::Cleanup),
        "no command ran on the server"
    );
}

#[test]
fn plain_serve_install_is_refused_by_add_and_migrated_by_repair() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, local, mut ui) = (
        Fake::new(),
        Local::default(),
        TestUi {
            accept: true,
            asked: 0,
            cancel_after_pair: None,
        },
    );
    *fake.preflight.borrow_mut() = installed_facts().replace("unit_hostmode=1", "unit_hostmode=0");
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert_eq!(status_of(&r, "install"), Status::Fail);
    assert!(!fake.ran(RemoteCmd::Provision(Sub::Install)));
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Repair);
    assert!(
        fake.ran(RemoteCmd::Provision(Sub::Snapshot)),
        "data is snapshotted first"
    );
    assert_eq!(status_of(&r, "install.run"), Status::Pass, "{}", r.table());
}

#[test]
fn resume_after_a_failed_step_skips_what_is_already_done() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, local, mut ui) = (
        Fake::new(),
        Local::default(),
        TestUi {
            accept: true,
            asked: 0,
            cancel_after_pair: None,
        },
    );
    fake.fail.borrow_mut().insert("install", 1);
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert_eq!(status_of(&r, "install.run"), Status::Fail);
    assert!(
        r.results.iter().all(|s| s.id != "pair"),
        "pairing waits for a good install"
    );

    fake.fail.borrow_mut().clear();
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert!(r.ok(), "{}", r.table());
    assert_eq!(
        ui.asked, 1,
        "the host key was pinned once and trusted afterwards"
    );

    // the install now exists on the server: a third run installs and pairs nothing again
    *fake.preflight.borrow_mut() = installed_facts();
    let installs = fake
        .calls
        .borrow()
        .iter()
        .filter(|(c, _)| *c == RemoteCmd::Provision(Sub::Install))
        .count();
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Add);
    assert_eq!(status_of(&r, "install"), Status::Skip);
    assert_eq!(status_of(&r, "pair"), Status::Skip);
    let again = fake
        .calls
        .borrow()
        .iter()
        .filter(|(c, _)| *c == RemoteCmd::Provision(Sub::Install))
        .count();
    assert_eq!(installs, again);
}

#[test]
fn an_installed_but_unpaired_host_can_be_uninstalled_and_purged() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::new();
    let store = Store::new(dir.path().join("hosts"));
    let kh = KnownHosts::new(store.known_hosts_path());
    kh.pin("203.0.113.7", 22, &fake.keys[0]).unwrap();
    for (level, sub) in [
        (RemoveLevel::Uninstall, Sub::Uninstall),
        (RemoveLevel::Purge, Sub::Purge),
    ] {
        store.save(&record()).unwrap(); // installed earlier, pairing never finished: no desktop_key
        fake.calls.borrow_mut().clear();
        let (local, mut ui) = (Local::default(), TestUi::default());
        let mut engine = Engine {
            transport: &fake,
            local: &local,
            ui: &mut ui,
            store: &store,
            known_hosts: &kh,
            params: Params {
                record: record(),
                mode: Mode::Repair,
                app_version: "0.1.0".into(),
                release_tag: None,
                desktop: true,
                retry_delay: Duration::ZERO,
                allow_install: true,
            },
        };
        let r = engine.remove(level);
        assert!(r.ok(), "{}", r.table());
        let calls = fake.calls.borrow();
        let helper = calls
            .iter()
            .position(|(c, _)| *c == RemoteCmd::Stage(StageFile::Provision))
            .expect("helper staged");
        let run = calls
            .iter()
            .position(|(c, _)| *c == RemoteCmd::Provision(sub))
            .expect("uninstall ran");
        assert!(helper < run, "the helper is uploaded before it is executed");
        drop(calls);
        kh.pin("203.0.113.7", 22, &fake.keys[0]).unwrap(); // remove forgets the pin
    }
}

#[test]
fn already_installed_host_is_only_paired_when_install_is_not_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, local, mut ui) = (
        Fake::new(),
        Local::default(),
        TestUi {
            accept: true,
            asked: 0,
            cancel_after_pair: None,
        },
    );
    *fake.preflight.borrow_mut() = installed_facts();
    let r = run_with(dir.path(), &fake, &local, &mut ui, Mode::Add, false);
    assert!(r.ok(), "{}", r.table());
    assert_eq!(status_of(&r, "install"), Status::Skip);
    assert_eq!(status_of(&r, "pair"), Status::Pass);
    assert!(
        !fake.ran(RemoteCmd::Provision(Sub::Install))
            && !fake.ran(RemoteCmd::Stage(StageFile::Installer)),
        "nothing is downloaded or installed"
    );

    // A fresh server is refused plainly, before anything is staged.
    let dir = tempfile::tempdir().unwrap();
    let (fake, local) = (Fake::new(), Local::default());
    let r = run_with(dir.path(), &fake, &local, &mut ui, Mode::Add, false);
    assert!(!r.ok());
    assert_eq!(status_of(&r, "install"), Status::Fail);
    assert!(r.results.iter().all(|s| s.id != "pair"));
    assert!(!fake.ran(RemoteCmd::Provision(Sub::Install)));
}

#[test]
fn a_failure_after_pairing_rolls_back_the_peers_this_run_created() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, local) = (Fake::new(), Local::default());
    let mut ui = TestUi {
        accept: true,
        ..TestUi::default()
    };
    *fake.preflight.borrow_mut() = installed_facts();
    // The screen grant fails after the invite was consumed and the key exists.
    fake.fail.borrow_mut().insert("screen", 1);
    let r = run_with(dir.path(), &fake, &local, &mut ui, Mode::Add, false);
    assert_eq!(status_of(&r, "pair"), Status::Fail);
    assert_eq!(*local.forgotten.borrow(), ["hostid1"], "local peer removed");
    let calls = fake.calls.borrow();
    let revoke = calls
        .iter()
        .find(|(c, _)| *c == RemoteCmd::Provision(Sub::Revoke))
        .expect("server entry revoked");
    assert_eq!(
        revoke.1,
        "key=deskkey1
"
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == RemoteCmd::Provision(Sub::ScreenRevoke)),
        "screen and input access taken back"
    );
    let hint = r
        .results
        .iter()
        .find(|s| s.id == "pair")
        .unwrap()
        .remediation
        .clone()
        .unwrap();
    assert!(hint.contains("removed from the server"), "{hint}");
    drop(calls);

    // Cancelled before the invite: nothing is created at all.
    let dir = tempfile::tempdir().unwrap();
    let (fake, local) = (Fake::new(), Local::default());
    *fake.preflight.borrow_mut() = installed_facts();
    let flag = std::rc::Rc::new(std::cell::Cell::new(true));
    let mut ui = TestUi {
        accept: true,
        cancel_after_pair: Some(flag),
        ..TestUi::default()
    };
    let _ = run_with(dir.path(), &fake, &local, &mut ui, Mode::Add, false);
    assert!(!fake.ran(RemoteCmd::Provision(Sub::Invite)));
    assert!(local.forgotten.borrow().is_empty() && !*local.paired.borrow());
}

#[test]
fn an_edited_saved_record_is_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("hosts"));
    store.save(&record()).unwrap();
    assert!(store.load("oracle").is_some());
    let mut evil = record();
    evil.host = "-oProxyCommand=calc".into();
    let text = serde_json::to_string(&evil).unwrap();
    std::fs::write(dir.path().join("hosts/oracle/host.json"), text).unwrap();
    assert!(store.load("oracle").is_none(), "hostile host field refused");
}

#[test]
fn a_host_already_paired_with_this_computer_is_confirmed_not_paired_twice() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::new();
    fake.desktops.borrow_mut().push("deskkey1".into()); // our key is already on the host
    *fake.preflight.borrow_mut() = installed_facts();
    let local = Local {
        preexisting: true,
        paired: RefCell::new(true),
        ..Local::default()
    };
    let mut ui = TestUi {
        accept: true,
        ..TestUi::default()
    };
    let r = run_with(dir.path(), &fake, &local, &mut ui, Mode::Add, false);
    assert_eq!(status_of(&r, "pair"), Status::Pass, "{}", r.table());
    assert!(
        r.results
            .iter()
            .find(|s| s.id == "pair")
            .unwrap()
            .detail
            .contains("already connected")
    );
    assert!(local.forgotten.borrow().is_empty(), "nothing is removed");
    assert!(
        !fake.ran(RemoteCmd::Provision(Sub::Screen)),
        "existing privileges are left alone"
    );
}

fn old_server_facts() -> String {
    installed_facts().replace("praxi_version=0.1.0", "praxi_version=0.0.9")
}

fn upgrade_ui() -> TestUi {
    TestUi {
        accept: true,
        asked: 0,
        cancel_after_pair: None,
    }
}

/// A paired server that runs an older version, as the wizard stored it.
fn paired_record(dir: &std::path::Path) -> Local {
    let store = Store::new(dir.join("hosts"));
    let mut rec = record();
    rec.paired_host_id = Some("hostid1".into());
    rec.desktop_key = Some("deskkey1".into());
    store.save(&rec).unwrap();
    Local {
        preexisting: true,
        ..Local::default()
    }
}

#[test]
fn upgrade_updates_in_place_and_checks_health() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, mut ui) = (Fake::new(), upgrade_ui());
    let local = paired_record(dir.path());
    fake.desktops.borrow_mut().push("deskkey1".into());
    *fake.preflight.borrow_mut() = old_server_facts();
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Upgrade);
    assert!(r.ok(), "{}", r.table());
    assert_eq!(status_of(&r, "install.run"), Status::Pass);
    assert!(local.forgotten.borrow().is_empty(), "nothing is un-paired");
    assert_eq!(status_of(&r, "health.version"), Status::Pass);
    let calls = fake.calls.borrow();
    let install = calls
        .iter()
        .find(|(c, _)| *c == RemoteCmd::Provision(Sub::Install))
        .unwrap();
    assert!(
        install.1.contains(
            "update=1
"
        ),
        "{}",
        install.1
    );
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == RemoteCmd::Provision(Sub::Snapshot)),
        "data is snapshotted before the update"
    );
}

#[test]
fn upgrade_stops_on_a_failed_verification_and_never_pairs_or_reports_health() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, mut ui) = (Fake::new(), upgrade_ui());
    let local = paired_record(dir.path());
    *fake.preflight.borrow_mut() = old_server_facts();
    fake.fail.borrow_mut().insert("install", 1);
    *fake.fail_stderr.borrow_mut() = "attestation verification failed: digest mismatch
[praxi-install] rollback complete; previously active services restarted and active"
        .into();
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Upgrade);
    assert!(!r.ok());
    assert_eq!(status_of(&r, "install.run"), Status::Fail);
    let detail = &r
        .results
        .iter()
        .find(|s| s.id == "install.run")
        .unwrap()
        .detail;
    assert!(detail.contains("rollback complete"), "{detail}");
    assert!(!r.results.iter().any(|s| s.id.starts_with("health")));
}

#[test]
fn upgrade_of_an_unreachable_server_fails_at_connect() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, mut ui) = (Fake::new(), upgrade_ui());
    let local = paired_record(dir.path());
    fake.fail.borrow_mut().insert("", 255);
    let r = run(dir.path(), &fake, &local, &mut ui, Mode::Upgrade);
    assert!(!r.ok());
    assert!(!fake.ran(RemoteCmd::Provision(Sub::Install)));
}

fn tagged_server(tag: &str) -> String {
    installed_facts().replace(
        "unit_present=1",
        &format!(
            "praxi_build={tag}
unit_present=1"
        ),
    )
}

fn run_upgrade_with_tag(server: String, app_tag: &str) -> (Report, Fake) {
    let dir = tempfile::tempdir().unwrap();
    let (fake, mut ui) = (Fake::new(), upgrade_ui());
    let local = paired_record(dir.path());
    fake.desktops.borrow_mut().push("deskkey1".into());
    *fake.preflight.borrow_mut() = server;
    let r = run_full(
        dir.path(),
        &fake,
        &local,
        &mut ui,
        Mode::Upgrade,
        true,
        Some(app_tag),
    );
    (r, fake)
}

#[test]
fn upgrade_installs_when_only_the_alpha_differs() {
    // Same cargo version 0.1.0, server alpha.3, app alpha.4: the install must run.
    let (r, fake) = run_upgrade_with_tag(tagged_server("v0.1.0-alpha.3"), "v0.1.0-alpha.4");
    assert_eq!(status_of(&r, "install.run"), Status::Pass, "{}", r.table());
    let calls = fake.calls.borrow();
    let install = calls
        .iter()
        .find(|(c, _)| *c == RemoteCmd::Provision(Sub::Install))
        .unwrap();
    assert!(
        install.1.contains("version=v0.1.0-alpha.4"),
        "{}",
        install.1
    );
}

#[test]
fn upgrade_installs_when_the_server_reports_no_build() {
    let (r, fake) = run_upgrade_with_tag(installed_facts(), "v0.1.0-alpha.4");
    assert_eq!(status_of(&r, "install.run"), Status::Pass, "{}", r.table());
    assert!(fake.ran(RemoteCmd::Provision(Sub::Install)));
}

#[test]
fn upgrade_skips_the_install_on_exactly_the_same_tag() {
    let (r, fake) = run_upgrade_with_tag(tagged_server("v0.1.0-alpha.4"), "v0.1.0-alpha.4");
    assert_eq!(status_of(&r, "install"), Status::Skip, "{}", r.table());
    assert!(!fake.ran(RemoteCmd::Provision(Sub::Install)));
}

#[test]
fn upgrade_refuses_a_server_with_a_newer_tag() {
    let (r, fake) = run_upgrade_with_tag(tagged_server("v0.1.0-alpha.5"), "v0.1.0-alpha.4");
    assert_eq!(status_of(&r, "install"), Status::Warn, "{}", r.table());
    assert!(!fake.ran(RemoteCmd::Provision(Sub::Install)));
}
