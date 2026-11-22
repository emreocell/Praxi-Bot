# Glossar

Alphabetische Übersicht der wichtigsten Begriffe, die in Praxi, im Benutzerhandbuch und im Code vorkommen.

- **Genehmigung** — Eine Bestätigungskarte, die erscheint, wenn ein Bot etwas tun will, wofür er eine Erlaubnis braucht. Du genehmigst oder lehnst jede einzeln ab. Siehe [Chat und Genehmigungen](../chatting-and-approvals.md). Code: `crates/permission-engine/`.
- **Bot** — Ein Assistent mit eigenem Namen, eigener Persönlichkeit, eigenem Modell, eigenen Werkzeugen und eigenen Berechtigungsregeln. Jeder Bot hat eigene Chats und Freigaben. Siehe [Bots und Profile](../bots-and-profiles.md).
- **Daemon** — Der Hintergrunddienst (`bot serve`), der die Agentenschleife ausführt, Daten in SQLite speichert, Berechtigungen durchsetzt und Zeitpläne auslöst. Er lauscht nur auf `127.0.0.1`. Siehe [CLI-Referenz](../cli-reference.md).
- **Egress-Sperre (egress gate)** — Die Regel, die eine Genehmigung verlangt, bevor ein Lauf, der nicht vertrauenswürdige Inhalte gelesen hat, Daten vom Rechner nach außen schicken darf. Siehe [Sicherheitsmodell](../security.md). Code: `crates/permission-engine/src/egress.rs`.
- **Host** — Ein dauerhaft laufender Linux-Server, auf dem der Daemon im Host-Modus läuft, damit Bots auch bei ausgeschaltetem Desktop weiterarbeiten. Siehe [Auf einem entfernten Host betreiben](../remote-host.md).
- **Never-Liste** — Eine fest eingebaute Liste katastrophaler Shell-Befehle (Datenträger formatieren, Systemordner löschen), die unabhängig von der Richtlinie immer eine Genehmigung verlangen. Siehe [Sicherheitsmodell](../security.md). Code: `crates/permission-engine/src/never_list.rs`.
- **Node-Agent** — Ein experimenteller Helfer (`bot-node`), mit dem ein Bot einen anderen Rechner steuern kann. Siehe [FAQ](faq.md). Code: `apps/node-agent/`.
- **Berechtigungsvorlage (Preset)** — Ein benannter Satz Standard-Berechtigungsregeln für einen Bot: `read-only`, `read-and-draft`, `approve-destructive` (Standard) und `full`. Siehe [Chat und Genehmigungen](../chatting-and-approvals.md). Code: `apps/cli/src/presets.rs`, `crates/permission-engine/src/modes.rs`.
- **Lauf (Run)** — Ein einzelner Arbeitsdurchgang: Der Bot erhält eine Nachricht, denkt nach, ruft Werkzeuge auf und streamt seine Antwort. Auch Durchgang genannt. Siehe [Chat und Genehmigungen](../chatting-and-approvals.md).
- **Zeitplan** — Ein zeitgesteuerter Auslöser, der einen Bot von selbst einen Lauf starten lässt (z. B. werktags um 9 Uhr). Siehe [Geplante Aufgaben](../scheduled-tasks.md).
- **Skill** — Eine kurze Markdown-Anleitung, die einem Bot zeigt, wie er eine knifflige Aufgabe, Seite oder App angeht. Kein Code, sondern Hinweise, die der Bot liest, wenn sie passen. Siehe [Skills](../skills.md).
- **Workflow** — Eine gespeicherte, parametrisierte Abfolge von Werkzeugaufrufen, die ohne Modellaufrufe erneut abgespielt wird. Siehe [Workflows](../workflows.md).
