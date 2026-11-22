# Kalender (CalDAV)

Bots können in einem CalDAV-Kalender (Nextcloud, Radicale, Fastmail, iCloud, Synology ...) Termine lesen und
hinzufügen. Jeder Aufruf läuft wie bei jedem anderen Werkzeug durch die Berechtigungs-Engine.

## Einrichten

1. Speichere das Kalender-Passwort im Schlüsselspeicher (niemals in einer Konfigurationsdatei):

   ```
   bot secrets set calendar/home/password
   ```

2. Lege `<Datenverzeichnis>/calendar.toml` an:

   ```toml
   [[calendar]]
   name = "home"
   url = "https://dav.example.com/calendars/me/personal/"   # die Kalendersammlung
   username = "me"
   password = "calendar/home/password"                      # Verweis, nicht das Passwort selbst
   ```

3. Starte `bot serve` neu. Die `calendar_*`-Werkzeuge erscheinen bei Bots, die sie benutzen dürfen.

## Werkzeuge

| Werkzeug | Wirkung | Hinweise |
| --- | --- | --- |
| `calendar_list_events` | lesen | Termine, die einen Zeitraum überschneiden; Serientermine werden vom Server aufgelöst. Titel und Notizen sind nicht vertrauenswürdiger Text. |
| `calendar_create_event` | schreiben | Fügt einen Termin hinzu. Es werden keine Teilnehmer eingetragen, also wird niemand eingeladen oder benachrichtigt. |
| `calendar_delete_event` | zerstörerisch | Löscht einen Termin anhand der `uid`; fragt immer nach. |

Daten haben das Format `YYYY-MM-DD` (ganztägig) oder RFC 3339 mit Zeitversatz.

## Sicherheit

- Die URL muss `https` sein (einfaches `http` nur für einen Loopback-Host: `localhost`, `::1` oder `127.*`, z. B. ein Testserver).
- Weiterleitungen werden nie befolgt, damit das Passwort nicht an einen anderen Host gesendet werden kann.
- Ein vom Server gelieferter Link wird nur zum Löschen benutzt, wenn er innerhalb des konfigurierten Kalenders bleibt.
- Kalendertext, der ans Modell zurückgeht, wird als nicht vertrauenswürdige Daten markiert; ein Lauf, der ihn
  gelesen hat, gilt als „befleckt“ wie einer, der eine Webseite oder E-Mail gelesen hat.
