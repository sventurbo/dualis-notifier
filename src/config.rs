//! Configuration from environment variables and an optional `.env` file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;
use std::{env, fs, io};

use anyhow::{Context, Result, bail};
use regex::Regex;

pub const DEFAULT_DUALIS_URL: &str = "https://dualis.dhbw.de";
const DOTENV_FILE: &str = ".env";
const DEFAULT_AGENT_NAME: &str = "Dualis Notifier";
const DEFAULT_PRIORITY: u8 = 5;
const REQUIRED: [&str; 4] = ["DUALIS_USER", "DUALIS_PASSWD", "GOTIFY_URL", "GOTIFY_TOKEN"];

static INLINE_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+#.*").expect("valid regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub user: String,
    pub passwd: String,
    /// Empty to fetch every result Dualis shows without a semester filter.
    pub semester_id: String,
    pub user_agent: String,
    pub gotify_url: String,
    pub gotify_token: String,
    pub gotify_priority: u8,
    /// `None` runs a single check, e.g. from cron.
    pub check_interval: Option<Duration>,
    /// Where `grades.csv` and `grades.html` are kept.
    pub data_dir: PathBuf,
    /// Only changed by tests, which point it at a mock server.
    pub dualis_base_url: String,
}

impl Config {
    /// Load `.env` from the working directory, then read the environment.
    /// Variables that are already set take precedence over `.env`.
    pub fn from_env() -> Result<Self> {
        let dotenv = match fs::read_to_string(DOTENV_FILE) {
            Ok(content) => parse_dotenv(&content),
            Err(error) if error.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(error).context(".env kann nicht gelesen werden"),
        };
        Self::from_lookup(|key| env::var(key).ok().or_else(|| dotenv.get(key).cloned()))
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());

        let missing: Vec<&str> = REQUIRED
            .into_iter()
            .filter(|key| get(key).is_none())
            .collect();
        if !missing.is_empty() {
            let mut message = format!(
                "Fehlende Konfiguration: {}. Trage die Werte in .env ein (siehe .env.example).",
                missing.join(", ")
            );
            if get("DISCORD_WEBHOOK").is_some() {
                message.push_str(
                    " DISCORD_WEBHOOK wird nicht mehr unterstützt – Benachrichtigungen \
                     laufen jetzt über Gotify (GOTIFY_URL und GOTIFY_TOKEN).",
                );
            }
            bail!(message);
        }

        let gotify_priority = match get("GOTIFY_PRIORITY") {
            None => DEFAULT_PRIORITY,
            Some(value) => value
                .trim()
                .parse()
                .ok()
                .filter(|priority| *priority <= 10)
                .with_context(|| {
                    format!("GOTIFY_PRIORITY muss eine Zahl von 0 bis 10 sein, nicht {value:?}")
                })?,
        };
        let check_interval = match get("CHECK_INTERVAL_MINUTES") {
            None => None,
            Some(value) => {
                let minutes: u64 = value
                    .trim()
                    .parse()
                    .ok()
                    .filter(|minutes| *minutes > 0)
                    .with_context(|| {
                        format!(
                            "CHECK_INTERVAL_MINUTES muss eine positive ganze Zahl sein, \
                             nicht {value:?}"
                        )
                    })?;
                Some(Duration::from_secs(minutes * 60))
            }
        };
        let required = |key: &str| get(key).expect("checked above");

        Ok(Self {
            user: required("DUALIS_USER").trim().to_owned(),
            passwd: required("DUALIS_PASSWD"),
            semester_id: get("SEMESTER_ID").unwrap_or_default().trim().to_owned(),
            user_agent: get("AGENT_NAME").unwrap_or_else(|| DEFAULT_AGENT_NAME.to_owned()),
            gotify_url: required("GOTIFY_URL").trim().to_owned(),
            gotify_token: required("GOTIFY_TOKEN").trim().to_owned(),
            gotify_priority,
            check_interval,
            data_dir: get("DATA_DIR").map_or_else(|| PathBuf::from("."), PathBuf::from),
            dualis_base_url: DEFAULT_DUALIS_URL.to_owned(),
        })
    }
}

/// Parse `.env` like python-dotenv, which the Python version used, so existing
/// files keep working: unquoted values may contain spaces and ` #` starts a
/// comment; quoted values keep `#` and support backslash escapes.
fn parse_dotenv(content: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with([' ', '\t']))
            .map_or(line, str::trim_start);
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty() {
            values.insert(key.to_owned(), parse_dotenv_value(raw_value.trim_start()));
        }
    }
    values
}

