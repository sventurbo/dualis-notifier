//! Timestamped log lines.
//!
//! Under cron the output is appended to `notifier.log`, where a bare level
//! prefix leaves no clue *when* something happened. Every line therefore gets
//! an RFC 3339 local timestamp. Informational lines go to stdout, errors to
//! stderr, so `2>&1` redirection keeps the previous behaviour.

use chrono::{Local, SecondsFormat};

pub fn info(message: &str) {
    println!("{} I: {message}", timestamp());
}

pub fn warn(message: &str) {
    println!("{} W: {message}", timestamp());
}

pub fn error(message: &str) {
    eprintln!("{} E: {message}", timestamp());
}

fn timestamp() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}
