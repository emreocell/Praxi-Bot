# Helfer (optional): Embeddings und Entscheidungshelfer

Nichts davon brauchen Sie, um Praxi zu benutzen. Ohne Schlüssel nutzt Praxi eine
einfache Offline-Methode, um Wörter abzugleichen, und **nichts verlässt Ihren
Computer**.

Öffnen Sie **Einstellungen > Modelle > Helfer (optional)**, um mehr hinzuzufügen.

## Was sind Embeddings?

Embeddings machen aus einem Text eine Zahlenliste, damit Praxi Notizen und
Erinnerungen nach Bedeutung findet („meine Wohnung in Berlin“ passt zu „wo wohne
ich“). Praxi kann mehrere Anbieter nutzen und ist an keinen gebunden:

| Anbieter | Gut für |
| --- | --- |
| Google Gemini (`gemini-embedding-001`, `gemini-embedding-2`) | Für den privaten Gebrauch reicht ein kostenloser Schlüssel |
| OpenAI (`text-embedding-3-small`, `text-embedding-3-large`) | Kostenpflichtig, weit verbreitet |
| Jeder OpenAI-kompatible Server | Ollama, LM Studio, OpenRouter, Mistral, Together: Adresse, Modell, Vektorgröße und ggf. Schlüssel angeben |
| Das lokale Modell | In Builds mit lokalen Embeddings; der Text bleibt auf Ihrem Computer |
| Offline-Methode | Immer als letzte Rückfalloption da |

## Ihr Text wird nur mit Ihrer Erlaubnis gesendet

Jeder Anbieter außer dem lokalen Modell und der Offline-Methode **erhält den
Text** Ihrer Notizen, Erinnerungen oder Dokumentteile. Deshalb:

- Beim Hinzufügen eines Anbieters müssen Sie in klaren Worten bestätigen, dass Text
  an ihn gesendet wird. Ohne Haken wird der Anbieter nie aufgerufen.
- Passwörter und Schlüssel werden vorher aus dem Text entfernt.
- Als privat Markiertes wird nie gesendet; dieser Stapel nutzt die Offline-Methode.
- Sie können bestimmte Bots von Remote-Anbietern fernhalten.
- API-Schlüssel liegen im Geheimnisspeicher, nie in der Einstellungsdatei, und
  werden nie wieder angezeigt.
- Nur der Eigentümer (Desktop-App oder Eigentümerschlüssel des Hosts) kann das ändern.

## Kostenlose Schlüssel und Limits

Kostenlose Schlüssel haben Limits pro Minute (Anfragen und Tokens) und pro Tag.
Praxi zählt selbst mit, bleibt bei etwa 90 % jedes Limits, wartet kurz und nimmt
sonst den nächsten Anbieter, statt eine Sperre zu riskieren. Die Balken in den
Einstellungen zeigen Anfragen pro Minute, Tokens pro Minute und Anfragen pro Tag.
Die Zähler liegen im Speicher und beginnen nach einem Neustart von Praxi neu.

Die Limits stammen aus einer datierten Liste in Praxi (siehe „zuletzt geprüft“).
Anbieter ändern Limits; Sie können sie pro Anbieter überschreiben. Geprüft am
2026-11-24 für die kostenlose Stufe von Google AI Studio:

| Modell | Anfragen/Min. | Tokens/Min. | Anfragen/Tag |
| --- | --- | --- | --- |
| Gemini Embedding 1 und 2 | 100 | 30.000 | 1.000 |
| Gemini Decision 1 | 200 | 50.000 | 10.000 |

## Rückfallkette

Für jeden Zweck (passende Abkürzung oder Werkzeug wählen, Erinnerungen abrufen)
probiert Praxi die Anbieter in Ihrer Reihenfolge. Ein Anbieter ohne Kontingent,
mit abgelehntem Schlüssel oder offline ruht eine Weile, und der nächste wird
genutzt. Die Offline-Methode steht immer am Ende. Vektoren verschiedener Anbieter
werden nie vermischt.

## Entscheidungshelfer (optional)

Praxi kann Googles Decisions-Modell (`gemini-decision-1`) als optionalen Helfer für
kleine Ja/Nein- und Auswahlfragen nutzen. Er ist **standardmäßig aus**, braucht
einen Gemini-Schlüssel und wird an derselben Stelle eingeschaltet. Er berät nur: Eine
Entscheidung kann nie eine Erlaubnis erteilen oder ein Werkzeug genehmigen. Aufrufe
zählen wie andere Modellaufrufe im LLM-Aufrufprotokoll und in den Ausgabenlimits.
Wenn Sie `decider.toml` selbst geschrieben haben (optionaler
Entscheidungspool), lassen die Einstellungen sie in Ruhe.

## Probleme

| Sie sehen | Was zu tun ist |
| --- | --- |
| Schlüssel wurde abgelehnt | Schlüssel beim Anbieter prüfen und neu einfügen |
| Kostenloses Limit erreicht, wartet | Warten oder einen zweiten Anbieter hinzufügen; Praxi macht von selbst weiter |
| Antworten haben die falsche Größe | Die Vektorgröße passt nicht zum Modell; die vorgeschlagene Zahl verwenden |
| Nicht erreichbar | Serveradresse prüfen und ob der Server läuft |
