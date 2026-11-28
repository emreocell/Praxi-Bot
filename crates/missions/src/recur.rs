//! Recurring mission waits: next fire time from an RFC 5545 `RRULE`.

use connector_calendar::rrule;

/// Longest rule the bot may register.
pub const MAX_RRULE_LEN: usize = 400;

/// The next instant strictly after `after_ms` for this series.
///
/// # Errors
/// Invalid rule, zone, or timestamps.
pub fn next_due(
    rrule: &str,
    dtstart_ms: i64,
    timezone: Option<&str>,
    after_ms: i64,
) -> Result<Option<i64>, String> {
    if rrule.trim().is_empty() || rrule.len() > MAX_RRULE_LEN {
        return Err(format!(
            "rrule must be plain text of at most {MAX_RRULE_LEN} characters"
        ));
    }
    rrule::next_after_ms(rrule.trim(), dtstart_ms, timezone, after_ms)
}