fn parse_dotenv_value(raw: &str) -> String {
    let mut chars = raw.chars();
    let quote = match chars.next() {
        Some(quote @ ('"' | '\'')) => quote,
        _ => return INLINE_COMMENT.replace(raw, "").trim_end().to_owned(),
    };
    let escapable: &[char] = if quote == '"' {
        &['\\', '\'', '"', 'a', 'b', 'f', 'n', 'r', 't', 'v']
    } else {
        &['\\', '\'']
    };

    let mut value = String::new();
    while let Some(c) = chars.next() {
        if c == quote {
            return value;
        }
        match chars.clone().next() {
            Some(next) if c == '\\' && escapable.contains(&next) => {
                chars.next();
                value.push(match next {
                    'a' => '\u{07}',
                    'b' => '\u{08}',
                    'f' => '\u{0c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'v' => '\u{0b}',
                    other => other,
                });
            }
            _ => value.push(c),
        }
    }
    // Unterminated quote: keep the text as written.
    raw.to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn dotenv_parsing_matches_python_dotenv() {
        // Expected values were produced by python-dotenv 1.1.0 `dotenv_values`.
        let content = r#"# comment

AGENT_NAME=Dualis Notifier
EMPTY=
SPACED =   padded value
INLINE=value # comment
HASH=pa#ss
export EXPORTED=yes
SINGLE='a # b \' c'
DOUBLE="x \"y\" \\n z"
TOKEN=abc==
NOVALUE
QUOTE_TAIL="q" # c
DOLLAR=p$ss
"#;
        let expected = [
            ("AGENT_NAME", "Dualis Notifier"),
            ("EMPTY", ""),
            ("SPACED", "padded value"),
            ("INLINE", "value"),
            ("HASH", "pa#ss"),
            ("EXPORTED", "yes"),
            ("SINGLE", "a # b ' c"),
            ("DOUBLE", "x \"y\" \\n z"),
            ("TOKEN", "abc=="),
            ("QUOTE_TAIL", "q"),
            ("DOLLAR", "p$ss"),
        ];

        let values = parse_dotenv(content);

        assert_eq!(values.len(), expected.len(), "{values:?}");
        for (key, value) in expected {
            assert_eq!(values.get(key).map(String::as_str), Some(value), "{key}");
        }
    }

    fn config(pairs: &[(&str, &str)]) -> Result<Config> {
        let values: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        Config::from_lookup(|key| values.get(key).cloned())
    }

    const MINIMAL: [(&str, &str); 4] = [
        ("DUALIS_USER", "s123456"),
        ("DUALIS_PASSWD", "secret"),
        ("GOTIFY_URL", "https://gotify.example.com"),
        ("GOTIFY_TOKEN", "AbCdEf"),
    ];

    #[test]
    fn minimal_configuration_uses_defaults() {
        let config = config(&MINIMAL).unwrap();

        assert_eq!(config.semester_id, "");
        assert_eq!(config.user_agent, "Dualis Notifier");
        assert_eq!(config.gotify_priority, 5);
        assert_eq!(config.check_interval, None);
        assert_eq!(config.data_dir, PathBuf::from("."));
        assert_eq!(config.dualis_base_url, DEFAULT_DUALIS_URL);
    }

    #[test]
    fn lists_every_missing_variable() {
        let error = config(&[("DUALIS_USER", "s123456"), ("GOTIFY_TOKEN", " ")]).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("DUALIS_PASSWD, GOTIFY_URL, GOTIFY_TOKEN"),
            "{error}"
        );
    }

    #[test]
    fn hints_at_gotify_when_only_discord_is_configured() {
        let error = config(&[
            ("DUALIS_USER", "s123456"),
            ("DUALIS_PASSWD", "secret"),
            ("DISCORD_WEBHOOK", "https://discord.com/api/webhooks/1/x"),
        ])
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("DISCORD_WEBHOOK wird nicht mehr unterstützt")
        );
    }

    #[test]
    fn reads_optional_settings() {
        let mut pairs = MINIMAL.to_vec();
        pairs.extend([
            ("GOTIFY_PRIORITY", "8"),
            ("CHECK_INTERVAL_MINUTES", "15"),
            ("DATA_DIR", "/data"),
            ("SEMESTER_ID", "-N000000015178000"),
        ]);

        let config = config(&pairs).unwrap();

        assert_eq!(config.gotify_priority, 8);
        assert_eq!(config.check_interval, Some(Duration::from_secs(900)));
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.semester_id, "-N000000015178000");
    }

    #[test]
    fn rejects_invalid_numbers() {
        for (key, value) in [
            ("GOTIFY_PRIORITY", "11"),
            ("GOTIFY_PRIORITY", "hoch"),
            ("CHECK_INTERVAL_MINUTES", "0"),
            ("CHECK_INTERVAL_MINUTES", "-5"),
        ] {
            let mut pairs = MINIMAL.to_vec();
            pairs.push((key, value));

            assert!(config(&pairs).is_err(), "{key}={value}");
        }
    }
}
