# Home Assistant

Bots können deine Home-Assistant-Entitäten lesen und Dienste aufrufen (ein Licht einschalten, einen Thermostat
einstellen). Jeder Aufruf läuft durch die Berechtigungs-Engine.

## Einrichten

1. Öffne in Home Assistant dein Profil und erstelle ein **langlebiges Zugriffstoken**.
2. Speichere es im Schlüsselspeicher (niemals in einer Konfigurationsdatei):

   ```
   bot secrets set homeassistant/token
   ```

3. Lege `<Datenverzeichnis>/homeassistant.toml` an:

   ```toml
   url = "http://homeassistant.local:8123"
   token = "homeassistant/token"             # Verweis, nicht das Token selbst
   allowed_domains = ["light", "switch"]     # optional: begrenzt, was aufgerufen werden darf
   ```

4. Starte `bot serve` neu.

## Werkzeuge

| Werkzeug | Wirkung | Hinweise |
| --- | --- | --- |
| `ha_list_entities` | lesen | Zustände, auf Wunsch nur einer Domain. Namen sind nicht vertrauenswürdiger Text. |
| `ha_get_state` | lesen | Eine Entität mit Attributen. |
| `ha_call_service` | zerstörerisch | Wirkt in der echten Welt und fragt deshalb immer nach. |

## Sicherheit

- `https`, oder einfaches `http` nur für eine Adresse im Heimnetz oder localhost.
- Weiterleitungen werden nie befolgt, damit das Token nicht woandershin gesendet werden kann.
- Dienste, die Code ausführen oder die Zentrale stoppen, sind für Bots gesperrt: `shell_command`,
  `python_script`, `pyscript`, `hassio`, `rest_command` sowie
  `homeassistant.restart|stop|reload_all`, `recorder.purge*`.
- `allowed_domains` schränkt die aufrufbaren Domains weiter ein (leer = alles, was nicht gesperrt ist).
