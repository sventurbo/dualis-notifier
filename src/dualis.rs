//! Login and results download for Dualis.

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, SET_COOKIE};

const SCRIPT_PATH: &str = "/scripts/mgrqispi.dll";
const USERNAME_DOMAIN: &str = "@student.dhbw-mannheim.de";
const TIMEOUT: Duration = Duration::from_secs(60);

static SESSION_ARGUMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ARGUMENTS=(.*?),").expect("valid regex"));

/// Credentials of one logged-in Dualis session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub cookie: String,
    pub id: String,
}

pub struct DualisClient {
    http: Client,
    base_url: String,
}

impl DualisClient {
    pub fn new(base_url: &str, user_agent: &str) -> Result<Self> {
        let http = Client::builder()
            .user_agent(user_agent)
            .timeout(TIMEOUT)
            .build()
            .context("HTTP-Client für Dualis kann nicht erstellt werden")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    pub fn login(&self, user: &str, password: &str) -> Result<Session> {
        let username = format!("{user}{USERNAME_DOMAIN}");
        let form = [
            ("usrname", username.as_str()),
            ("pass", password),
            ("APPNAME", "CampusNet"),
            ("PRGNAME", "LOGINCHECK"),
            (
                "ARGUMENTS",
                "clino,usrname,pass,menuno,menu_type,browser,platform",
            ),
            ("clino", "000000000000001"),
            ("menuno", "000324"),
            ("menu_type", "classic"),
            ("browser", ""),
            ("platform", ""),
        ];
        let response = self
            .http
            .post(format!("{}{SCRIPT_PATH}", self.base_url))
            .form(&form)
            .send()
            .context("Dualis ist nicht erreichbar")?;

        let status = response.status();
        session_from_headers(response.headers()).with_context(|| {
            format!(
                "Login bei Dualis fehlgeschlagen (HTTP {status}) – \
                 DUALIS_USER und DUALIS_PASSWD prüfen"
            )
        })
    }

    /// Download the results page for `semester_id` (empty for all results).
    pub fn fetch_results(&self, session: &Session, semester_id: &str) -> Result<String> {
        let url = format!(
            "{}{SCRIPT_PATH}?APPNAME=CampusNet&PRGNAME=COURSERESULTS&ARGUMENTS={},-N000307,{}",
            self.base_url, session.id, semester_id
        );
        let response = self
            .http
            .get(url)
            .header(COOKIE, &session.cookie)
            .send()
            .and_then(|response| response.error_for_status())
            .context("Notenübersicht kann nicht von Dualis geladen werden")?;

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .bytes()
            .context("Notenübersicht kann nicht von Dualis geladen werden")?;
        Ok(decode_body(content_type.as_deref(), &body))
    }
}

fn session_from_headers(headers: &HeaderMap) -> Result<Session> {
    let cookie = headers
        .get(SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|pair| pair.replace(' ', ""))
        .filter(|pair| !pair.is_empty())
        .context("Dualis hat kein Sitzungs-Cookie gesendet")?;
    let refresh = headers
        .get("REFRESH")
        .and_then(|value| value.to_str().ok())
        .context("Dualis hat keinen REFRESH-Header gesendet")?;
    let Some(id) = SESSION_ARGUMENT.captures(refresh).map(|c| c[1].to_owned()) else {
        bail!("Dualis hat keine Sitzungskennung gesendet");
    };
    Ok(Session { cookie, id })
}

/// Decode a response like Python's `requests` did, so cached names stay equal:
/// the charset from `Content-Type`, else a `<meta charset>` in the body itself
/// (Dualis omits it from the header but declares it in the HTML), else
/// ISO-8859-1 for text responses.
fn decode_body(content_type: Option<&str>, body: &[u8]) -> String {
    let content_type = content_type.unwrap_or_default();
    let label = charset_from_content_type(content_type).or_else(|| meta_charset(body));

    match label {
        Some(label) => decode_with_label(&label, body),
        None if content_type.to_ascii_lowercase().contains("text") => decode_latin1(body),
        None => String::from_utf8_lossy(body).into_owned(),
    }
}

fn charset_from_content_type(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim().eq_ignore_ascii_case("charset").then(|| {
            value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_owned()
        })
    })
}

/// `<meta charset="...">` or the older `<meta http-equiv="Content-Type"
/// content="...; charset=...">`. Matched on raw bytes since meta tags are
/// pure ASCII, so this needs no assumption about the body's own encoding.
static META_CHARSET: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
    regex::bytes::Regex::new(r#"(?is)<meta\b[^>]*charset\s*=\s*["']?([a-zA-Z0-9_-]+)"#)
        .expect("valid regex")
});

fn meta_charset(body: &[u8]) -> Option<String> {
    let capture = META_CHARSET.captures(body)?.get(1)?;
    Some(String::from_utf8_lossy(capture.as_bytes()).into_owned())
}

fn decode_with_label(label: &str, body: &[u8]) -> String {
    if is_latin1_label(label) {
        return decode_latin1(body);
    }
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(encoding) => encoding.decode_without_bom_handling(body).0.into_owned(),
        None => String::from_utf8_lossy(body).into_owned(),
    }
}

/// encoding_rs maps these labels to windows-1252; Python decodes them as true latin-1.
fn is_latin1_label(label: &str) -> bool {
    let label = label.to_ascii_lowercase().replace('_', "-");
    matches!(
        label.as_str(),
        "iso-8859-1" | "iso8859-1" | "latin-1" | "latin1" | "l1" | "cp819" | "iso-ir-100"
    )
}

