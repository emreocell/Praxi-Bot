# Häufige Probleme lösen

Suche die Meldung, die du siehst. Führe die Schritte der Reihe nach aus. Die Meldungstitel unten sind die Worte, die die App zeigt.

Wenn nichts hilft: Öffne im Kontomenü **Hilfe**, drücke **Anzeigen, was kopiert wird**, lies es und drücke dann **Kopieren**. Schlüssel, Passwörter und E-Mail-Adressen werden unkenntlich gemacht. Es wird nichts irgendwohin gesendet. Füge den Text in einen Fehlerbericht ein. Sieh dir auch die technische Seite [Fehlerbehebung](../troubleshooting.md) an.

## 1. „Der lokale Dienst von Praxi wurde gestoppt“ oder „Praxi kann seinen lokalen Dienst nicht erreichen“

Der Hintergrundteil von Praxi läuft nicht.

1. Warte 10 Sekunden. Er startet vielleicht noch („Der lokale Dienst von Praxi startet“). Praxi versucht es von selbst erneut.
2. Drücke **Lokalen Dienst neu starten** (oder **Dienst neu starten**).
3. Immer noch nicht? Schließe Praxi ganz und öffne es neu.

Deine Bots und Chats sind sicher.

## 2. „Windows hat den Dienst von Praxi blockiert“ oder „eine Windows-Komponente fehlt“

Dein Virenschutz oder Windows hat den Start des Dienstes verweigert, oder ein Windows-Teil fehlt.

1. Blockiert: Öffne deinen Virenschutz oder SmartScreen und erlaube `bot.exe` im Ordner von Praxi. Drücke dann **Erneut versuchen**.
2. Komponente fehlt: Installiere das „Microsoft Visual C++ Redistributable (x64)“ von Microsoft. Drücke dann **Erneut versuchen**.
3. „Ein anderes Programm belegt die Verbindung, die Praxi braucht“: Schließe dieses Programm oder starte den Computer neu.
4. Wenn es sich wiederholt: Drücke **Details kopieren** und sende die Details. **Log-Ordner öffnen** zeigt das Protokoll.

## 3. Der Bot antwortet nicht, oder „Das Modell konnte nicht antworten“

Der KI-Dienst hat keine Antwort gegeben.

1. Lies die Details unter der Meldung. Sie nennen, was der Dienst gemeldet hat.
2. Drücke **Erneut versuchen**.
3. Öffne **Einstellungen → Bots & Modelle** und prüfe, ob dein Schlüssel dort steht. Wenn nicht, füge ihn erneut hinzu.
4. Wähle ein anderes Modell und versuche es erneut.

## 4. „hat diesen Schlüssel nicht akzeptiert“

Der Schlüssel wurde nicht vollständig kopiert oder gehört zu einem anderen Dienst.

1. Gehe zurück zur Schlüsselseite und kopiere den Schlüssel noch einmal. Kopiere die ganze Zeile.
2. Füge ihn erneut ein. Das Feld prüft ihn sofort.
3. Achte darauf, den richtigen Dienst gewählt zu haben (zum Beispiel Google Gemini für einen Gemini-Schlüssel).
4. Wenn du ein Programmier-Werkzeug als Gehirn nutzt: „Du bist bei diesem Programmier-Werkzeug abgemeldet“ heißt, du musst dich in den **Einstellungen** dort anmelden. „Dieses Abonnement hat sein Nutzungslimit erreicht“ heißt, warte, bis es zurückgesetzt wird, oder wähle ein anderes Modell.

## 5. „… konnte nicht erreicht werden“ oder „Das hat zu lange gedauert“

Wahrscheinlich liegt es an deiner Internetverbindung.

1. Prüfe, ob dein Internet in einem Browser funktioniert.
2. Drücke **Erneut versuchen**.
3. Wenn du ein VPN oder ein Firmennetz nutzt, schalte es kurz aus und versuche es erneut.

Ist eine Nachricht nicht durchgegangen („Deine Nachricht wurde nicht gesendet“), prüfe die Verbindung und sende sie erneut.

