//! Checks Dualis for new or changed grades and reports them through Gotify.

#![forbid(unsafe_code)]

pub mod config;
pub mod dualis;
pub mod gotify;
pub mod log;
pub mod parse;
pub mod state;
pub mod table;

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};

use crate::config::Config;
use crate::dualis::DualisClient;
use crate::gotify::{GotifyClient, Notification};

pub const CACHE_FILE: &str = "grades.csv";
/// The last downloaded results page, kept for debugging.
pub const RAW_HTML_FILE: &str = "grades.html";

pub struct Notifier {
    config: Config,
    dualis: DualisClient,
    gotify: GotifyClient,
}

impl Notifier {
    pub fn new(config: Config) -> Result<Self> {
        fs::create_dir_all(&config.data_dir).with_context(|| {
            format!(
                "DATA_DIR {} kann nicht angelegt werden",
                config.data_dir.display()
            )
        })?;
        let dualis = DualisClient::new(&config.dualis_base_url, &config.user_agent)?;
        let gotify = GotifyClient::new(
            &config.gotify_url,
            &config.gotify_token,
            config.gotify_priority,
        )?;
        Ok(Self {
            config,
            dualis,
            gotify,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn cache_file(&self) -> PathBuf {
        self.config.data_dir.join(CACHE_FILE)
    }

    pub fn send_test_notification(&self) -> Result<()> {
        self.gotify.send(&Notification::test())
    }

    /// Run one check: fetch grades, compare with the cache, notify, save.
    pub fn run_once(&self) -> Result<()> {
        let session = self.dualis.login(&self.config.user, &self.config.passwd)?;
        let raw_html = self
            .dualis
            .fetch_results(&session, &self.config.semester_id)?;
        let raw_html_file = self.config.data_dir.join(RAW_HTML_FILE);
        fs::write(&raw_html_file, &raw_html).with_context(|| {
            format!("{} kann nicht geschrieben werden", raw_html_file.display())
        })?;

        let current = parse::extract_table(&raw_html)?;
        let observed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false);
        let cache_file = self.cache_file();

        let previous = if cache_file.exists() {
            Some(state::load_cache(&cache_file)?).filter(|cache| !cache.columns.is_empty())
        } else {
            None
        };
        let Some(previous) = previous else {
            // First run: record what is there without announcing it.
            state::save_cache(&cache_file, &state::baseline(&current, &observed_at)?)?;
            log::warn("No cache found");
            log::info("Created cache");
            return Ok(());
        };

        let (new_state, events) = state::reconcile(&previous, &current, &observed_at)?;
        if events.is_empty() {
            log::info("No changes");
        } else {
            for event in &events {
                log::info(&format!("{} grade for {}", event.kind.label(), event.name));
            }
            // One combined message per run: a single send is atomic, so a
            // failure retries the whole batch instead of re-sending the grades
            // that already went out. This avoids duplicate notifications when
            // several modules change at once.
            self.gotify.send(&Notification::for_events(&events))?;
        }
        // Only now: if the notification failed, the next run detects it again.
        state::save_cache(&cache_file, &new_state)
    }
}
