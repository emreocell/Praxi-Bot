# Gedächtnis: woran sich deine Bots erinnern

In anderer Sprache: [English](../memory.md) · [Türkçe](../tr/memory.md).

Praxi-Bots können sich zwischen Chats nützliche Dinge merken, etwa deinen Namen, wie du Antworten magst oder ein Projekt, zu dem du immer wieder zurückkehrst. Diese Seite erklärt in einfachen Worten, was gemerkt wird, wo es liegt und wie du es ansiehst, korrigierst, vergisst und abschaltest.

## Was gemerkt wird

Nach einem normalen Chat-Durchgang schaut der Bot auf das, was **du** gesagt hast, und kann kurze Notizen behalten, etwa „Wohnt in Berlin“ oder „Mag kurze Antworten“. Diese Regeln werden im Code erzwungen, nicht nur dem Modell gesagt:

* Nur das, was du selbst gesagt hast, wird behalten, mit deinen genauen Worten als Zitat. Fragen, „Was wäre wenn“-Gedanken und Scherze werden nicht behalten.
* Passwörter, API-Schlüssel und alles, was nach einem Geheimnis aussieht, wird nie behalten.
* Vor einer Antwort werden die wenigen passenden Notizen als Hintergrund vor deine Nachricht gesetzt. Sie werden nie als Anweisung verwendet.

Nicht jede Notiz wird gleich behandelt:

* **Was du gesagt hast**, wird sofort gespeichert und genutzt (außer du wählst „Vor dem Speichern fragen“).
* **Was von außen aufgeschnappt wurde**, etwa aus einer Webseite, Datei, E-Mail, einem Werkzeugergebnis oder einem anderen Bot, wird als *nicht vertrauenswürdig* markiert. Es wartet unter **Braucht dein OK** und wird **erst nach deiner Freigabe genutzt**.

Ein Bot kann auch auf deine Bitte hin eine Notiz speichern oder vergessen („merk dir, dass ich am 3. März Geburtstag habe“). Dafür muss er deine eigene Nachricht zitieren.

## Wo es liegt

Alles liegt auf deinem Computer im Praxi-Datenordner (die Datei `memory.db`), getrennt von deinen Chats. Dieselbe Datei enthält auch eine durchsuchbare Kopie deiner früheren Nachrichten für das Werkzeug „frühere Chats durchsuchen“; ein Bot mit **ausgeschaltetem** Gedächtnis fügt dort nichts mehr hinzu. Zum Merken wird nichts an jemanden gesendet. Jeder Bot sieht deine gemeinsamen Notizen plus seine eigenen; Notizen anderer Personen sind nie sichtbar.

Schaltest du das **intelligente** Finden (unten) mit einem Online-Embedding-Anbieter ein, wird der Text deiner Notizen an diesen Anbieter gesendet, um in Suchdaten umgewandelt zu werden. Das geschieht nur, nachdem du ausdrücklich zugestimmt hast, und ist standardmäßig aus.

## Ansehen, korrigieren und vergessen

Öffne **Einstellungen → Persönliches Gedächtnis**, wähle einen Bot, dann kannst du:

* jede Notiz mit Kategorie, zugehörigem Bot, Speicherzeitpunkt und dem Zitat lesen, aus dem sie stammt;
* eine Notiz **bearbeiten** (deine Änderung zählt als von dir gesagt) oder **vergessen**;
* die letzte Änderung an einer Notiz **rückgängig machen**;
* selbst eine Notiz hinzufügen;
* eine **Vorschau** sehen, was genau vor deine nächste Nachricht gesetzt würde.

Eine Notiz, die nur eine Weile stimmt, zeigt **läuft ab** mit einem Datum. Danach nutzt der Bot sie nicht mehr. Sagst du Dinge wie „bis Freitag“, „diese Woche“ oder „für 3 Tage“, setzt der Bot dieses Datum selbst; er versteht solche Formulierungen auf Englisch, Deutsch und Türkisch und rät keine Daten, die er nicht lesen kann.

**Vergessen** blendet eine Notiz aus und stoppt ihre Nutzung. Sie bleibt im Rückgängig-Verlauf auf diesem Computer. Um alles von der Festplatte zu entfernen, schließe Praxi und lösche `memory.db` im Datenordner; die Bots starten dann mit leerem Gedächtnis.

## Braucht dein OK

Notizen aus Webseiten, Dateien, Werkzeugen oder anderen Bots erscheinen oben in den Gedächtnis-Einstellungen in der Liste **Braucht dein OK**. **Freigeben** erlaubt dem Bot, die Notiz zu nutzen (dem Modell wird sie weiterhin als zitierte Daten gezeigt, nicht als Befehl). **Verwerfen** vergisst sie. Bis du entscheidest, nutzt der Bot sie nicht und sie erscheint nie in der Vorschau.

## Ausschalten oder erst fragen

Jeder Bot hat seine eigene Einstellung unter **Bot bearbeiten → Erweitert → Gedächtnis** (und oben unter **Einstellungen → Persönliches Gedächtnis**):

| Einstellung | Was passiert |
| --- | --- |
| **An** (Standard) | Lernt aus dem, was du sagst. Dinge von außen warten auf dein OK. |
| **Vor dem Speichern fragen** | Jede neue Notiz wartet zuerst unter **Braucht dein OK**. |
| **Aus** | Der Bot lernt nichts, erinnert sich an nichts und hat keine Gedächtnis-Werkzeuge. Vorhandene Notizen bleiben gespeichert, werden aber nicht genutzt. |

Unter **Erweitert**:

* **Gedächtnis pro Antwort** legt fest, wie viel Hintergrund (etwa 150 bis 1500 Wörter, Standard 600) vor eine Nachricht gesetzt werden darf. Mehr gibt mehr Kontext, macht aber jede Antwort etwas langsamer und teurer.
* **Gelerntes mit meinen anderen Bots teilen** (Standard an) teilt Fakten über dich und vergangene Ereignisse mit all deinen Bots. Schaltest du es aus, behält dieser Bot alles, was er lernt, für sich. Vorgehensweisen und Lektionen bleiben immer bei dem Bot, der sie gelernt hat.

Nur du kannst diese Einstellungen auf dem Computer ändern, auf dem der Bot läuft; ein gekoppeltes Handy oder ein anderer Computer kann das nicht. Der **abgesicherte Modus** hält das Gedächtnis ganz aus.

## Wie Notizen gefunden werden: Einfach oder Intelligent

* **Einfach (Wortvergleich)** ist der Standard. Eine Notiz wird gefunden, wenn sie Wörter mit deiner Nachricht gemeinsam hat („Tee“ findet „mag grünen Tee“). Es braucht kein Modell und sendet nichts irgendwohin. Es versteht nicht, dass „Auto“ und „Wagen“ dasselbe bedeuten.
* **Intelligent (Bedeutung)** nutzt ein Embedding-Modell, das du bereits für das Wissen eines Bots eingerichtet hast (siehe [Wissens-Embeddings](../knowledge-embeddings.md)), sodass „Auto“ auch „fährt einen Wagen“ findet. Das Einschalten indexiert deine Notizen im Hintergrund neu und zeigt den Fortschritt; bis es fertig ist, gilt Einfach. Schlägt das Modell fehl, fällt Praxi auf Einfach zurück und nichts geht verloren.

Zum Umschalten nutze **Einstellungen → Persönliches Gedächtnis → Wie Erinnerungen gefunden werden**.
