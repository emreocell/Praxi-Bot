# Offline diktieren und vorlesen

Die lokale Spracherkennung und Sprachausgabe in der Desktop-App brauchen nach der Einrichtung
keine Internetverbindung. Der Hintergrunddienst muss mit der Cargo-Funktion `local-voice` gebaut
sein (`cargo build -p cli --features local-voice`). Der Standard-Build enthält die Funktion
noch nicht.

1. Öffne **Einstellungen → Allgemein → Sprachdiktat** und wähle den Anbieter für diesen
   Computer. Lade ein Whisper-Modell herunter und speichere die Auswahl. Whisper und Silero
   sind MIT-lizenziert. Einschließlich Silero braucht Tiny etwa 105 MB, Base 162 MB und Small
   377 MB. Small erkennt Deutsch und Türkisch am besten. Verwende das Mikrofon am Eingabefeld
   oder die unter Mikrofon und Push-to-Talk eingerichtete Sprechtaste.
2. Öffne **Einstellungen → Allgemein → Stimme**. Lade unter **Offline read-aloud** das
   CPU-Modell Supertonic 3 herunter (etwa 146 MB, OpenRAIL-M) und schalte die Option ein.
   Verwende die Vorlesetaste an einer Chat-Antwort oder aktiviere das automatische Vorlesen.
   Im Freisprechmodus liest die Stimme des Betriebssystems vor.

Vor dem Download zeigt die App die Größe, die Quelle und die Lizenz an. Der Dienst prüft den
Speicherplatz und hält beim Download 512 MiB frei. Er speichert die Dateien unter
`<Datenverzeichnis>/models/voice/` und prüft vor der Verwendung ihre SHA-256-Prüfsummen.
Ein unterbrochener Download wird beim nächsten Klick auf die Download-Taste fortgesetzt.

Beide Modelle laufen auf der CPU. Das erste Audio kann ein paar Sekunden dauern. Die Sprache
richtet sich nach der App-Sprache. Deutsch, Englisch und Türkisch werden unterstützt. Bei einer
von Supertonic nicht unterstützten Sprache liest die App auf Englisch vor. Der Bot erzeugt
Antworten weiterhin über das für ihn eingestellte Sprachmodell.

`bot doctor` und die Diagnose in den Einstellungen zeigen, welche Modelldateien vorhanden sind.
Die Modellprüfung in den Einstellungen und der Sprachprozess prüfen die SHA-256-Prüfsummen.
Mehr zur Bedienung eines Sprachanrufs steht in der [englischen Anleitung](../voice-call.md).
