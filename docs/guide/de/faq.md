# FAQ

### Ist Praxi einsatzbereit?
Es befindet sich in früher Entwicklung. In der README steht, dass es keine stabile API und keine Releases gibt und
dass Dinge kaputtgehen werden. Es ist zum Ausprobieren und Weiterbauen gedacht.

### Verlässt etwas meinen Rechner?
Prompts und Werkzeugergebnisse gehen an den Modellanbieter, den du eingerichtet hast, und Telegram-Nachrichten
laufen über Telegram, wenn du es aktivierst. Es gibt keine Telemetrie. Mit einem lokalen Modell (Ollama, LM Studio)
und ohne Telegram verlässt nichts deinen Rechner.

### Kostet es Geld?
Praxi selbst ist kostenlos und Open Source (Apache-2.0). Du zahlst deinen Modellanbieter, oder nichts bei einem
kostenlosen Tarif oder mit lokalen Modellen. Siehe [Anbieter und Modelle](../providers-and-models.md). Die API
meldet den Token-Verbrauch und, wo der Katalog Preise hat, die geschätzten Kosten (`/v1/usage`; die Desktop-App
hat eine Nutzungsseite).

### Welche Betriebssysteme funktionieren?
CLI und Daemon lassen sich unter Windows, Linux und macOS bauen. Desktop-Steuerung (Klicken und Tippen in anderen
Apps) funktioniert unter Windows (UI Automation) und unter Linux mit X11 oder einem virtuellen Xvfb-Display
(AT-SPI2 und XTest; Wayland-Sitzungen werden abgelehnt). Das macOS-Backend ist ein Platzhalter und noch nicht
nutzbar. Die Desktop-App und das Installationsprogramm sind für Windows. Browser-Werkzeuge steuern einen
Chromium-basierten Browser über das DevTools-Protokoll und funktionieren überall, wo ein solcher Browser vorhanden ist.

### Kann ich es ohne Terminal benutzen?
Unter Windows ja: Installiere die Desktop-App, folge dem Assistenten, chatte, genehmige und plane über die Oberfläche.

### Kann der Bot etwas tun, ohne mich zu fragen?
Nur, was die Richtlinie erlaubt. Standardmäßig ist Lesen frei, und Änderungen fragen nach. Du kannst das pro Bot
und pro Kategorie erweitern. Geld und katastrophale Shell-Befehle fragen immer nach. Siehe
[Chat und Genehmigungen](../chatting-and-approvals.md).

### Wie stoppe ich einen Bot sofort?
Sende `/stop` im Chat oder in Telegram, drücke den globalen Hotkey (standardmäßig `Strg+Alt+Esc`, Desktop-App),
übernimm die Kontrolle in der Desktop-App oder stoppe den Daemon (Strg-C), was laufende Läufe pausiert.
`POST /v1/runs/{id}/cancel` bricht einen einzelnen Lauf ab.

### Kann ich mehrere Bots betreiben?
Ja. Jeder hat sein eigenes Profil, eigene Chats und Freigaben; siehe [Bots und Profile](../bots-and-profiles.md).

### Wo sind meine Daten, und wie sichere ich sie?
Im Datenverzeichnis (`--data-dir` / `BOT_DATA_DIR`, sonst der benutzerspezifische App-Datenordner des
Betriebssystems für `praxi-bot`): eine SQLite-Datenbank plus Ereignisprotokolle, `providers.toml`, Skills,
Workflows und das API-Token. Führe `bot backup create <Datei>` aus: Es schreibt eine einzelne, mit einer
Passphrase verschlüsselte Datei, die du mit `bot backup restore` wiederherstellen kannst (siehe
[Verschlüsseltes Backup und Wiederherstellung](../backup.md)). API-Schlüssel liegen in der Schlüsselverwaltung
deines Betriebssystems (oder in einer verschlüsselten Datei im Datenverzeichnis) und werden nur mit
`--include-secrets` mitgesichert.

### Wie sichere ich in Docker?

