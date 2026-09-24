# Dualis Notifier

`dualis-notifier` prüft deine in Dualis veröffentlichten Modulnoten und schickt bei Änderungen eine Push-Nachricht über deinen [Gotify](https://gotify.net/)-Server.

Die Noten werden lokal in `grades.csv` gespeichert. Bei jedem weiteren Lauf vergleicht das Programm die Module über ihre Dualis-Modulnummer und benachrichtigt dich nur über neue oder tatsächlich geänderte Noten.

> Die Zugangsdaten liegen ausschließlich in deiner lokalen `.env`-Datei. Sie wird nicht in Git übernommen.

## Voraussetzungen

- [Rust](https://rustup.rs/) 1.88 oder neuer – oder alternativ Docker
- Ein Dualis-Benutzername, z. B. `s123456`
- Ein Gotify-Server mit einem App-Token (siehe [Gotify einrichten](#gotify-einrichten))

## Gotify einrichten

1. In der Gotify-Weboberfläche unter **Apps** → **Create Application** eine App anlegen, z. B. „Dualis“.
2. Den angezeigten **Token** der App kopieren. Er gehört in `GOTIFY_TOKEN`.
3. Die Adresse deines Servers, z. B. `https://gotify.example.com`, gehört in `GOTIFY_URL`.

Mit `GOTIFY_PRIORITY` (0–10, Standard 5) legst du fest, wie auffällig die Benachrichtigung in den Gotify-Apps erscheint. Ein Tipp auf die Benachrichtigung öffnet Dualis.

## Einrichtung ohne Docker

Repository öffnen und das Programm bauen:

```bash
cd ~/dualis-notifier
cargo build --release
```

Das fertige Programm liegt danach unter `target/release/dualis-notifier`.

Dann die Konfigurationsvorlage kopieren:

```bash
cp .env.example .env
chmod 600 .env
nano .env
```

Beispiel für den Inhalt von `.env`:

```env
# Nur die s-Kennung eintragen, ohne @student.dhbw-mannheim.de
DUALIS_USER=s123456
DUALIS_PASSWD=dein-dualis-passwort

# Leer lassen, um alle in der Dualis-Ansicht verfügbaren Ergebnisse abzurufen.
SEMESTER_ID=

# Gotify-Server und Token der dort angelegten App
GOTIFY_URL=https://gotify.example.com
GOTIFY_TOKEN=dein-app-token

# Optional
GOTIFY_PRIORITY=5
AGENT_NAME=Dualis Notifier
```

Die Datei wird beim Start aus dem aktuellen Arbeitsverzeichnis geladen. Werte, die als normale Umgebungsvariablen gesetzt wurden, haben Vorrang – dadurch bleibt Docker ebenfalls unterstützt.

### Alle Einstellungen

| Variable | Pflicht | Bedeutung |
| --- | --- | --- |
| `DUALIS_USER` | ja | s-Kennung ohne `@student.dhbw-mannheim.de` |
| `DUALIS_PASSWD` | ja | Dualis-Passwort |
| `GOTIFY_URL` | ja | Adresse des Gotify-Servers |
| `GOTIFY_TOKEN` | ja | Token der Gotify-App |
| `GOTIFY_PRIORITY` | nein | Priorität der Nachrichten, 0–10, Standard `5` |
| `SEMESTER_ID` | nein | Dualis-ID eines Semesters, leer für alle Ergebnisse |
| `CHECK_INTERVAL_MINUTES` | nein | Dauerhaft laufen und alle N Minuten prüfen (±20 % zufällig gestreut, siehe unten); leer für eine einzelne Prüfung |
| `DATA_DIR` | nein | Ordner für `grades.csv` und `grades.html`, Standard: aktuelles Verzeichnis |
| `AGENT_NAME` | nein | User-Agent gegenüber Dualis, Standard `Dualis Notifier` |

## Verbindung zu Gotify testen

```bash
cd ~/dualis-notifier
./target/release/dualis-notifier --test-notification
```

Kommt die Testnachricht an, stimmen `GOTIFY_URL` und `GOTIFY_TOKEN`.

## Erster Testlauf

```bash
cd ~/dualis-notifier
./target/release/dualis-notifier
```

Beim ersten erfolgreichen Lauf erscheinen diese Meldungen:

```text
W: No cache found
I: Created cache
```

Das ist erwartetes Verhalten: Es wird lediglich `grades.csv` als Ausgangsstand angelegt. Es wird dabei noch keine Gotify-Nachricht gesendet.

### Verhalten bei verschwundenen Modulen

Dualis kann Ergebnisse eines Moduls vorübergehend ausblenden und später erneut veröffentlichen. Das Programm behält ein in einer Abfrage fehlendes Modul deshalb im Cache und markiert es nur mit einem internen Zeitstempel. Erscheint es danach mit unveränderten Daten erneut, wird **keine** doppelte Nachricht gesendet.

Es gibt nur diese Benachrichtigungen:

- **Neue Note**: ein bisher unbekanntes Modul erscheint nach dem ersten Lauf.
- **Note geändert**: die gespeicherten Daten eines bekannten Moduls haben sich geändert.

Kann eine Nachricht nicht zugestellt werden, etwa weil Gotify nicht erreichbar ist, bleibt `grades.csv` unverändert. Der nächste Lauf erkennt die Änderung dann erneut und versucht es noch einmal.

## Automatisch prüfen

Für eine Prüfung alle 15 Minuten von 06:00 bis 19:45 Uhr täglich, diese Crontab einrichten:

```bash
crontab -e
```

Folgende Zeile einfügen:

```cron
*/15 6-19 * * * cd "$HOME/dualis-notifier" && ./target/release/dualis-notifier >> "$HOME/dualis-notifier/notifier.log" 2>&1
```

Den eingerichteten Zeitplan anzeigen:

```bash
crontab -l
```

Live-Logs ansehen:

```bash
tail -f ~/dualis-notifier/notifier.log
```

Ohne cron geht es auch: Mit `CHECK_INTERVAL_MINUTES=15` läuft das Programm dauerhaft und prüft selbst alle 15 Minuten, z. B. als systemd-Dienst. Der tatsächliche Abstand zwischen zwei Prüfungen wird dabei zufällig um ±20 % gestreut (bei 15 Minuten also 12–18), damit die Anfragen nicht in einem exakt gleichbleibenden Takt bei Dualis ankommen.

## Abgerufene Noten ansehen

Die gespeicherten Noten stehen in `grades.csv` und lassen sich mit jedem Tabellenprogramm öffnen oder direkt im Terminal anzeigen:

```bash
cat ~/dualis-notifier/grades.csv
```

Die zuletzt von Dualis geladene Seite liegt zur Fehlersuche in `grades.html`.

## Semester-ID

Normalerweise kann `SEMESTER_ID` leer bleiben. Das Programm ruft dann die Ergebnisse ab, die Dualis ohne Semestereinschränkung bereitstellt.

Wenn du auf ein bestimmtes Semester einschränken möchtest, übergib dessen Dualis-ID:

```env
SEMESTER_ID=-N000000015178000
```

Die ID steht – falls Dualis sie übergibt – in der Adresse einer Ergebnisseite direkt nach `-N000307,`.

## Docker (optional)

Das Projekt lässt sich auch als Container ausführen. Ein fertiges Image steht unter `ghcr.io/sventurbo/dualis-notifier:latest` bereit (wird bei jedem Push auf `main` automatisch gebaut):

```bash
docker run -d --name dualis-notifier --restart unless-stopped \
  --env-file .env \
  -v dualis-data:/data \
  ghcr.io/sventurbo/dualis-notifier:latest
```

Alternativ selbst bauen:

```bash
docker build -t dualis-notifier .
docker run -d --name dualis-notifier --restart unless-stopped \
  --env-file .env \
  -v dualis-data:/data \
  dualis-notifier
```

Der Container prüft alle 15 Minuten (änderbar über `CHECK_INTERVAL_MINUTES`). Die Noten liegen im Volume `dualis-data` und bleiben erhalten, wenn der Container neu erstellt wird.

Gotify-Verbindung testen und Logs ansehen:

```bash
docker run --rm --env-file .env dualis-notifier --test-notification
docker logs -f dualis-notifier
```

## Umstieg von der Python-Version

- Eine vorhandene `grades.csv` wird unverändert weiterverwendet. Es entstehen dabei weder doppelte noch verpasste Benachrichtigungen.
- In `.env` den Eintrag `DISCORD_WEBHOOK` durch `GOTIFY_URL` und `GOTIFY_TOKEN` ersetzen.
- Die Crontab-Zeile auf `./target/release/dualis-notifier` umstellen (siehe oben). Der Ordner `.venv` wird nicht mehr gebraucht.
- Bei Docker das Image neu bauen und den Container mit `-v dualis-data:/data` neu starten. Der Cache des alten Containers wird dabei nicht übernommen, der erste Lauf legt deshalb still einen neuen Ausgangsstand an.

## Entwicklung

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Sicherheit

- Teile weder deine Dualis-Zugangsdaten noch dein Gotify-App-Token.
- Gib keine Dualis-URLs mit `ARGUMENTS=-N…` weiter; sie können eine temporäre Sitzungskennung enthalten.
- Die lokale `.env` ist per `.gitignore` vom Commit und per `.dockerignore` vom Docker-Image ausgeschlossen. Prüfe vor einem Commit trotzdem immer `git status`.
