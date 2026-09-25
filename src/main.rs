use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::process::ExitCode;
use std::time::Duration;
use std::{env, thread};

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use dualis_notifier::Notifier;
use dualis_notifier::config::Config;

const USAGE: &str = "\
Usage: dualis-notifier [--test-notification]

Checks Dualis for new or changed grades and sends them to Gotify.
Runs once, or every CHECK_INTERVAL_MINUTES minutes if that is set.

Options:
  --test-notification  Send a test message to Gotify and exit
  -h, --help           Show this help";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let test_notification = match args.as_slice() {
        [] => false,
        [flag] if flag == "--test-notification" => true,
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("E: Unknown arguments: {}\n\n{USAGE}", args.join(" "));
            return ExitCode::from(2);
        }
    };

    match run(test_notification) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("E: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(test_notification: bool) -> Result<()> {
    let notifier = Notifier::new(Config::from_env()?)?;

    if test_notification {
        notifier.send_test_notification()?;
        println!("I: Test notification sent");
        return Ok(());
    }

    let Some(interval) = notifier.config().check_interval else {
        return notifier.run_once();
    };

    // Exit promptly on `docker stop`; the cache is written atomically.
    ctrlc::set_handler(|| {
        println!("I: Stopping notifier");
        std::process::exit(0);
    })
    .context("Signal-Handler kann nicht eingerichtet werden")?;

    println!("I: Checking every {} min (±20%)", interval.as_secs() / 60);
    let config = notifier.config();
    let (window, days, timezone) = (config.check_window, config.check_days, config.timezone);
    if let Some((start, end)) = window {
        println!(
            "I: Only checking between {} and {} ({timezone})",
            start.format("%H:%M"),
            end.format("%H:%M")
        );
    }
    if let Some((first, last)) = days {
        println!("I: Only checking {first}-{last} ({timezone})");
    }
    loop {
        let now = Utc::now();
        if in_check_window(window, timezone, now) && on_check_day(days, timezone, now) {
            if let Err(error) = notifier.run_once() {
                eprintln!("E: {error:#}");
            }
        } else {
            println!("I: Outside check window, skipping");
        }
        thread::sleep(jittered(interval));
    }
}

/// Whether `now` (interpreted in `timezone`) falls inside `window`. No
/// window means always. `start > end` wraps past midnight.
fn in_check_window(
    window: Option<(NaiveTime, NaiveTime)>,
    timezone: Tz,
    now: chrono::DateTime<Utc>,
) -> bool {
    let Some((start, end)) = window else {
        return true;
    };
    let local = now.with_timezone(&timezone).time();
    if start <= end {
        local >= start && local < end
    } else {
        local >= start || local < end
    }
}

/// Whether `now` (interpreted in `timezone`) falls on one of `days`, both
/// ends included. No days means every day. `first > last` wraps past Sunday.
fn on_check_day(
    days: Option<(Weekday, Weekday)>,
    timezone: Tz,
    now: chrono::DateTime<Utc>,
) -> bool {
    let Some((first, last)) = days else {
        return true;
    };
    let day = now
        .with_timezone(&timezone)
        .weekday()
        .num_days_from_monday();
    let (first, last) = (first.num_days_from_monday(), last.num_days_from_monday());
    if first <= last {
        day >= first && day <= last
    } else {
        day >= first || day <= last
    }
}

/// Randomizes `base` by up to ±20%, so checks don't land at a perfectly
/// predictable cadence, which is easy for Dualis to fingerprint as a bot.
fn jittered(base: Duration) -> Duration {
    let spread = base / 5;
    base - spread + random_duration_up_to(spread * 2)
}

