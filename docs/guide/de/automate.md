# Dinge automatisieren

Ein **geplanter Job** lässt einen Bot zu den Zeiten, die du wählst, von selbst etwas tun. Zum Beispiel: „Sag mir jeden Morgen, was heute ansteht.“ Du richtest ihn einmal ein. Danach läuft er von allein.

> **Bilder:** Zeilen, die mit „Bild:“ beginnen, beschreiben einen Screenshot, der später ergänzt wird.

## Deinen ersten Job erstellen

1. Klicke in der linken Leiste auf **Geplante Aufgaben**.
2. Drücke **Neuer Zeitplan**. Das Fenster **Neue geplante Aufgabe** öffnet sich.
3. Klicke unter **Mit einem Beispiel beginnen** auf ein Beispiel. Es füllt das Formular aus. Danach kannst du alles ändern.
4. Wähle unter **Wie oft?** **Einmal**, **Jeden Tag**, **Werktags**, **Jede Woche**, **Jeden Monat** oder **Anderes**.
5. Wähle unter **Um wie viel Uhr?** die Uhrzeit. Praxi zeigt die verwendete Zeitzone. Es zeigt auch **Nächste 5 Läufe**. Prüfe, ob das deiner Absicht entspricht.
6. Prüfe die **Anweisung**. Das ist das, was der Bot jedes Mal tun soll. Schreibe sie in eigenen Worten.
7. Wähle unter **Mich benachrichtigen** **Jedes Mal**, **Nur bei Problemen** oder **Nie**.
8. Drücke **Erstellen**.

> **Bild:** das Fenster **Neue geplante Aufgabe** mit **Wie oft?**, **Um wie viel Uhr?** und **Nächste 5 Läufe**.

## Drei Beispiele zum Übernehmen

### 1. Tagesüberblick

- **Wie oft?** Jeden Tag, um 08:00.
- **Anweisung:** „Fasse meine ungelesenen E-Mails und den heutigen Kalender in fünf kurzen Stichpunkten zusammen. Das Dringendste zuerst.“
- Du brauchst: E-Mail und Kalender verbunden. Siehe [Verbinde deine Apps](connect-your-apps.md).
- Sicher, weil der Bot nur liest und eine Zusammenfassung schreibt.

### 2. Wochenrückblick

- **Wie oft?** Jede Woche, freitags um 17:00.
- **Anweisung:** „Sieh dir die Chats und Notizen dieser Woche an. Liste auf, was erledigt ist, was offen bleibt und womit ich am Montag beginnen sollte.“
- Du brauchst: nichts Zusätzliches.

### 3. Preisbeobachter

- **Wie oft?** Jeden Tag, um 12:00.
- **Anweisung:** „Öffne die unten genannte Produktseite, lies den aktuellen Preis und sag mir, ob er niedriger ist als gestern. Produktseite: (Link hier einfügen)“
- Du brauchst: Ersetze den Text in Klammern durch deinen Link. Der Bot öffnet diese Seite, deshalb fragt er im Modus **Vorsichtig** eventuell vorher.

Dieselbe Liste enthält auch **Posteingang sortieren** und **Backup-Erinnerung**.

## Die Zeit in Worten sagen

Wähle **Anderes** und tippe die Zeit in eigenen Worten ein. Zum Beispiel „every weekday at 9“ oder „her gün 09:00“. Englisch und Türkisch funktionieren. Deine Worte liest ein fester Regelsatz, kein KI-Modell. Du siehst immer, was verstanden wurde, bevor du **Erstellen** drückst.

## Wenn der Computer aus war

Jobs laufen nur, solange Praxi auf einem wachen Computer offen ist. Unter **Wenn der Computer zur geplanten Zeit aus war** wählst du:

- **Überspringen und auf den nächsten Termin warten** (Standard).
- **Einmal ausführen, sobald der Computer wieder da ist**.
- **Nachholen: jeden verpassten Termin ausführen (bis zu 10)**.

Wenn Jobs nachts laufen sollen, nutze einen Server. Siehe [Auf diesem Computer oder auf einem Server](local-or-server.md).

## Jetzt ausführen, bearbeiten, pausieren, löschen

Bei jedem Job kannst du **Jetzt ausführen**, **Bearbeiten** oder **Löschen** drücken. Ein Job, der fehlgeschlagen ist oder verpasst wurde, hat **Erneut versuchen**. Vor dem Löschen wirst du um Bestätigung gebeten.

## Sicherheit

- Ein Job hat dieselben Berechtigungen wie sein Bot, nie mehr.
- Braucht ein Job dein OK, wartet er unter **Braucht dich**. Er macht nicht allein weiter. Antwortet niemand, wird die Antwort Nein.
- Webseiten und E-Mails, die ein Job liest, sind Information, nie Befehle.
- Ausgabenlimits, die du gesetzt hast, gelten auch für Jobs.

Mehr: [Geplante Jobs, einfach erklärt](schedules.md) und die [technische Referenz](../scheduled-tasks.md).

## Einen Bot von außen starten

Ein **Webhook** startet diesen Bot, wenn etwas anderes eine Adresse aufruft: GitHub, ein Formular oder ein Schalter zu Hause.

1. Öffne das Bot-Panel und wähle **Automatisierung**.
2. Gib dem Webhook einen Namen und schreibe, wenn du willst, was der Bot mit den Daten tun soll. Drücke **Webhook anlegen**.
3. Kopiere das Geheimnis sofort. Praxi zeigt es nur dieses eine Mal.
4. Praxi hört nur auf diesem Computer. Leite einen Tunnel deiner Wahl (Cloudflare Tunnel oder ssh) auf `127.0.0.1`. Öffne keinen neuen Port am Router.

Jeder Aufruf sendet die Zeit in `X-Webhook-Timestamp` und `X-Signature-256: sha256=` plus den HMAC aus dieser Zeit, einem Punkt und dem Rohtext. Derselbe signierte Aufruf gilt einmal und nur innerhalb von 5 Minuten. Der Text darf höchstens 64 KB groß sein. Mehr als 30 Aufrufe pro Minute werden abgelehnt. Der Text ist Daten, nie ein Befehl. Der Bot fragt weiter, bevor er handelt, und die Frage erscheint als Benachrichtigung.

**Test senden** startet einen Lauf aus der App. Er verbraucht nicht das Limit von außen.

Mehr: [Eingehende Webhooks](../incoming-webhooks.md).
