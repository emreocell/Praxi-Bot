# Fehlerbehebung (technisch)

Beginne mit `bot status`. Der Befehl zeigt, ob der Hintergrunddienst (Daemon) läuft, ob dein Token akzeptiert wird und wie viele Genehmigungen auf dich warten. Wenn du kein Terminal benutzt, schau zuerst in die einfach geschriebene Seite [Häufige Probleme lösen](fix-common-problems.md).

## Aktuelle Protokolle und ein Supportpaket

Öffne **Einstellungen → Hilfe → Letzte Protokolle anzeigen** (auch in der Hilfe
verfügbar). Suche oder wähle eine Stufe. **Angezeigte Protokolle kopieren** kopiert
genau die bereinigten Einträge auf dem Bildschirm. **Supportpaket ansehen**
bereitet vier Dateien vor: `versions.json`, `diagnostics.json` (Dienstbereitschaft
und sicherer Modus), `recent-logs.jsonl` und die bereinigte `doctor.md`. Öffne jede
Vorschau vor **Supportpaket speichern**. Die ZIP-Datei speichert genau diese Inhalte,
einschließlich aller aktuellen Protokolle unabhängig vom Suchfilter. Chats und
Zugangsdaten sind ausgeschlossen. Es wird nichts automatisch hochgeladen. Ist die
Bereinigung nicht möglich, wird kein Paket angeboten.

Im Terminal zeigt `bot logs --since 15m --level warn` die Warnungen der letzten
15 Minuten. `bot logs --follow` wartet auf neue Einträge (mit Strg-C beenden).
`--since` akzeptiert auch `2h`, `1d` oder einen UTC-Unix-Zeitstempel in Millisekunden.
Die lokalen Dateien lassen sich auch bei gestopptem Dienst lesen; verwende seinen
Datenordner. Ein leeres Ergebnis kann bedeuten, dass der Filter nichts gefunden
hat oder ältere Einträge bereits ersetzt wurden.

Unter `<Datenordner>/logs` liegen je Daemon und Sidecar höchstens fünf Dateien mit
je 2 MiB. Die Anzeige liest maximal 500 aktuelle Einträge. `RUST_LOG` steuert die
Tracing-Stufe (Standard `info`); auch `trace` schließt Anfrageinhalte, Header,
Zugangsdaten und persönliche Pfade aus. Fehlgeschlagene HTTP-Anfragen enthalten
eine Routenvorlage, Status, Fehlercode und eine erzeugte Anfrage-ID. Startphasen
und Neustartgründe werden ebenfalls erfasst. Auf Linux-systemd-Hosts gelangt
dieselbe strukturierte Ausgabe ins Journal: `journalctl -u praxi.service --since
'15 minutes ago'` (verwende den Namen deiner installierten Unit).
`GET /v1/logs/recent?level=warn&since=<Millisekunden>` braucht sowohl das API-Token
als auch den lokalen Eigentümerschlüssel. Gekoppelte Geräte werden immer abgewiesen.

## Diagnose (Einstellungen → Diagnose, `bot doctor`)

**Einen Fehler melden:** Öffne die Hilfe und nutze *Diagnoseinfos kopieren*. Zuerst wird genau der Text angezeigt, der kopiert würde (Versionen, Plattform, Prüfergebnisse; Schlüssel, Passwörter, E-Mail-Adressen und dein Benutzerordner sind unkenntlich gemacht, Nachrichten sind nicht enthalten). Kopiert wird erst, wenn du auf *Kopieren* drückst. Es wird nichts irgendwohin gesendet.

Die Desktop-App hat in den Einstellungen eine Seite **Diagnose**. Sie listet dieselben Prüfungen wie `bot doctor`: Anbieter-Schlüssel und Anmeldung bei Coding-CLIs, geladene Anbieter, Datenordner (auch mit einem Dateinamen mit türkischen Buchstaben), Browser, Desktop-Automatisierung, Peer- / VPS-Verbindung, Speicherplatz und Uhr. Was die Plattform nicht bietet, wird als *nicht verfügbar* angezeigt, nicht als Fehler.

