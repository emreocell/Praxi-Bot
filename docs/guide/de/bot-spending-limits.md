# Ausgabenlimits für Bots

Jeder Bot kann ein Tages-, Wochen- und Monatslimit dafür bekommen, was seine Modellaufrufe verbrauchen dürfen.
Öffne den Bot, wähle **Bearbeiten** und dann **Limits**.

## Was du einstellen kannst

Für jeden Zeitraum (pro Tag, pro Woche, pro Monat; nach lokalem Kalender, die Woche beginnt am Montag) gibt es
vier optionale Felder:

| Feld | Was es tut |
| --- | --- |
| Warnen bei (USD) / (Token) | Benachrichtigt dich bei 80 % und bei 100 % dieses Betrags. Der Bot arbeitet weiter. |
| Stoppen bei (USD) / (Token) | Der Lauf stoppt **vor** einem Modellaufruf, der über diesen Betrag hinausgehen würde. |

Leer bedeutet: kein Limit. Eine Warnschwelle darf nicht über der Stoppschwelle liegen.

## Der Standard für neue Bots

Ein Bot, der nie eingestellt wurde, ist auf **5 USD oder 10 Millionen Token pro Tag** begrenzt. Der Bereich
Limits sagt das, und du kannst den Wert erhöhen oder entfernen. Bots, die es schon vor dieser Funktion gab,
laufen ohne Limits weiter, bis du welche setzt.

## Was am Limit passiert

Der Lauf endet mit einer Karte „Tageslimit erreicht“ (oder Wochen-, Monatslimit) und drei Möglichkeiten:
**Limit erhöhen** (öffnet den Bereich Limits), **Warten** (der Zähler wird zur angezeigten Zeit zurückgesetzt)
oder **Stoppen** (gestoppt lassen). Nichts wird in einer Schleife wiederholt, und das Modell wird nicht
aufgerufen. Geplante Läufe, Kanalnachrichten (zum Beispiel Telegram), Subagenten und Gedächtnisarbeit im
Hintergrund folgen denselben Limits.

## Wie der Verbrauch gezählt wird

- Das Geld stammt aus den aufgezeichneten Kosten jedes Aufrufs im [LLM-Aufrufprotokoll](../llm-call-ledger.md).
  Modelle ohne bekannten Preis zählen nur Token. Ein Geld-Limit (USD) kann sie deshalb nicht erfassen: Es stoppt
  kein Modell, dessen Preis unbekannt ist. Darum bekommt jeder neue Bot auch ein Token-Limit, das jedes Modell
  zählt. Setze zusätzlich ein Token-Limit, wenn du Modelle ohne bekannten Preis nutzt.
- Ein Aufruf, für den der Anbieter keinen Verbrauch gemeldet hat, wird mit einer vorsichtigen Schätzung gezählt;
  fehlende Daten bedeuten also nie „kostenlos“. Die Seite zeigt, wie viele Aufrufe so gezählt wurden.
- Die Zähler liegen im Protokoll und überstehen daher Neustarts. Parallele Läufe eines Bots teilen sich den
  verbleibenden Betrag.

## Wer Limits ändern darf

Nur der Eigentümer (die Desktop-App auf diesem Computer). Auch ein gekoppeltes Handy oder ein anderer Desktop
kann ein Limit weder ändern noch löschen; sie können es nur lesen. Ein Bot, eine CLI-Engine, der MCP-Modus oder
alles, was nur das API-Token besitzt, kann ein Limit weder ändern noch aufheben, und jede Änderung wird mit
altem und neuem Wert ins Audit-Protokoll geschrieben (`bot.spend_limits_changed`). Limits eines entfernten
Hosts werden auf diesem Host geändert.

Das installationsweite Monatslimit auf der Seite Nutzung gilt zusätzlich.

API: `GET` und `PUT /v1/bots/{id}/spend-limits`.