Der Container hat keine Passphrase gesetzt. Exportiere `BACKUP_PASSPHRASE` in deiner Shell und gib sie per Namen
weiter: `docker exec -e BACKUP_PASSPHRASE praxi-host bot backup create
/tmp/praxi.bak --passphrase-env BACKUP_PASSPHRASE`, und kopiere die Datei dann mit `docker cp` heraus. Die
Wiederherstellung braucht einen gestoppten Daemon (ein einmaliger Container auf demselben Volume); siehe
[Docker-Host](../docker-host.md).

### Wie mache ich einen Bot schneller und günstiger?
Nutze ein kleines `classifier`-Modell, speichere [Workflows](../workflows.md) für wiederkehrende Aufgaben, drücke
nach einem gelungenen Lauf auf **Das merken** und schreibe [Skills](../skills.md) für Seiten und Apps, mit denen
der Bot Mühe hat.

### Kann ich einen Workflow aufzeichnen, indem ich ihn vormache?
Noch nicht (**Geplant**). Heute sind Workflows TOML-Dateien, die du selbst schreibst, und „Merken“ speichert den
Weg eines erfolgreichen Laufs zwischen. Siehe [Workflows](../workflows.md).

### Gibt es ein Langzeitgedächtnis?
Im Repository gibt es eine Gedächtnisbibliothek (Abruf vor einem Durchgang, Extraktion danach), aber der Daemon
aktiviert sie noch nicht (**Geplant**). Chats behalten ihren eigenen Verlauf.

### Kann ich MCP-Server oder Plugins hinzufügen?
Bei MCP-Servern ja: `bot serve` lädt `mcp.toml` und startet die aktivierten Server, und du kannst sie über
`/v1/mcp/servers` verwalten; siehe [MCP-Server](../mcp-servers.md). Plugins sind noch **Geplant**: Das Design sieht
Plugins für Werkzeuge, Anbieter und Kanäle vor, aber für Endnutzer gibt es noch keine Plugin-API.

### Kann ein Bot einen anderen Computer steuern?
Ja, experimentell. Ein Bot kann auf einen gekoppelten PC gerichtet werden (das Computerziel des Profils; siehe
[Bots und Profile](../bots-and-profiles.md)), und der Daemon startet das Peer-Netzwerk, das ihn bedient. Der
Node-Agent (`bot-node`, Befehle `join`, `run`, `devices`) lässt einen Rechner, der dir gehört, ohne eingehende
Ports steuern. Läufe auf entfernten PCs bieten nur Desktop-Werkzeuge. Betrachte den ganzen Weg als **experimentell**.

### Welche anderen Messenger werden unterstützt?
Nur Telegram. Andere, etwa E-Mail oder Chat-Dienste, stehen in den Designdokumenten (**Geplant**).

### Gibt es Sprach-, Bild- oder Videoerzeugung?
Nein. Das steht nur auf der Design-Roadmap (**Geplant**).

### Kann ich Deutsch benutzen?
Ja. Die Desktop-App gibt es auf Englisch, Türkisch und Deutsch; der Assistent auf der Kommandozeile bietet Englisch
und Türkisch an, und der Zeitplan-Parser versteht türkische Ausdrücke wie `her gün 09:00`. Das Modell antwortet in
der Sprache, in der du ihm schreibst, und die `language`-Einstellung eines Profils kann eine Vorliebe festlegen.

### Wie aktualisiere ich?
Baue aus dem Quellcode neu oder installiere ein neueres Installationsprogramm; dein Datenverzeichnis bleibt
erhalten. Weil sich das Projekt schnell ändert, sichere vorher das Datenverzeichnis.

### Wie deinstalliere ich?
Windows: Nutze das Deinstallationsprogramm (es behält deine Daten, außer du hakst **Anwendungsdaten löschen** an).
CLI: Lösche die Programmdatei und, wenn du magst, das Datenverzeichnis sowie die `provider.*`- und
Telegram-Einträge in deiner Schlüsselverwaltung (`bot secrets delete`).

### Wo melde ich Fehler oder wie trage ich bei?
Siehe [CONTRIBUTING.md](../../../CONTRIBUTING.md).