- **Das Lesen der Seite ändert nichts.** Für einige Probleme gibt es eine Reparatur-Schaltfläche (Datenordner anlegen, Anbieter neu laden). Eine Reparatur läuft nur, wenn du ihre Schaltfläche drückst, und jeder Versuch wird im Audit-Log festgehalten. Nichts anderes wird als Ein-Klick-Lösung angeboten; das API-Token wird nie neu erzeugt.
- **Support-Bericht kopieren** kopiert eine Markdown-Zusammenfassung (Versionen, Plattform, Prüfergebnisse, *Namen* der Konfigurationsdateien). Dein Benutzerordner wird als `~` angezeigt; Geheimnisse, die der Daemon kennt (auch URL-kodiert, base64 oder JSON-maskiert), das API-Token, schlüsselähnliche Zeichenfolgen, Zugangsdaten und Hostnamen in URLs, die Zeilen, die ein Konfigurations-Parsefehler zitiert, deine Benutzer- und Computernamen sowie E-Mail-Adressen sind unkenntlich gemacht. Lassen sich die Geheimnisse nicht lesen, wird kein Bericht erstellt. Lies ihn, bevor du ihn teilst.
- **Peer- / VPS-Verbindung** wird gegen die Live-Verbindung des Daemons geprüft: Ein konfigurierter Host, der offline oder nicht gekoppelt ist, oder ein Daemon ohne Peer-Netzwerk wird als Problem gemeldet. Eine Coding-CLI, deren Anmeldung nicht bestätigt werden konnte, ist eine Warnung, nie „bereit“.
- **Host-Display (`host_display`)** prüft auf einem Linux-Dauerbetriebs-Host die Voraussetzungen der Reihe nach und meldet das erste fehlende Teil statt einer allgemeinen Warnung: ob `DISPLAY` gesetzt ist, ob das Display antwortet (und ob `XAUTHORITY` gesetzt und lesbar ist) und ob ein nutzbarer Browser ohne Snap vorhanden ist (`BOT_BROWSER_PATH` oder `/opt/praxi-browser/chrome`). Führe auf einem systemd-Host `sudo praxi doctor` aus, damit die Prüfung mit dem eigenen Benutzer, der Umgebung (`/etc/praxi/praxi.env`) und dem Mount-Namespace des Dienstes läuft statt mit deiner interaktiven Shell.
- **Freier Arbeitsspeicher (`free_ram`)** liest unter Linux `MemAvailable` aus `/proc/meminfo` und warnt unter 512 MiB (mit dem Vorschlag, `limits.max_computer_runs` in `host.toml` zu senken).
- Die Daemon-Routen hinter der Seite sind `GET /v1/doctor`, `POST /v1/doctor/repair/{id}` und `GET /v1/doctor/report` (Zugriffstoken nötig). Sie sind lokal auf diesen Computer beschränkt: gekoppelte Handys und Desktops können sie nicht aufrufen. `bot doctor` führt weitere Prüfungen aus, die ein Terminal brauchen (Port-Test, begrenzter Start eines Headless-Browsers mit 5 Sekunden Startzeit / 8 Sekunden Gesamtzeit, `--online`-Verbindungstests).

## Der Dauerbetriebs-Host

Logs, Neustart, Upgrade, Backup und die Tabelle zu Host offline / langsamer Start / Peer stehen in [Host-Betrieb](../host-operations.md).

## `daemon not reachable at http://127.0.0.1:47821` (Exit-Code 3)

Der Daemon läuft nicht oder lauscht auf einem anderen Port.

- Starte ihn: `bot serve` (oder öffne die Desktop-App, die ihn startet).
- Hast du für `serve` `--port` oder `BOT_PORT` benutzt? Verwende dasselbe für den Client-Befehl oder gib `--url http://127.0.0.1:<port>` an.
- Port schon belegt: Starte mit `bot serve --port 47822` und richte die Clients darauf aus.

## Authentifizierung abgelehnt (Exit-Code 4)

Das Token oder die Host/Origin-Prüfung ist fehlgeschlagen.

- Der Client liest `<Datenordner>/api-token`. Wenn du den Daemon mit einem anderen `--data-dir` oder `BOT_DATA_DIR` gestartet hast, gib dem Client denselben.
- Die API akzeptiert nur Loopback-Hosts. Setze keinen Proxy mit einem anderen Hostnamen davor.
- Entwickelst du eine Web-Oberfläche gegen den Daemon? Füge ihren Origin vor `bot serve` zu `BOT_ALLOWED_ORIGINS` hinzu (kommagetrennt).

## `no usable provider; run bot setup or bot providers add`

`bot serve` hat keinen aktivierten Anbieter gefunden, den es aufbauen kann.

- `bot providers list` zeigt, was eingerichtet ist. Ein Anbieter ohne Schlüssel wird beim Start mit einer Warnung übersprungen (`provider <id> skipped: ...`).
- Füge einen hinzu: `bot providers add gemini --key-env GEMINI_API_KEY`.
- Schlüssel in Umgebungsvariablen werden gelesen, wenn der Befehl läuft; setze die Variable in derselben Shell, und stelle bei der Desktop-App sicher, dass sie nach dem Setzen gestartet wurde (oder speichere den Schlüssel stattdessen über die verdeckte Eingabe).

## `connection test failed`

`bot providers add` und `bot providers test` rufen den Anbieter auf. Prüfe den Schlüssel, das Netzwerk oder den Proxy und bei lokalen Anbietern, ob Ollama oder LM Studio läuft und das Modell heruntergeladen ist. Mit `--no-test` wird der Anbieter ohne Test gespeichert.

