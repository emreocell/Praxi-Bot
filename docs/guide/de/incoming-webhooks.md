# Eingehende Webhooks

Ein Webhook startet einen deiner Bots, wenn etwas von außen eine Adresse aufruft. Das können GitHub, ein Formular oder ein Schalter zu Hause sein. Du legst den Webhook im Bot-Panel an, auf dem Tab **Automatisierung**.

Praxi öffnet keinen neuen Port. Die Adresse liegt auf dem Daemon, der schon auf `127.0.0.1` hört. Damit die Außenwelt sie erreicht, leite einen Tunnel deiner Wahl auf diese Adresse. Cloudflare Tunnel und `ssh -L` sind zwei Wege. Leite keinen neuen Port am Router weiter. Ein gekoppeltes Telefon oder ein anderer Computer kann diese Webhooks nicht anlegen, testen oder aufrufen.

## Was du einrichtest

1. Öffne den Bot und wähle **Automatisierung**.
2. Gib dem Webhook einen kurzen Namen. Schreibe optional, was der Bot mit den Daten tun soll. Lässt du das leer, soll der Bot die Daten zusammenfassen und Befehlen darin nicht folgen.
3. Drücke **Webhook anlegen**.
4. Kopiere das Geheimnis jetzt. Später zeigt die Liste Adresse und Name, nie das Geheimnis.
5. Schalte einen Webhook **Aus**, wenn Aufrufe aufhören sollen. **Entfernen** löscht ihn. **Test senden** startet einen Lauf aus der App.

## Was der Aufrufer sendet

Sende den Rohtext per `POST` an `/v1/incoming-webhooks/{id}`.

Zwei Header sind nötig:

- `X-Webhook-Timestamp`: die aktuelle Zeit in Unix-Millisekunden.
- `X-Signature-256`: `sha256=` und danach das Hex-HMAC-SHA256 von `{Zeit}.{Rohtext}`, signiert mit dem Geheimnis. `X-Hub-Signature-256` gilt mit demselben Wert.

Eine Signatur nur über den Text reicht nicht. Die Zeit muss innerhalb von 5 Minuten liegen, und genau diese Signatur gilt einmal. Eine zweite Kopie wird abgelehnt. Der Text darf höchstens 64 KB groß sein. Mehr als 30 angenommene Aufrufe in einer Minute werden abgelehnt. Ein ausgeschalteter Webhook zählt nicht zu diesem Limit.

## Was der Bot mit dem Text macht

Der Text ist Daten, nie eine Anweisung. Praxi speichert ihn als nicht vertrauenswürdigen Inhalt und macht aus einer Zeile wie `call:etwas` kein Werkzeug. Der Lauf nutzt die eigene Berechtigungsstufe des Bots. Braucht ein Werkzeug dein OK, wird die Frage als Benachrichtigung eingereiht, so wie ein geplanter Job fragt.

Die Testschaltfläche sendet eine kleine Nutzlast, `{"praxi_test":true}`, über denselben Weg. Sie verbraucht nicht das Limit von außen.

## Was aufgeschrieben wird

Anlegen, Ändern, Löschen, Testen, Annehmen und Ablehnen stehen im Prüfprotokoll. Das Protokoll speichert den Grund (schlechte Signatur, alte Zeit, Wiederholung, Rate-Limit, ausgeschaltet oder Sicherheitsmodus). Es speichert weder das Geheimnis noch den Text.