## 6. „Du hast diesen Schritt nicht erlaubt“

Du hast **Ablehnen** gedrückt, oder die Karte ist abgelaufen und die Antwort wurde Nein.

1. Frage den Bot erneut.
2. Wenn die Karte **Genehmigung erforderlich** erscheint, lies sie und drücke **Einmal erlauben**.
3. Willst du bei etwas, dem du vertraust, nicht jedes Mal gefragt werden? Nutze **Immer** auf der Karte. Siehe [Halte deinen Bot sicher](keep-your-bot-safe.md).

## 7. „Der Bot konnte die Datei oder das Programm nicht finden“ oder „… ist nicht installiert“

1. Frage erneut und nenne den vollständigen Namen oder den Ordner der Datei.
2. Wird in der Meldung ein Programm genannt, installiere es. Frage dann erneut.
3. Wenn der Bot einen Ordner nutzen sollte, den er nicht anfassen darf, füge ihn als vertrauenswürdigen Ordner hinzu: **Einstellungen → Sicherheit & Berechtigungen**.

## 8. „Der abgesicherte Modus ist aktiv“

Der abgesicherte Modus schaltet Werkzeuge, Workflows und Zeitpläne ab. Er dient dazu, Probleme zu beheben.

1. Schließe Praxi.
2. Öffne es normal (nicht im abgesicherten Modus).

Alles kommt zurück. Nichts wurde gelöscht.

## 9. „… hat nicht geantwortet“ oder „Der Bot konnte den Zielcomputer nicht erreichen“

Ein Server oder ein anderer Computer ist aus oder nicht verbunden.

1. Schalte ihn ein und prüfe sein Netzwerk.
2. Warte eine Minute und versuche es erneut.
3. Brauchst du diesen Computer nicht? Wähle im Computer-Menü **Dieser Computer**.

Mehr: [Auf diesem Computer oder auf einem Server](local-or-server.md).

## 10. „Bot-Limit erreicht“ oder „Dieser Bot nähert sich seinem Limit“

Du hast ein Ausgabenlimit gesetzt und der Bot hat es verbraucht. Der KI-Dienst wurde nicht aufgerufen.

1. Öffne die Einstellungen des Bots und erhöhe das Limit oder warte auf das Zurücksetzen.
2. Oder lass den Lauf gestoppt.

Siehe [Ausgabenlimits für Bots](bot-spending-limits.md).

## 11. Eine App zeigt „Fehler“, „Anmeldung erforderlich“ oder „In Quarantäne“

1. Öffne **Apps**, suche die App und drücke **Verwalten**.
2. **Anmeldung erforderlich**: Drücke **Anmelden**.
3. **Fehler**: Öffne die Details unter der Zeile und tu, was dort steht.
4. Drücke dann **Neu verbinden**.
5. „Composio ist noch nicht eingerichtet“: Drücke bei einer App auf **Verbinden** und melde dich einmal an.
6. „Ein Werkzeug hat sich nach Ihrer Freigabe geändert“: Lies beide Texte. Drücke **Neuer Version vertrauen** nur, wenn du der App vertraust.

Siehe [Verbinde deine Apps](connect-your-apps.md).

## 12. Ein geplanter Job lief nicht, oder „Dieser Zeitplan ist ungültig“

1. Jobs laufen nur, solange Praxi offen und der Computer wach ist. Wenn er aus war, erscheint der Job als verpasst. Drücke **Erneut versuchen**.
2. Öffne **Geplante Aufgaben** und prüfe, ob der Job nicht pausiert ist. Die Liste **Nächste 5 Läufe** zeigt, wann er läuft.
3. „Dieser Zeitplan ist ungültig“: Prüfe die Zeit oder die Wiederholungsregel. Nutze den Tab **Einfach**, statt Worte zu tippen.
4. Ist der abgesicherte Modus aktiv? Siehe Problem 8.
5. Der Job wartet vielleicht auf dein OK. Sieh unter **Braucht dich** nach.

Siehe [Dinge automatisieren](automate.md).