## Ein Bot antwortet „has no usable profile“

Das gespeicherte Profil des Bots ist kein gültiges Profil. Das passiert beim ersten Bot, den der CLI-Assistent (`bot setup`) speichert. Lege stattdessen mit `bot bots add` einen Bot aus einer Profildatei an; siehe [Bots und Profile](../bots-and-profiles.md). Ein Profil braucht mindestens `[bot]` mit `id` und `name` sowie `[models]` mit `executor`.

## `bot bots add` wird abgelehnt (Exit-Code 6)

Die Meldung nennt das Problem: ungültiges TOML, ein fehlender `executor`, eine unbekannte Berechtigungsvorlage (nimm `read-only`, `read-and-draft`, `approve-destructive` oder `full`) oder ein fehlerhaftes `[tools]`-Glob.

## Der Bot hält an und wartet

Er wartet auf dich: auf eine Genehmigung oder eine Frage. Die Anzahl zeigt `bot status`. Oder achte in der Chat-Ausgabe auf `[approval needed]` / `[question]` und antworte mit `bot approve <id>` / `bot answer <id> "..."`. Genehmigungen für geplante Läufe laufen nach 30 Minuten ab, danach wird der Lauf pausiert.

## Ein Werkzeugaufruf wird abgelehnt

- Die Richtlinie sagt **deny**: Vorlage `read-only` oder eine `deny`-Regel. Sieh dir das Profil des Bots an und `GET /v1/bots/{id}` für die gültige Richtlinie.
- Desktop-Werkzeuge: Du hast in der Desktop-App die Steuerung übernommen oder den Bot pausiert. Gib die Steuerung zurück.
- Gar keine Desktop-Werkzeuge: Es gibt sie nur unter Windows, und `BOT_DESKTOP=off` schaltet sie ab.

## „Immer erlauben“ bleibt nicht gespeichert oder fragt nie

`bot grants list <bot-id>` zeigt die gespeicherten Erlaubnisse. Widerrufe eine, damit der Bot wieder fragt. Geld und Aktionen auf der Nie-Liste fragen von Natur aus immer, egal welche Erlaubnis es gibt.

## Rate-Limit-Fehler von Gemini (HTTP 429)

