use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::process::ExitCode;
use std::time::Duration;
use std::{env, thread};

use anyhow::{Context, Result};
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
    loop {
        if let Err(error) = notifier.run_once() {
            eprintln!("E: {error:#}");
        }
        thread::sleep(jittered(interval));
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
}
