# Verbinde deine Apps

Eine **App** lässt deinen Bot mit einem anderen Dienst arbeiten, zum Beispiel Gmail, Notion, Linear oder deinen Dateien. Ohne Apps kann ein Bot nur chatten und seine Grundwerkzeuge nutzen.

Manche Apps heißen **MCP-Server**. Das ist nur ein technischer Name für „ein Zusatzmodul, das einem Bot neue Werkzeuge gibt“. Mehr musst du dazu nicht wissen.

> **Bilder:** Zeilen, die mit „Bild:“ beginnen, beschreiben einen Screenshot, der später ergänzt wird.

## Eine App verbinden

1. Klicke in der linken Leiste auf **Apps**.
2. Nutze **Apps suchen** oder die Kategorie-Chips, um die App zu finden. Apps, die du hinzufügen kannst, stehen unter **Entdecken**.
3. Drücke bei der App auf **Verbinden**.
4. In deinem Browser öffnet sich eine Seite. Melde dich dort an und erlaube den Zugriff. Gehe dann zurück zu Praxi.
5. Die App wechselt zu **Verbunden**.

Bei manchen Apps bittet dich Praxi zuerst, dich einmal bei einem kostenlosen Dienst namens Composio anzumelden. Danach öffnet es von selbst die eigene Seite der App. Du siehst „Schritt 1 von 2“ und „Schritt 2 von 2“.

> **Bild:** das Fenster **Apps** mit **Apps suchen**, **Entdecken** und **Verbunden**.

## Wer die App nutzen darf

Drücke bei einer verbundenen App auf **Verwalten**. Wähle unter **Wer diese App nutzen darf**:

- **Alle Bots**, oder
- **Nur diese Bots** und hake die Bots an. Drücke dann **Speichern**.

Wenn du keinen Bot ankreuzt, **kann kein Bot die App nutzen**. Gib jede App nur den Bots, die sie brauchen.

## Was jede Berechtigung bedeutet

| Was du siehst | Was es in einfachen Worten heißt |
| --- | --- |
| **Wer diese App nutzen darf** | Welche deiner Bots die App überhaupt nutzen dürfen. |
| **Werkzeuge, die Bots nutzen dürfen** | Die Liste dessen, was die App kann. Du kannst einzelne Werkzeuge ausschalten. |
| **ändert Daten** | Dieses Werkzeug kann Dinge in der App ändern, zum Beispiel eine Seite bearbeiten. |
| **kann löschen oder senden** | Dieses Werkzeug kann Dinge löschen oder nach außen senden, zum Beispiel eine E-Mail verschicken. |
| **Ordner, den der Bot lesen und beschreiben darf** | Manche Datei-Apps verlangen einen Ordner. Nur dieser Ordner wird geteilt. Die eigenen Daten von Praxi und deine Passwort- oder Anmeldeordner können nie geteilt werden. |
| **Abmelden** | Die Anmeldung vergessen. Die App bleibt in der Liste und zeigt **Anmeldung erforderlich**. |
| **Trennen** | Die App entfernen. Praxi fragt vorher „Diesen Server trennen?“. |

Sicherer Standard: Lass Werkzeuge mit **ändert Daten** und **kann löschen oder senden** ausgeschaltet, bis du sie brauchst.

## Riskante Schritte genehmigst du weiterhin

Eine App zu verbinden überspringt keine Genehmigungen. Wenn ein Bot über eine App etwas ändern oder senden will, siehst du eine Karte **Genehmigung erforderlich**. Bei Apps über Composio weist sie darauf hin, dass alle Argumente oben angezeigt werden. Drücke **Alle Argumente anzeigen**, um alles zu lesen, bevor du entscheidest. Siehe [Halte deinen Bot sicher](keep-your-bot-safe.md).

## Daten gehen an den Dienst der App

Wenn dein Bot eine App nutzt, gehen die gesendeten Daten an diesen Dienst. Bei Composio-Apps laufen sie über die Server von Composio. Verbinde nur Dienste, denen du vertraust.

## Wenn sich eine App ändert

Eine App kann ändern, wie sie eines ihrer Werkzeuge beschreibt. Passiert das, nachdem du es genehmigt hast, schaltet Praxi das Werkzeug aus. Du siehst **Ein Werkzeug hat sich nach Ihrer Freigabe geändert** mit **Das haben Sie freigegeben** und **Das steht jetzt dort**.

- Lies beide Texte.
- Drücke **Neuer Version vertrauen** nur, wenn du der App vertraust und der neue Wortlaut richtig aussieht.
- Wenn du unsicher bist, tu nichts. Das Werkzeug bleibt aus und der Bot kann es nicht nutzen.

## Statuswörter

| Wort | Was zu tun ist |
| --- | --- |
| **Verbunden** | Nichts. Es funktioniert. |
| **Verbindet** | Warte ein paar Sekunden. |
| **Anmeldung erforderlich** | Drücke **Anmelden**. |
| **Getrennt** | Drücke unter **Verwalten** auf **Neu verbinden**. |
| **Fehler** | Öffne die Details unter der Zeile. Behebe, was dort steht, und drücke dann **Neu verbinden**. |
| **In Quarantäne** | Die App ist mehrmals fehlgeschlagen und wurde angehalten. Behebe das Problem und drücke dann **Neu verbinden**. |

Mehr: [Apps verbinden](../apps.md) (alle Details) und [MCP-Server](../mcp-servers.md) (für alle, die sie von Hand einrichten).
