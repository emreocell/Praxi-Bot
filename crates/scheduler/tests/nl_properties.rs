//! Parser properties use fixed anchors, never the wall clock for assertions.
//! Proptest defaults to 256 cases; `PROPTEST_CASES` raises that and
//! `PROPTEST_RNG_SEED` replays a run. Keep generated strings bounded for CI.

use proptest::prelude::*;
use scheduler::{ScheduleSpec, parse_natural_language, parse_natural_language_with_tz, parse_nl};

const NOW: i64 = 1_700_000_000_123;

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..512).prop_map(|chars| chars.into_iter().collect())
}

fn quantity() -> impl Strategy<Value = u64> {
    prop_oneof![
        0..10_000_u64,
        any::<u64>(),
        proptest::sample::select(vec![0, 1, u64::MAX, 9_223_372_036_854_775]),
    ]
}

proptest! {
    /// All NL entry points tolerate arbitrary UTF-8, timestamps, and timezone text.
    #[test]
    fn arbitrary_text_does_not_panic(phrase in text(), now in any::<i64>(), tz in text()) {
        let _ = parse_natural_language(&phrase, now);
        let _ = parse_natural_language_with_tz(&phrase, now, Some(&tz));
        let _ = parse_nl(&phrase);
    }

    /// Accepted schedules yield no timestamp before a fixed, representable anchor.
    #[test]
    fn accepted_schedules_do_not_run_in_the_past(
        phrase in prop_oneof![
            text(),
            quantity().prop_map(|n| format!("every {n} seconds")),
            quantity().prop_map(|n| format!("in {n} minutes")),
            (0..24_u8, 0..60_u8).prop_map(|(h, m)| format!("her gün {h:02}:{m:02}")),
        ],
        now in 946_684_800_000_i64..4_102_444_800_000,
        tz in proptest::sample::select(vec![None, Some("UTC"), Some("Europe/Istanbul"), Some("America/New_York")]),
    ) {
        if let Some(spec) = parse_natural_language_with_tz(&phrase, now, tz) {
            prop_assert!(spec.validate().is_ok());
            // A valid spec need not have a future occurrence. Do not unwrap None.
            if let Some(next) = spec.next_after(now).unwrap() {
                prop_assert!(next > now, "{phrase:?}: {spec:?} gave {next} after {now}");
            }
        }
    }

    /// Numeric EN/TR intervals reject zero and multiplication overflow consistently.
    #[test]
    fn interval_quantities_are_checked(
        count in quantity(),
        (en, tr, seconds) in proptest::sample::select(vec![
            ("seconds", "saniyede", 1_u64),
            ("minutes", "dakikada", 60),
            ("hours", "saatte", 3_600),
            ("days", "günde", 86_400),
            ("weeks", "haftada", 604_800),
        ]),
    ) {
        let expected = count.checked_mul(seconds).filter(|&n| n > 0)
            .map(|seconds| ScheduleSpec::Every { seconds });
        prop_assert_eq!(parse_natural_language(&format!("every {count} {en}"), NOW), expected.clone());
        prop_assert_eq!(parse_natural_language(&format!("her {count} {tr} bir"), NOW), expected);
    }

    /// Relative one-offs saturate safely and have no occurrence at or after their due time.
    #[test]
    fn relative_one_offs_can_be_exhausted(count in quantity(), now in any::<i64>()) {
        let expected = count.checked_mul(60)
            .filter(|&n| n > 0)
            .and_then(|secs| i64::try_from(secs).ok())
            .and_then(|secs| secs.checked_mul(1_000))
            .map(|delta| ScheduleSpec::At { unix_ms: now.saturating_add(delta) });
        let actual = parse_natural_language(&format!("in {count} minutes"), now);
        prop_assert_eq!(&actual, &expected);
        if let Some(spec @ ScheduleSpec::At { unix_ms }) = actual {
            prop_assert!(unix_ms >= now);
            prop_assert_eq!(spec.next_after(unix_ms).unwrap(), None);
            prop_assert_eq!(spec.next_after(i64::MAX).unwrap(), None);
        }
    }

    /// Calendar phrases accept only valid 24-hour times and normalize ASCII case/whitespace.
    #[test]
    fn calendar_time_boundaries(hour in 0..32_u8, minute in 0..72_u8) {
        let phrase = format!("every day at {hour:02}:{minute:02}");
        let expected = (hour < 24 && minute < 60)
            .then(|| ScheduleSpec::Cron(format!("{minute} {hour} * * *")));
        prop_assert_eq!(parse_natural_language(&phrase, NOW), expected.clone());
        let varied = format!("\tEVERY  DAY\nAT {hour:02}:{minute:02}\u{2003}");
        prop_assert_eq!(parse_natural_language(&varied, NOW), expected);
    }
}

/// Empty, long, non-ASCII, ambiguous, and malformed phrases never panic.
#[test]
fn adversarial_examples() {
    for phrase in [
        String::new(),
        "\u{2003}\0💥İı東京".into(),
        "yarın sabah".into(),
        "every monday at 12am pm".into(),
        "every day at 24:60".into(),
        "in 18446744073709551616 seconds".into(),
        "her gün ".repeat(8_192),
    ] {
        let _ = parse_natural_language(&phrase, NOW);
        let _ = parse_natural_language_with_tz(&phrase, NOW, Some("invalid/timezone"));
        let _ = parse_nl(&phrase);
    }
    // Timezones are attached by the parser, validated by ScheduleSpec later.
    let invalid = parse_natural_language_with_tz("daily", NOW, Some("invalid/timezone")).unwrap();
    assert!(invalid.validate().is_err());
    assert!(invalid.next_after(NOW).is_err());
    assert_eq!(
        parse_natural_language("in 1 second", i64::MAX)
            .unwrap()
            .next_after(i64::MAX)
            .unwrap(),
        None
    );
}
