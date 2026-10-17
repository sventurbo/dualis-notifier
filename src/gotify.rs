//! Push notifications through a Gotify server.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Url;
use reqwest::blocking::Client;
use serde_json::json;

use crate::state::{Event, EventKind, FieldChange};

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
        let details = format_fields(&event.fields);
        match event.kind {
            EventKind::Changed => Self {
                title: format!("Note geändert: {name}"),
                message: format!(
                    "Für {name} wurde eine Note geändert.\n\
                     {details}Gehe zu {DUALIS_WEB_URL}, um den aktuellen Stand einzusehen."
                ),
            },
            EventKind::New => Self {
                title: format!("Neue Note: {name}"),
                message: format!(
                    "Für {name} wurden neue Ergebnisse veröffentlicht.\n\
                     {details}Gehe zu {DUALIS_WEB_URL}, um deine Note einzusehen."
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

    /// The message for one run. A single changed grade keeps its dedicated
    /// wording; several are bundled into one digest so the run sends exactly
    /// one message. A failed send then retries the whole batch instead of
    /// re-sending grades that were already delivered.
    pub fn for_events(events: &[Event]) -> Self {
        match events {
            [event] => Self::for_event(event),
            _ => Self::digest(events),
        }
    }

    fn digest(events: &[Event]) -> Self {
        let mut message = format!("Für {} Module gibt es Neuigkeiten:\n", events.len());
        for event in events {
            message.push_str(&format!("\n{}: {}\n", german_kind(event.kind), event.name));
            message.push_str(&format_fields(&event.fields));
        }
        message.push_str(&format!(
            "Gehe zu {DUALIS_WEB_URL}, um den aktuellen Stand einzusehen."
        ));
        Self {
            title: format!("{} Noten aktualisiert", events.len()),
            message,
        }
    }
}

fn german_kind(kind: EventKind) -> &'static str {
    match kind {
        EventKind::New => "Neue Note",
        EventKind::Changed => "Note geändert",
    }
}

/// One line per changed field, e.g. `Endnote: 1,7 → 1,3`, or just the value
/// for a brand-new module. Empty when there is nothing to report.
fn format_fields(fields: &[FieldChange]) -> String {
    fields
        .iter()
        .map(|field| match &field.old {
            Some(old) => format!("{}: {old} → {}\n", field.column, field.new),
            None => format!("{}: {}\n", field.column, field.new),
        })
        .collect()
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
                            Endnote: 1,7\n\
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
            fields: vec![FieldChange {
                column: "Endnote".into(),
                old: None,
                new: "1,7".into(),
            }],
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
            fields: vec![FieldChange {
                column: "Endnote".into(),
                old: Some("1,7".into()),
                new: "1,3".into(),
            }],
        };

        let notification = Notification::for_event(&event);

        assert_eq!(notification.title, "Note geändert: Mathematik");
        assert!(notification.message.contains("wurde eine Note geändert"));
        assert!(notification.message.contains("Endnote: 1,7 → 1,3"));
    }

    #[test]
    fn event_without_fields_has_no_extra_line() {
        let event = Event {
            kind: EventKind::New,
            name: "Mathematik".into(),
            fields: vec![],
        };

        let notification = Notification::for_event(&event);

        assert_eq!(
            notification.message,
            "Für Mathematik wurden neue Ergebnisse veröffentlicht.\n\
             Gehe zu https://dualis.dhbw.de/, um deine Note einzusehen."
        );
    }

    #[test]
    fn multiple_events_become_one_digest_message() {
        let events = [
            Event {
                kind: EventKind::New,
                name: "Mathematik".into(),
                fields: vec![FieldChange {
                    column: "Endnote".into(),
                    old: None,
                    new: "1,7".into(),
                }],
            },
            Event {
                kind: EventKind::Changed,
                name: "Physik".into(),
                fields: vec![FieldChange {
                    column: "Endnote".into(),
                    old: Some("2,3".into()),
                    new: "1,7".into(),
                }],
            },
        ];

        let notification = Notification::for_events(&events);

        assert_eq!(notification.title, "2 Noten aktualisiert");
        assert!(
            notification.message.contains("Neue Note: Mathematik"),
            "{}",
            notification.message
        );
        assert!(
            notification.message.contains("Note geändert: Physik"),
            "{}",
            notification.message
        );
        assert!(notification.message.contains("Endnote: 2,3 → 1,7"));
    }

    #[test]
    fn single_event_keeps_its_dedicated_wording() {
        let event = Event {
            kind: EventKind::New,
            name: "Mathematik".into(),
            fields: vec![],
        };

        assert_eq!(
            Notification::for_events(std::slice::from_ref(&event)),
            Notification::for_event(&event)
        );
    }

    #[test]
    fn rejects_invalid_url() {
        assert!(GotifyClient::new("gotify.example.com", "t", 5).is_err());
        assert!(GotifyClient::new("ftp://gotify.example.com", "t", 5).is_err());
    }
}
