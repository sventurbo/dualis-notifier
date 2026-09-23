//! Push notifications through a Gotify server.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Url;
use reqwest::blocking::Client;
use serde_json::json;

use crate::state::{Event, EventKind};

/// Opened when the notification is tapped, and linked in the message text.
const DUALIS_WEB_URL: &str = "https://dualis.dhbw.de/";
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub message: String,
}

impl Notification {
    pub fn for_event(event: &Event) -> Self {
        let name = &event.name;
        match event.kind {
            EventKind::Changed => Self {
                title: format!("Note geändert: {name}"),
                message: format!(
                    "Für {name} wurde eine Note geändert.\n\
                     Gehe zu {DUALIS_WEB_URL}, um den aktuellen Stand einzusehen."
                ),
            },
            EventKind::New => Self {
                title: format!("Neue Note: {name}"),
                message: format!(
                    "Für {name} wurden neue Ergebnisse veröffentlicht.\n\
                     Gehe zu {DUALIS_WEB_URL}, um deine Note einzusehen."
                ),
            },
        }
    }

    pub fn test() -> Self {
        Self {
            title: "Dualis Notifier".to_owned(),
            message: "Testnachricht: Die Verbindung zu Gotify funktioniert.".to_owned(),
        }
    }
}

pub struct GotifyClient {
    http: Client,
    endpoint: Url,
    token: String,
    priority: u8,
}

impl GotifyClient {
    pub fn new(base_url: &str, token: &str, priority: u8) -> Result<Self> {
        let endpoint = Url::parse(&format!("{}/message", base_url.trim_end_matches('/')))
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .with_context(|| format!("GOTIFY_URL ist keine gültige http(s)-Adresse: {base_url}"))?;
        let http = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("HTTP-Client für Gotify kann nicht erstellt werden")?;
        Ok(Self {
            http,
            endpoint,
            token: token.to_owned(),
            priority,
        })
    }

    pub fn send(&self, notification: &Notification) -> Result<()> {
        let body = json!({
            "title": notification.title,
            "message": notification.message,
            "priority": self.priority,
            "extras": {
                "client::notification": { "click": { "url": DUALIS_WEB_URL } }
            }
        });
        let response = self
            .http
            .post(self.endpoint.clone())
            .header("X-Gotify-Key", &self.token)
            .json(&body)
            .send()
            .context("Gotify ist nicht erreichbar")?;

        let status = response.status();
        if !status.is_success() {
            let details = response.text().unwrap_or_default();
            bail!("Gotify hat die Nachricht abgelehnt (HTTP {status}): {details}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};

    #[test]
    fn sends_message_with_token_priority_and_click_url() {
        let mut server = Server::new();
        let message = server
            .mock("POST", "/gotify/message")
            .match_header("x-gotify-key", "AbCdEf")
            .match_header("content-type", "application/json")
            .match_body(Matcher::Json(json!({
                "title": "Neue Note: Mathematik",
                "message": "Für Mathematik wurden neue Ergebnisse veröffentlicht.\n\
                            Gehe zu https://dualis.dhbw.de/, um deine Note einzusehen.",
                "priority": 8,
                "extras": {
                    "client::notification": { "click": { "url": "https://dualis.dhbw.de/" } }
                }
            })))
            .with_body(r#"{"id":1}"#)
            .create();
        let client = GotifyClient::new(&format!("{}/gotify/", server.url()), "AbCdEf", 8).unwrap();
        let event = Event {
            kind: EventKind::New,
            name: "Mathematik".into(),
        };

        client.send(&Notification::for_event(&event)).unwrap();

        message.assert();
    }

    #[test]
    fn rejected_message_is_an_error() {
        let mut server = Server::new();
        server
            .mock("POST", "/message")
            .with_status(401)
            .with_body(r#"{"error":"Unauthorized"}"#)
            .create();
        let client = GotifyClient::new(&server.url(), "wrong", 5).unwrap();

        let error = client.send(&Notification::test()).unwrap_err();

        assert!(error.to_string().contains("401"), "{error}");
        assert!(error.to_string().contains("Unauthorized"), "{error}");
    }

    #[test]
    fn changed_event_uses_changed_wording() {
        let event = Event {
            kind: EventKind::Changed,
            name: "Mathematik".into(),
        };

        let notification = Notification::for_event(&event);

        assert_eq!(notification.title, "Note geändert: Mathematik");
        assert!(notification.message.contains("wurde eine Note geändert"));
    }

    #[test]
    fn rejects_invalid_url() {
        assert!(GotifyClient::new("gotify.example.com", "t", 5).is_err());
        assert!(GotifyClient::new("ftp://gotify.example.com", "t", 5).is_err());
    }
}
