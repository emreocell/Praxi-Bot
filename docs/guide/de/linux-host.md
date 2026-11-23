# Linux-Dauerbetrieb (virtuelles Display, Update, Deinstallation)

Diese Seite enthält die Befehle zum Kopieren und Einfügen, um Praxi auf einem Linux-Server laufen zu lassen, der rund um die Uhr an ist, zum Beispiel auf einer kostenlosen ARM-Ubuntu-VM. Der [Assistent „Server hinzufügen“](../servers.md) startet dasselbe Installationsskript und zeigt dieselben Meldungen; die ausführliche Anleitung steht unter [Betrieb auf einem Remote-Host](../remote-host.md).

Führe alle Befehle auf dem Server per SSH aus. Ersetze `v0.1.0` durch die gewünschte Version.

## 1. Erst Vorschau, dann installieren

Sieh dir immer zuerst die Vorschau an. `--dry-run` ändert nichts und braucht kein `sudo`:

```bash
bash ./install-linux.sh --dry-run --headless --version v0.1.0
```

Installation (prüfe das Skript vorher, wie in [remote-host.md](../remote-host.md) beschrieben):

```bash
sudo bash ./install-linux.sh --headless --version v0.1.0
```

`--headless` bedeutet: Dauerbetrieb (`bot serve --host`, lauscht nur auf `127.0.0.1`, kein offener Port) plus ein **virtuelles Display** (Xvfb) und ein Chromium-Browser, damit Bots surfen und einen Desktop nutzen können, ohne dass jemand angemeldet ist. Das Display hat `1920x1080x24` auf `:99`. Mit `--screen` änderst du es:

```bash
sudo bash ./install-linux.sh --headless --screen 1280x720x24 --version v0.1.0
```

## 2. Was installiert wird

| Teil | Details |
| --- | --- |
| `praxi.service` | Startet nach einem Absturz neu (nach 5 Sekunden, höchstens 10 Mal in 10 Minuten); abgesichert: keine neuen Rechte, schreibgeschütztes System, eigener Temp-Ordner. |
| `praxi-xvfb.service` | Das virtuelle Display, ohne Netzwerkzugriff, mit privatem Zugangs-Cookie. |
| Logs | Landen im System-Journal. Die Dienste von Praxi sind ratenbegrenzt (je höchstens 2000 Zeilen pro 30 Sekunden), damit ein außer Kontrolle geratener Bot es nicht fluten kann. Das Installationsprogramm ändert die Größenbegrenzung des System-Journals **nicht** und startet journald nie neu, weil andere Dienste auf demselben Server es mitnutzen. Lies die Logs von Praxi mit `journalctl -u praxi -f`. |
| Daten | `/var/lib/praxi` (deine Bots, Dateien, Geheimnisse). Einstellungen in `/etc/praxi`. |

## 3. Alles prüfen

```bash
sudo praxi doctor
```

Der Doctor prüft Display, Browser, freien Speicherplatz, freien Arbeitsspeicher, die Uhr und den Port; zu jedem Problem steht der genaue Befehl. Die häufigsten:

| Doctor meldet | Ausführen |
| --- | --- |
| Display antwortet nicht | `sudo systemctl restart praxi-xvfb`, dann `sudo systemctl status praxi-xvfb` |
| kein Browser gefunden | `sudo bash ./install-linux.sh --update --with-desktop` |
| wenig Speicherplatz | `sudo journalctl --vacuum-size=200M` und `sudo apt-get clean` |
| Uhr geht falsch | `sudo timedatectl set-ntp true` |
| Port belegt | `sudo ss -ltnp 'sport = :47821'` zeigt, wer ihn nutzt; mit `--port` einen anderen wählen |

## 4. Update (Daten bleiben erhalten)

```bash
bash ./install-linux.sh --dry-run --update --version v0.1.1
sudo bash ./install-linux.sh --update --version v0.1.1
```

`--update` ersetzt nur das Programm. Deine Bots, Dateien, Geheimnisse, Displaygröße und der Host-Modus bleiben. Ist Praxi noch nicht installiert, bricht das Skript mit einer klaren Meldung ab, statt etwas Neues zu installieren.

## 5. Deinstallieren

```bash
sudo bash ./install-linux.sh --uninstall          # entfernt Praxi, BEHÄLT deine Daten
sudo bash ./install-linux.sh --uninstall --purge  # löscht auch alle Daten. Nicht rückgängig zu machen.
```

Beides lässt sich vorher mit `--dry-run` ansehen.

Ältere Installationsprogramme haben eine serverweite Journal-Begrenzung geschrieben (`/etc/systemd/journald.conf.d/50-praxi.conf`). `--update` und `--uninstall` löschen diese Datei, aber nur, wenn sie noch die Zeile „Written by the Praxi installer“ enthält. Eine Datei, die du selbst unter diesem Namen angelegt hast, bleibt unberührt. journald wird nicht neu gestartet; die alte Begrenzung gilt daher ab dem nächsten Neustart nicht mehr (oder wenn du selbst `sudo systemctl restart systemd-journald` ausführst).

## 6. Logs

```bash
sudo journalctl -u praxi --since "1 hour ago"
sudo journalctl --disk-usage
```

Du willst eine Größenbegrenzung für das gesamte Journal? Das entscheidest du als Server-Besitzer: Trage `SystemMaxUse=500M` unter `[Journal]` selbst in eine Datei in `/etc/systemd/journald.conf.d/` ein. Praxi macht das nicht für dich.
