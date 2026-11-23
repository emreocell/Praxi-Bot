# Verbindung testen

Jeder Schlüssel und jede App-Verbindung in Praxi-Bot hat eine Schaltfläche **Verbindung testen**:
KI-Anbieter-Schlüssel (Einstellungen → KI), Schlüssel für Websuche und Diktat (Einstellungen),
E-Mail-Konten, MCP-Server (Apps) und der Schlüssel-Schritt der Ersteinrichtung. Der Test läuft im
Praxi-Dienst auf deinem Computer, zeigt deinen Schlüssel nie an, gibt nach etwa 12 Sekunden auf und
antwortet mit einem der Ergebnisse unten. HTTP-Status und der Fehlercode des Dienstes stehen im Feld
**Details**.

Ein MCP-Servertest startet den Server und listet seine Werkzeuge auf. Er führt nie ein Werkzeug aus.

<a id="bad-key"></a>
## Falscher Schlüssel

Der Dienst hat abgelehnt (HTTP 401/403 oder eine Meldung wie „API key not valid"). Kopiere den Schlüssel
erneut von der Website des Anbieters, prüfe, dass am Anfang und Ende nichts fehlt und dass er zum
richtigen Anbieter gehört, und speichere ihn neu. Ein Schlüssel, den du in einen Chat oder eine Notiz
eingefügt hast, kann vom Anbieter gesperrt worden sein; erstelle einen neuen.

<a id="no-network"></a>
## Kein Netzwerk

Der Dienst war nicht erreichbar oder hat nicht rechtzeitig geantwortet. Prüfe Internetverbindung, VPN und
Proxy. Bei einem Programm auf diesem Computer (Ollama, LM Studio, ein MCP-Server) stelle sicher, dass es läuft.

<a id="quota-exceeded"></a>
## Limit erreicht

Der Schlüssel ist in Ordnung, aber Tarif, Guthaben oder Ratenlimit sind aufgebraucht (HTTP 402/429).
Warte etwas und versuche es erneut, oder prüfe Tarif und Guthaben auf der Website des Anbieters.

<a id="wrong-endpoint"></a>
## Falsche Adresse

Der Server hat geantwortet, aber nicht wie dieser Dienst (HTTP 404 oder eine Seite, die keine API ist),
oder der Befehl eines MCP-Servers ließ sich nicht starten. Prüfe die eingegebene Webadresse inklusive
`https://` und Pfad (viele Dienste erwarten einen Pfad, der auf `/v1` endet) oder den Befehl des MCP-Servers.

<a id="provider-outage"></a>
## Störung beim Anbieter

Der Dienst selbst hat ein Problem (HTTP 5xx). Versuche es in ein paar Minuten erneut oder sieh auf der Statusseite des Anbieters nach.

<a id="unknown"></a>
## Unklares Ergebnis

Keines der obigen. Öffne **Details**, vergleiche Status und Code mit der Dokumentation des Anbieters und
kopiere die Details, wenn du um Hilfe bittest. Sie enthalten nie deinen Schlüssel.
