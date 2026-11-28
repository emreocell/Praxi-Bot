//! Daily, cheap session health probes for:
//! - site-ledger accounts (browser cookies + optional evidence URL),
//! - configured mail identities (IMAP/SMTP login),
//! - connected Composio apps (authorization refresh via the host).
//!
//! Results are written under `<data dir>/account-health/report.json` and read
//! by `bot doctor` and `site_ledger` `check`.

mod doctor;
mod http_probe;
mod ledger;
mod report;
mod runner;

pub use doctor::{DoctorStatus, check_account_health};
pub use ledger::{LedgerTarget, scan_ledger_targets};
pub use report::{
    ItemKind, ItemStatus, ProbeItem, StoredReport, due as report_due, load_report, save_report,
};
pub use runner::{
    ComposioConnState, ComposioProbe, Deps, MailProbe, RunOutcome, daily_loop, run_once,
};
