use std::process::ExitCode;
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

    println!("I: Checking every {} min", interval.as_secs() / 60);
    loop {
        if let Err(error) = notifier.run_once() {
            eprintln!("E: {error:#}");
        }
        thread::sleep(interval);
    }
}