/// A random duration in `[0, max]`. Seeded from `RandomState`'s per-process
/// random keys (the same source `HashMap` uses), so no extra dependency is
/// needed just for jitter.
fn random_duration_up_to(max: Duration) -> Duration {
    if max.is_zero() {
        return Duration::ZERO;
    }
    let random = RandomState::new().build_hasher().finish();
    Duration::from_nanos(random % (max.as_nanos() as u64 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let base = Duration::from_secs(15 * 60);
        let lower = base - base / 5;
        let upper = base + base / 5;

        for _ in 0..1000 {
            let jittered = jittered(base);
            assert!(
                jittered >= lower && jittered <= upper,
                "{jittered:?} outside [{lower:?}, {upper:?}]"
            );
        }
    }

    #[test]
    fn random_duration_up_to_zero_is_zero() {
        assert_eq!(random_duration_up_to(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn random_duration_up_to_respects_bound() {
        let max = Duration::from_secs(60);
        for _ in 0..1000 {
            assert!(random_duration_up_to(max) <= max);
        }
    }

    fn berlin_at(hour: u32, minute: u32) -> chrono::DateTime<Utc> {
        berlin_on(15, hour, minute)
    }

    /// A day in July 2026: the 15th is a Wednesday, the 17th a Friday, the
    /// 18th a Saturday, the 19th a Sunday and the 20th a Monday.
    fn berlin_on(day: u32, hour: u32, minute: u32) -> chrono::DateTime<Utc> {
        use chrono::TimeZone;
        // A fixed summer date (CEST, UTC+2) so the offset is predictable.
        Tz::Europe__Berlin
            .with_ymd_and_hms(2026, 7, day, hour, minute, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn no_window_always_checks() {
        assert!(in_check_window(None, Tz::UTC, berlin_at(3, 0)));
    }

    #[test]
    fn daytime_window_excludes_night() {
        let window = Some((
            NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
            NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
        ));

        assert!(in_check_window(window, Tz::Europe__Berlin, berlin_at(6, 0)));
        assert!(in_check_window(
            window,
            Tz::Europe__Berlin,
            berlin_at(17, 59)
        ));
        assert!(!in_check_window(
            window,
            Tz::Europe__Berlin,
            berlin_at(18, 0)
        ));
        assert!(!in_check_window(
            window,
            Tz::Europe__Berlin,
            berlin_at(3, 0)
        ));
    }

    #[test]
    fn overnight_window_wraps_midnight() {
        let window = Some((
            NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
        ));

        assert!(in_check_window(
            window,
            Tz::Europe__Berlin,
            berlin_at(23, 0)
        ));
        assert!(in_check_window(window, Tz::Europe__Berlin, berlin_at(1, 0)));
        assert!(!in_check_window(
            window,
            Tz::Europe__Berlin,
            berlin_at(12, 0)
        ));
    }

    #[test]
    fn no_days_always_checks() {
        assert!(on_check_day(None, Tz::UTC, berlin_on(19, 12, 0)));
    }

    #[test]
    fn weekdays_exclude_weekend() {
        let days = Some((Weekday::Mon, Weekday::Fri));

        assert!(on_check_day(days, Tz::Europe__Berlin, berlin_on(17, 12, 0)));
        assert!(on_check_day(days, Tz::Europe__Berlin, berlin_on(20, 12, 0)));
        assert!(!on_check_day(
            days,
            Tz::Europe__Berlin,
            berlin_on(18, 12, 0)
        ));
        assert!(!on_check_day(
            days,
            Tz::Europe__Berlin,
            berlin_on(19, 12, 0)
        ));
    }

    #[test]
    fn day_range_wraps_past_sunday() {
        let days = Some((Weekday::Fri, Weekday::Mon));

        assert!(on_check_day(days, Tz::Europe__Berlin, berlin_on(18, 12, 0)));
        assert!(on_check_day(days, Tz::Europe__Berlin, berlin_on(20, 12, 0)));
        assert!(!on_check_day(
            days,
            Tz::Europe__Berlin,
            berlin_on(15, 12, 0)
        ));
    }

    #[test]
    fn weekday_uses_configured_timezone() {
        let days = Some((Weekday::Mon, Weekday::Fri));
        // Saturday 00:30 in Berlin is still Friday 22:30 in UTC.
        let saturday_night = berlin_on(18, 0, 30);

        assert!(!on_check_day(days, Tz::Europe__Berlin, saturday_night));
        assert!(on_check_day(days, Tz::UTC, saturday_night));
    }
}
