# Computersteuerung auf deinem eigenen PC

Bots können in Apps auf deinem Windows-PC arbeiten, wenn der Chat **Dieser Computer** nutzt und der
Eigentümer-Schalter *Meine Bots dürfen diesen Computer benutzen* an ist. Ist er aus, zeigt der Chat eine Karte
**Computersteuerung einschalten** mit einer Schaltfläche.

## Was ein Bot tun kann

| Werkzeug | Zweck |
| --- | --- |
| `desktop_list_apps` | Listet sichtbare Fenster auf (ID, Titel, Prozess). |
| `desktop_launch` | Startet ein Programm, Dokument oder einen URI. Paketierte Apps (zum Beispiel WhatsApp) nutzen `appx:<AppUserModelID>`. |
| `desktop_focus` | Holt ein offenes Fenster nach vorn und liest es. |
| `desktop_get_state` | Liest den Barrierefreiheitsbaum (UI Automation) eines Fensters: Namen, Rollen, Bildschirmgrenzen. Optional ein Screenshot nur dieses Fensters. |
| `desktop_wait` | Wartet bis zu 10 s auf einen Fenstertitel oder einen Elementnamen bzw. eine Rolle. |
| `desktop_click`, `desktop_type`, `desktop_key`, `desktop_scroll` | Handeln per Elementindex (mit `gen`) oder, wenn der Baum das Ziel nicht enthält, per Bildschirm-x/y im beobachteten Fenster. |

Dem Modell wird gesagt, dass es zuerst den Barrierefreiheitsbaum lesen und nur dann einen Screenshot machen soll,
wenn der Baum das Ziel nicht zeigt.

## Sicherheit

- Jedes Fokussieren, Klicken, Tippen, jede Taste und jedes Scrollen läuft durch die Berechtigungs-Engine: Standardmäßig wird nachgefragt, und eine Genehmigung kann auf die aktuelle Aufgabe begrenzt werden.
- Genehmigte Eingaben werden vor dem Senden erneut gegen das tatsächliche Fenster geprüft; hat ein anderes Fenster den Fokus übernommen, wird die Aktion abgelehnt.
- In Passwortfelder wird nie getippt.
- Nichts läuft, während du den PC selbst benutzt oder solange Stopp / Pause aktiv ist. Die Anzeige *Praxi steuert deinen PC* und der Stopp-Hotkey bleiben immer in Kraft.

## Nicht überprüft

- Das Verhalten mit einer echten WhatsApp-Installation (paketierte App) wurde nicht auf einem Live-Rechner getestet.
- Ein Integrationstest mit Notepad/Rechner auf dem echten Desktop wurde nicht hinzugefügt; abgedeckt wird mit einem Fake-Backend und Unit-Tests.
- Linux-/macOS-Gegenstellen: Fokussieren und Warten fallen auf „nicht unterstützt“ zurück.
