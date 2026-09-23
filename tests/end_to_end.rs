//! Full runs against mock Dualis and Gotify servers.

use std::fs;
use std::path::Path;

use dualis_notifier::config::Config;
use dualis_notifier::{CACHE_FILE, Notifier, RAW_HTML_FILE};
use mockito::{Matcher, Mock, Server, ServerGuard};

const RESULTS: &str = include_str!("fixtures/courseresults.html");
const RESULTS_CHANGED: &str = include_str!("fixtures/courseresults_changed.html");
const RESULTS_WITHOUT_MATHS: &str = "<table><thead><tr><th>Nr.</th><th>Name</th>\
    <th>Endnote</th><th>Credits</th><th>Status</th><th></th></tr></thead><tbody>\
    <tr><td>T3INF1002</td><td>Theoretische Informatik I</td><td>2,3</td>\
    <td>5,0</td><td></td><td>Prüfungen</td></tr>\
    <tr><td>T3INF1003</td><td>Einführung in die Öffentlichkeitsarbeit – Grundlagen</td>\
    <td>2,0</td><td>5,0</td><td>bestanden</td><td>Prüfungen</td></tr></tbody></table>";

struct Harness {
    dualis: ServerGuard,
    gotify: ServerGuard,
    data_dir: tempfile::TempDir,
    notifier: Notifier,
}

impl Harness {
    fn new() -> Self {
        let mut dualis = Server::new();
        let gotify = Server::new();
        let data_dir = tempfile::tempdir().unwrap();

        dualis
            .mock("POST", "/scripts/mgrqispi.dll")
            .with_header("Set-Cookie", "cnsc=ABC123; path=/")
            .with_header(
                "REFRESH",
                "0; URL=/scripts/mgrqispi.dll?ARGUMENTS=-N42,-N000019,",
            )
            .create();

        let mut config = Config::from_lookup(|key| {
            let value = match key {
                "DUALIS_USER" => "s123456",
                "DUALIS_PASSWD" => "secret",
                "GOTIFY_URL" => return Some(gotify.url()),
                "GOTIFY_TOKEN" => "AbCdEf",
                "DATA_DIR" => return Some(data_dir.path().display().to_string()),
                _ => return None,
            };
            Some(value.to_owned())
        })
        .unwrap();
        config.dualis_base_url = dualis.url();
        let notifier = Notifier::new(config).unwrap();

        Self {
            dualis,
            gotify,
            data_dir,
            notifier,
        }
    }

    /// Serve `html` as the results page for the next run.
    fn serve_results(&mut self, html: &str) -> Mock {
        self.dualis
            .mock("GET", "/scripts/mgrqispi.dll")
            .match_query(Matcher::UrlEncoded(
                "ARGUMENTS".into(),
                "-N42,-N000307,".into(),
            ))
            .match_header("cookie", "cnsc=ABC123")
            .with_header("Content-Type", "text/html; charset=utf-8")
            .with_body(html)
            .create()
    }

    /// Expect one Gotify message per title (checked when there is exactly one).
    fn expect_messages(&mut self, titles: &[&str]) -> Mock {
        let mock = self
            .gotify
            .mock("POST", "/message")
            .match_header("x-gotify-key", "AbCdEf");
        let mock = match titles {
            [title] => mock.match_body(Matcher::PartialJson(serde_json::json!({ "title": title }))),
            _ => mock,
        };
        mock.expect(titles.len()).create()
    }

    fn run(&mut self, html: &str, titles: &[&str]) {
        let results = self.serve_results(html);
        let messages = self.expect_messages(titles);

        self.notifier.run_once().unwrap();

        messages.assert();
        results.remove();
        messages.remove();
    }

    fn cache(&self) -> String {
        fs::read_to_string(self.data_dir.path().join(CACHE_FILE)).unwrap()
    }
}

#[test]
fn full_notification_cycle() {
    let mut harness = Harness::new();

    // First run: silent baseline.
    harness.run(RESULTS, &[]);
    assert!(harness.cache().contains("T3INF1001"));
    assert!(harness.data_dir.path().join(RAW_HTML_FILE).exists());

    // A grade for one module appears.
    harness.run(
        RESULTS_CHANGED,
        &["Note geändert: Theoretische Informatik I"],
    );

    // Nothing changed.
    harness.run(RESULTS_CHANGED, &[]);

    // Dualis hides a module and shows it again later: no duplicate alert.
    harness.run(RESULTS_WITHOUT_MATHS, &[]);
    harness.run(RESULTS_CHANGED, &[]);
}

#[test]
fn failed_notification_keeps_cache_so_the_next_run_retries() {
    let mut harness = Harness::new();
    harness.run(RESULTS, &[]);
    let baseline = harness.cache();

    let results = harness.serve_results(RESULTS_CHANGED);
    let failing = harness
        .gotify
        .mock("POST", "/message")
        .with_status(500)
        .create();
    assert!(harness.notifier.run_once().is_err());
    assert_eq!(harness.cache(), baseline);
    results.remove();
    failing.remove();

    harness.run(
        RESULTS_CHANGED,
        &["Note geändert: Theoretische Informatik I"],
    );
}

#[test]
fn python_cache_carries_over_without_alerts() {
    let mut harness = Harness::new();
    let legacy =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy_grades_stateful.csv");
    fs::copy(legacy, harness.data_dir.path().join(CACHE_FILE)).unwrap();

    harness.run(RESULTS, &[]);

    // The cache now holds the original Dualis values instead of pandas' "17".
    assert!(harness.cache().contains("\"1,7\""), "{}", harness.cache());
}
