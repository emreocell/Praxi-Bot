# Ältere unverschlüsselte Backups wiederherstellen

Aktuelle Versionen erstellen **verschlüsselte** Backups; siehe [Verschlüsseltes Backup und Wiederherstellung](../backup.md).
Ältere Versionen schrieben mit `bot backup <DATEI>` unverschlüsselte Archive. `bot restore` liest sie weiterhin:

```sh
bot restore ./praxi-backup.tar            # zuerst den Daemon stoppen
bot restore ./praxi-backup.tar --replace  # vorhandene Bots und Chats ersetzen
```

- Der Befehl weigert sich zu laufen, solange ein Daemon das Datenverzeichnis belegt oder am Dienstport antwortet.
- Eine Installation, die schon Bots oder Chats hat, braucht `--replace`; der ersetzte Zustand bleibt in einem
  `backup-*`-Ordner im Datenverzeichnis erhalten.
- `--allow-policy-relaxation` akzeptiert ein Backup, dessen Berechtigungsrichtlinie freizügiger ist als die aktuelle.
- Unverschlüsselte Archive enthalten keine API-Schlüssel: Gib sie erneut ein (`bot secrets set`, `bot setup`).
- Mit `--json` wird bei Erfolg ein JSON-Objekt ausgegeben: `file`, `files`, `app_version`,
  `previous_state`. Fehler gehen als Text an stderr.

Behandle alte `.tar`-Backups als sensibel und erwäge, sie mit `bot backup create` neu zu erstellen.