Du hast ein Limit der kostenlosen Stufe erreicht, meist das Tageslimit für Anfragen dieses Modells. Warte auf das Zurücksetzen, wechsle zu einem Modell mit höherem Limit, reduziere Zeitpläne oder nimm einen bezahlten Schlüssel oder einen anderen Anbieter. Siehe [Gemini-Gratisstufe](../providers-and-models.md#gemini-free-tier).

## Sicherer Modus: alles aus starten

Nutze ihn, wenn etwas, das von selbst handelt, die Reparatur behindert: ein Zeitplan, der bei jedem Start einen fehlschlagenden Lauf auslöst, ein MCP-Server, der beim Start hängt, ein Bot, der immer wieder den Bildschirm steuert.

Der sichere Modus wird **nur beim Start des Dienstes** gewählt, und zwar von dir:

- **Desktop-App:** Einstellungen → Diagnose → **Im sicheren Modus neu starten**, oder **Im sicheren Modus starten** auf dem Bildschirm, der erscheint, wenn der Dienst nicht hochkommt.
- **Terminal:** `bot serve --safe`, oder setze `BOT_SAFE_MODE=1` vor `bot serve`.

Solange er an ist, zeigt die App ein Banner („Sicherer Modus. Werkzeuge, Zeitpläne und Kanäle sind aus.“), und `bot doctor`, Einstellungen → Diagnose und `GET /v1/health` (`safe_mode`) sagen es ebenfalls. Du kannst weiter chatten (das Modell antwortet ohne Werkzeuge; jeder Werkzeugaufruf wird abgelehnt) und Bots, Zeitpläne und Einstellungen lesen und ändern.

Aus ist: jedes Werkzeug, Zeitpläne und Workflows (nichts wird ausgelöst), die Telegram-Brücke und andere Kanäle, MCP-Server und Apps, E-Mail-/Kalender-/Web-Anbindungen, Erinnerungsabruf und Zurückschreiben, das Desktop-Backend sowie Peer- oder Host-Netzwerk. Gespeicherte Workflows werden nie abgespielt (eine passende Chat-Zeile beantwortet das Modell, der Endpunkt zum Ausführen wird abgelehnt), und Coding-CLI-Ausführer werden abgelehnt. Unterbrochene Läufe werden nicht fortgesetzt: Beim Start des Dienstes werden Läufe, die liefen oder auf eine Genehmigung warteten, als pausiert markiert, wie nach jedem Neustart, und auch ein späterer normaler Start setzt sie nicht fort; sie bleiben sichtbar. Die Einstellungsseiten der ausgeschalteten Teile antworten „nicht eingerichtet“, bis du den sicheren Modus verlässt.

Nichts wird gelöscht oder verändert: Bots, Chats, Zeitpläne, Kanäle und Einstellungen bleiben genau, wie sie waren, und `mcp.toml` wird nicht einmal geöffnet. Zum Verlassen drücke im Banner **Normal neu starten** (oder starte den Dienst ohne `--safe` und ohne `BOT_SAFE_MODE` neu); dann ist alles wieder da. Solange der Dienst läuft, kann kein Werkzeug, Bot oder gekoppeltes Gerät den sicheren Modus ein- oder ausschalten. Er ist eine Hilfe zur Fehlersuche, kein Schutz gegen Schadsoftware.

## Zeitpläne lösen nicht aus

- Der Daemon muss zur fälligen Zeit laufen. Verpasste Auslösungen werden standardmäßig übersprungen.
- `bot schedules list` zeigt die nächste Laufzeit. Wurde die Formulierung nicht verstanden, schlägt `add` fehl; probiere eine Form aus [Geplante Aufgaben](../scheduled-tasks.md) oder sieh dir die Vorschau mit `POST /v1/schedules/parse` an.
- Kalenderformulierungen nutzen UTC, wenn du nicht `--tz` angibst.
- Eine neue Auslösung wird übersprungen, solange der vorige Lauf desselben Zeitplans noch wartet oder läuft.

## Telegram

- `pair` sagt, es braucht den Daemon: Starte zuerst `bot serve`; der Code wird vom laufenden Daemon erzeugt und läuft nach 10 Minuten ab.
- Nach `setup` passiert nichts: Starte `bot serve` neu; `bot channels telegram status` zeigt eingerichtet / aktiviert / läuft.
- Zwei Programme, die dasselbe Bot-Token abfragen, kommen sich in die Quere (Telegram antwortet mit 409). Nimm pro laufendem Daemon ein eigenes Token und stelle sicher, dass für den Bot kein Webhook gesetzt ist.
- Dir wird gesagt, du sollst koppeln: Sende `/pair <Code>` mit einem frischen Code an den Bot.

## Der Geheimnisspeicher fragt nach einer Passphrase

Beim Backend mit verschlüsselter Datei kommt die Passphrase aus `BOT_SECRETS_PASSPHRASE` oder einer Eingabeaufforderung. Erzwinge mit `--secrets-backend keyring` den Schlüsselbund deines Betriebssystems, falls du einen hast.

## Der Browser öffnet sich nicht oder läuft in ein Zeitlimit

- **Begrenzte Warte- und Sitzungszeiten**: `browser_open` begrenzt den Start des Browsers (Chrome/Chromium starten, auf `DevToolsActivePort` warten und den ersten Tab öffnen) auf **40 Sekunden** pro Versuch und das Warten auf einen anderen Browser-Aufruf, der die Sitzung hält, auf **45 Sekunden**. Bei einem Zeitlimit wiederhole `browser_open` nicht in einer Schleife; führe auf diesem Computer `bot doctor` aus (`sudo praxi doctor` auf einem Linux-Host), um zu sehen, was fehlt.
- **Meldungen bei frühem Chrome-Ende**: Beendet sich der Browserprozess beim Start, enthält der Fehler die letzten Zeilen der Chrome-Ausgabe (`Browser said: ...`) und hebt bekannte Ursachen hervor: ein fehlendes X-Display, fehlendes oder unlesbares `XAUTHORITY`, eine blockierte Chrome-Sandbox (fehlendes AppArmor-Profil unter Ubuntu 24.04) oder fehlende gemeinsame Bibliotheken.
- **Snap-Browser**: Als Snap verpacktes Chromium/Chrome wird beim gehärteten Linux-Dienst nicht unterstützt. Installiere einen Browser ohne Snap oder setze `BOT_BROWSER_PATH`.
- **Langsamer Xvfb-Start unter Linux**: Schlägt die Abfrage des X11-Displays bei einem vollen Systemstart fehl, fragt der Daemon nach **15 Sekunden** erneut ab, statt bis zum Neustart im Headless-Modus hängen zu bleiben.

## Windows SmartScreen blockiert das Installationsprogramm

Die Builds sind noch nicht code-signiert. Wähle **Weitere Informationen** und dann **Trotzdem ausführen**, aber nur, wenn du das Installationsprogramm selbst gebaut hast oder ihm vertraust.

## Informationen für einen Fehlerbericht sammeln

Gib `bot --version`, dein Betriebssystem, den genauen Befehl und Fehler, ob du die Desktop-App oder die CLI nutzt, und die passende Daemon-Ausgabe an. Füge dein `api-token`, API-Schlüssel oder den Inhalt deines Datenordners nicht ein.
