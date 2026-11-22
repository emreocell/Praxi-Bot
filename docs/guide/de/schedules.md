# Geplante Jobs, einfach erklärt

Ein **geplanter Job** lässt einen Bot zu den Zeiten, die du wählst, von selbst etwas tun:
„werktags um 9:00 meine ungelesenen E-Mails zusammenfassen“. Du richtest ihn einmal ein, danach läuft er
von allein. (Die technische Referenz ist [Geplante Aufgaben](../scheduled-tasks.md).)

## Deinen ersten Job erstellen

1. Öffne **Zeitpläne** und drücke **Neu**.
2. Wähle ein **Beispiel** für einen schnellen Start (Tagesüberblick, Wochenrückblick, Posteingang sortieren,
   Preisbeobachter, Backup-Erinnerung) oder lass das Formular leer. Alles, was das Beispiel ausfüllt, kannst du ändern.
3. Wähle, **wie oft**: *Einmal*, *Jeden Tag*, *Werktags*, *Jede Woche*, *Jeden Monat* oder *Andere*.
4. Wähle die **Uhrzeit** (und den Tag, falls danach gefragt wird). Unter der Auswahl siehst du, welche Zeitzone
   verwendet wird. Darunter siehst du die **nächsten 5 Zeitpunkte**, zu denen der Job läuft, damit du prüfen
   kannst, ob es das ist, was du meintest.
5. Schreibe mit eigenen Worten, was der Bot tun soll.
6. Wähle, wann du **benachrichtigt** werden willst, und drücke **Erstellen**.

### In Worten eintippen

Wähle **Andere**, um den Zeitpunkt selbst zu beschreiben. Englisch und Türkisch funktionieren, zum Beispiel
`every weekday at 9`, `her gün 09:00`, `hafta içi her sabah 8`, `in 30 minutes`. Nichts wird von einem KI-Modell
geraten: Ein fester Satz von Regeln liest deine Worte. Du siehst immer zuerst, was verstanden wurde und wann der
Job als Nächstes läuft; drücke zur Bestätigung erneut **Erstellen**. Werden die Worte nicht verstanden, bekommst
du eine entsprechende Meldung; nutze dann die Auswahlfelder.

### Erweitert (eingeklappt)

*Erweitert* enthält den rohen **Cron-Ausdruck** für alle, die Cron kennen, und die Einstellung für
**verpasste Läufe**. Die meisten brauchen das nie.

## Wenn der Computer aus war

Jobs laufen nur, solange Praxi-Bot auf einem wachen Computer läuft. Für Zeitpunkte, die verstrichen sind, während
er aus war oder schlief, wählst du:

- **Überspringen** (Standard): auf den nächsten geplanten Zeitpunkt warten.
- **Einmal ausführen, wenn der Computer wieder da ist**: ein Nachhol-Lauf, egal wie viele verpasst wurden.
- **Nachholen**: jeden verpassten Zeitpunkt ausführen, höchstens 10.

Verpasste Zeitpunkte erscheinen im Verlauf des Jobs. Ein Neustart führt denselben Zeitpunkt nie zweimal aus.

## Jetzt ausführen, pausieren, bearbeiten, löschen

- **Jetzt ausführen** startet den Job sofort; die geplanten Zeitpunkte bleiben unverändert.
- **Pausieren** hält ihn an, ohne ihn zu verlieren; **Fortsetzen** schaltet ihn wieder ein.
- **Bearbeiten** ändert den Zeitpunkt, den Text oder die Benachrichtigungswahl.
- **Löschen** fragt vorher zur Bestätigung nach. Löschst du einen Bot, werden auch seine Jobs gelöscht.

## Verlauf und Probleme

Jeder Job listet seine letzten Läufe mit einem einfachen Status (fertig, fehlgeschlagen, läuft, verpasst) und
einem Link zum Chat, in dem er lief. Ein fehlgeschlagener oder verpasster Lauf hat die Schaltfläche
**Erneut versuchen**. Öffne den Lauf, um zu sehen, was passiert ist; wartete er auf eine Genehmigung, die nie
kam, kannst du ihn von dort fortsetzen.

## Dich benachrichtigen

Pro Job: **Jedes Mal**, **Nur wenn etwas schiefgeht** (Standard) oder **Nie**. Das wird auf dem Computer
gespeichert, den du gerade benutzt, und steuert nur, was die App ankündigt. Braucht ein Job dein OK, wirst du
immer benachrichtigt, und die Anfrage wartet unter **Wartet auf dich**.

## Sicherheit

Ein Job hat genau die Berechtigungen seines Bots, nie mehr. Alles, was deine Genehmigung braucht, wartet auf
dich, statt einfach weiterzulaufen, und verfällt sicher, wenn niemand antwortet. Webseiten, E-Mails und anderer
fremder Text, den ein Job liest, gelten als Information, nie als Befehl. Ein monatliches Ausgabenlimit
(Einstellungen) gilt auch für geplante Läufe. Jobs zu erstellen, zu ändern und zu löschen ist nur dem
Eigentümer erlaubt.