fn decode_latin1(body: &[u8]) -> String {
    body.iter().map(|&byte| char::from(byte)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};

    fn client(server: &Server) -> DualisClient {
        DualisClient::new(&server.url(), "Test Agent").unwrap()
    }

    #[test]
    fn login_sends_encoded_form_and_reads_session() {
        let mut server = Server::new();
        let login = server
            .mock("POST", SCRIPT_PATH)
            .match_header("user-agent", "Test Agent")
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("usrname".into(), "s123456@student.dhbw-mannheim.de".into()),
                Matcher::UrlEncoded("pass".into(), "p&s+w%rd ä".into()),
                Matcher::UrlEncoded("PRGNAME".into(), "LOGINCHECK".into()),
            ]))
            .with_header("Set-Cookie", "cnsc =ABC123; path=/; secure")
            .with_header(
                "REFRESH",
                "0; URL=/scripts/mgrqispi.dll?APPNAME=CampusNet&PRGNAME=STARTPAGE_DISPATCH\
                 &ARGUMENTS=-N123456789012345,-N000019,-N000000000000000",
            )
            .create();

        let session = client(&server).login("s123456", "p&s+w%rd ä").unwrap();

        login.assert();
        assert_eq!(
            session,
            Session {
                cookie: "cnsc=ABC123".into(),
                id: "-N123456789012345".into(),
            }
        );
    }

    #[test]
    fn failed_login_is_reported_clearly() {
        let mut server = Server::new();
        server
            .mock("POST", SCRIPT_PATH)
            .with_body("Falsches Passwort")
            .create();

        let error = client(&server).login("s123456", "wrong").unwrap_err();

        let message = format!("{error:#}");
        assert!(
            message.contains("Login bei Dualis fehlgeschlagen"),
            "{message}"
        );
    }

    #[test]
    fn fetch_results_sends_cookie_and_semester() {
        let mut server = Server::new();
        let results = server
            .mock("GET", SCRIPT_PATH)
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("PRGNAME".into(), "COURSERESULTS".into()),
                Matcher::UrlEncoded(
                    "ARGUMENTS".into(),
                    "-N123,-N000307,-N000000015178000".into(),
                ),
            ]))
            .match_header("cookie", "cnsc=ABC123")
            .with_header("Content-Type", "text/html; charset=utf-8")
            .with_body("<p>Prüfungen</p>")
            .create();
        let session = Session {
            cookie: "cnsc=ABC123".into(),
            id: "-N123".into(),
        };

        let html = client(&server)
            .fetch_results(&session, "-N000000015178000")
            .unwrap();

        results.assert();
        assert_eq!(html, "<p>Prüfungen</p>");
    }

    #[test]
    fn fetch_results_fails_on_server_error() {
        let mut server = Server::new();
        server.mock("GET", Matcher::Any).with_status(500).create();
        let session = Session {
            cookie: "c=1".into(),
            id: "-N1".into(),
        };

        assert!(client(&server).fetch_results(&session, "").is_err());
    }

    #[test]
    fn decodes_like_python_requests() {
        let utf8 = "Prüfung – Öl".as_bytes();

        assert_eq!(
            decode_body(Some("text/html; charset=UTF-8"), utf8),
            "Prüfung – Öl"
        );
        assert_eq!(
            decode_body(Some("text/html; charset=\"utf-8\""), utf8),
            "Prüfung – Öl"
        );
        // Without a charset, requests fell back to ISO-8859-1 for text/*.
        assert_eq!(
            decode_body(Some("text/html"), utf8),
            "Pr\u{c3}\u{bc}fung \u{e2}\u{80}\u{93} \u{c3}\u{96}l"
        );
        assert_eq!(
            decode_body(Some("text/html; charset=iso-8859-1"), &[0xfc, 0x96]),
            "ü\u{96}"
        );
        assert_eq!(
            decode_body(Some("text/html; charset=windows-1252"), &[0x96]),
            "–"
        );
        assert_eq!(decode_body(None, utf8), "Prüfung – Öl");
    }

    #[test]
    fn falls_back_to_meta_charset_when_header_omits_it() {
        // Matches what the real Dualis server sends: no charset on the HTTP
        // header, but a UTF-8 meta tag in the page itself.
        let html = "<html><head>\
                     <meta http-equiv=\"Content-Type\" \t\tcontent=\"text/html; charset=utf-8\" />\
                     </head><body>unvollständig</body></html>";

        assert_eq!(decode_body(Some("text/html"), html.as_bytes()), html);
    }

    #[test]
    fn recognizes_the_html5_short_meta_charset_form() {
        let html = "<html><head><meta charset=\"utf-8\"></head><body>Prüfung</body></html>";

        assert_eq!(decode_body(Some("text/html"), html.as_bytes()), html);
    }

    #[test]
    fn header_charset_still_wins_over_a_conflicting_meta_charset() {
        let body: &[u8] = &[0xfc]; // 'ü' in ISO-8859-1
        let html_with_utf8_meta = [b"<meta charset=\"utf-8\">".as_slice(), body].concat();

        assert_eq!(
            decode_body(Some("text/html; charset=iso-8859-1"), &html_with_utf8_meta),
            "<meta charset=\"utf-8\">ü"
        );
    }
}
