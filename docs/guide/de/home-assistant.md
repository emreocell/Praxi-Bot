# Home Assistant

Bots können deine Home-Assistant-Entitäten lesen und Dienste aufrufen (ein Licht einschalten, einen Thermostat
einstellen). Du richtest es in der App ein, es gibt keine Datei zu bearbeiten. Jeder Aufruf läuft durch die
Berechtigungs-Engine.

## In der App einrichten

1. Öffne in Home Assistant dein Profil, gehe zu **Sicherheit** und erstelle ein **langlebiges Zugriffstoken**.
2. Öffne in Praxi **Apps** und die Karte **Home Assistant**.
3. Gib die Adresse ein (zum Beispiel `http://homeassistant.local:8123`), füge das Token ein und drücke
   **Save and test**. Praxi fragt `/api/` von Home Assistant ab und zählt deine Entitäten. Die Einrichtung
   wird nur gespeichert, wenn Home Assistant sie akzeptiert.
4. Wähle unter **Who can use Home Assistant** **All bots** oder **Only these bots** und drücke
   **Save access**.

Bots können es sofort nutzen, ohne Neustart. Das Token liegt im sicheren Speicher und wird nie wieder angezeigt:
Um es zu ändern, gib ein neues ein. **Test connection** prüft die gespeicherte Einrichtung erneut, **Remove**
löscht die Einrichtung und das gespeicherte Token; die Werkzeuge verschwinden bei allen Bots.

Das kann nur die Besitzerin oder der Besitzer: die Desktop-App oder der Besitzerschlüssel des Hosts. Ein Bot, eine
CLI-Engine oder ein Skript, das nur das API-Token hat, kann die Einrichtung weder lesen noch ändern.

## Auf der Kommandozeile

```
HA_TOKEN=... bot ha setup --url http://homeassistant.local:8123 --token-env HA_TOKEN
bot ha status
bot ha test
bot ha access --bot Atlas          # oder --all; ohne beides kein Bot
bot ha remove
```

`bot ha setup` liest das Token aus der genannten Umgebungsvariable (oder einer verdeckten Eingabe) und gibt es nie
aus. Mit `--allow cover.open_cover` (wiederholbar) füllst du die unten beschriebene Liste von Diensten.
Siehe [CLI-Referenz](../cli-reference.md).

## Werkzeuge und was nachfragt

| Werkzeug | Wirkung | Hinweise |
| --- | --- | --- |
| `ha_list_entities` | lesen | Zustände, auf Wunsch nur einer Domain. Namen sind nicht vertrauenswürdiger Text. |
| `ha_get_state` | lesen | Eine Entität mit Attributen. |
| `ha_call_service` | schreiben | Wirkt auf die echte Welt. Es folgt der Zugriffsstufe des Bots wie jede Schreibaktion, läuft also auf **Full access** ohne Karte. |

Nur einfache Geräte-Dienste folgen der Zugriffsstufe des Bots: Lichter, Schalter, Ventilatoren, Klima,
Mediaplayer, Staubsauger, Luftbefeuchter, Warmwasserbereiter, Zahlen und Auswahlen, Benachrichtigungen und
Sprachausgabe. Alles andere fragt auf **jeder** Stufe nach, auch auf Full access, es sei denn, du hast es erlaubt:

- jede andere Domain: Schlösser, Alarmzentralen, Abdeckungen (Garagentore, Rollläden), Ventile, Tasten, Helfer,
  Sirenen, Skripte, Automatisierungen und Szenen sowie rohe Protokolle wie MQTT oder Z-Wave, die ein Schloss
  erreichen können, ohne es zu nennen;
- jeder Dienst, dessen Name `unlock`, `disarm` oder `open` enthält;
- jeder Aufruf für eine Entität, deren Name `garage`, `gate`, `door`, `lock` oder `alarm` enthält (ein Garagentor
  ist oft ein einfacher Schalter);
- ein Aufruf für einen Bereich, ein Gerät, eine Etage, ein Label, `all` oder nichts Bestimmtes, außer bei Lichtern,
  Ventilatoren, Klima, Mediaplayern, Staubsaugern, Luftbefeuchtern und Warmwasserbereitern;
- ungewöhnliche Entitäts-IDs (alles außer einem kleingeschriebenen `domain.object`): Sie werden abgelehnt, bevor
  sie Home Assistant erreichen.

Damit für einen Dienst keine Karte mehr erscheint, trage ihn unter **Services allowed without asking** ein, einen
pro Zeile als `domain.service` (zum Beispiel `cover.open_cover`). Ein Dienst in der Liste läuft für jede Entität,
auf der er aufgerufen wird, ohne Nachfrage. Nur genaue Namen zählen, und `homeassistant.*` lässt sich nie eintragen.
Die übrigen Sicherheitsregeln, die auch auf Full access nachfragen können (zum Beispiel Daten, die deinen Computer
verlassen), gelten weiter.

## Sicherheit

- Bots sprechen nur mit der Adresse, die du angegeben hast. Weiterleitungen werden nie verfolgt, das Token kann
  also nicht woandershin gehen, und App, API und Kommandozeile geben das Token nie zurück.
- `https`, oder einfaches `http` nur für eine Heimnetz- (auch Tailscale) oder localhost-Adresse. Die Adresse darf
  nicht auf Praxis eigene API, die Cloud-Metadaten oder link-lokale Adressen zeigen.
- Dienste, die Code ausführen oder die Zentrale stoppen, sind für Bots gesperrt: `shell_command`,
  `python_script`, `pyscript`, `hassio`, `rest_command` sowie
  `homeassistant.restart|stop|reload_all`, `recorder.purge*`.
- Entitätsnamen und Attribute sind Text, den andere bearbeiten können: Bots behandeln sie als Daten, nie als
  Anweisungen.

## Von Hand (Hosts ohne Bildschirm)

Die App und `bot ha` schreiben `<Datenverzeichnis>/homeassistant.toml`, und eine selbst geschriebene Datei
funktioniert genauso. Das Token ist ein Verweis auf ein Geheimnis, nicht das Token selbst:

```toml
url = "http://homeassistant.local:8123"
token = "homeassistant/token"             # Name eines gespeicherten Geheimnisses
allowed_domains = ["light", "switch"]     # optional: nur diese Domains dürfen aufgerufen werden
trusted_services = ["cover.open_cover"]   # optional: keine Karte für diese
bots = ["<Bot-ID>"]                       # optional: fehlt = alle Bots, [] = keiner
```

Speichere das Token mit `bot secrets set homeassistant/token`. Starte `bot serve` neu, damit eine von Hand
bearbeitete Datei gelesen wird; Speichern in der App oder mit `bot ha` braucht keinen Neustart (und schreibt die
Datei ohne ihre Kommentare neu).
