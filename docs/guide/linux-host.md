# Linux always-on host (virtual display, update, uninstall)

This page is the short, copy-paste version of running Praxi on a Linux server that stays on around the clock, for example a free ARM Ubuntu VM. The [Add a server wizard](servers.md) runs the same installer and shows the same messages; the long reference is [Running on a remote host](remote-host.md).

Run every command on the server, over SSH. Replace `v0.1.0` with the release you want.

## 1. Preview, then install

Always preview first. `--dry-run` changes nothing and needs no `sudo`:

```bash
bash ./install-linux.sh --dry-run --headless --version v0.1.0
```

Install (verify the installer first, as described in [remote-host.md](remote-host.md)):

```bash
sudo bash ./install-linux.sh --headless --version v0.1.0
```

`--headless` means: always-on host mode (`bot serve --host`, listens on `127.0.0.1` only, no open port) plus a **virtual display** (Xvfb) and a Chromium browser, so bots can browse and use a desktop with nobody logged in. The display is `1920x1080x24` on `:99`. Change it with `--screen`:

```bash
sudo bash ./install-linux.sh --headless --screen 1280x720x24 --version v0.1.0
```

## 2. What gets installed

| Part | Details |
| --- | --- |
| `praxi.service` | Restarts after a crash (after 5 seconds, at most 10 times in 10 minutes), sandboxed: no new privileges, read-only system, private temp folder. |
| `praxi-xvfb.service` | The virtual display, no network access, private access cookie. |
| Logs | Go to the system journal. Praxi's units are rate-limited (at most 2000 lines per 30 seconds each), so a runaway bot cannot flood it. The installer does **not** change the size limit of the system journal and never restarts journald, because other services on the same server share it. Read Praxi's logs with `journalctl -u praxi -f`. |
| Data | `/var/lib/praxi` (your bots, files, secrets). Settings in `/etc/praxi`. |

## 3. Check everything

```bash
sudo praxi doctor
```

The doctor checks the display, the browser, free disk, free memory, the clock and the port, and every problem comes with the exact command to run. The most common ones:

| Doctor says | Run |
| --- | --- |
| display does not answer | `sudo systemctl restart praxi-xvfb` then `sudo systemctl status praxi-xvfb` |
| no browser found | `sudo bash ./install-linux.sh --update --with-desktop` |
| disk space low | `sudo journalctl --vacuum-size=200M` and `sudo apt-get clean` |
| clock is off | `sudo timedatectl set-ntp true` |
| port in use | `sudo ss -ltnp 'sport = :47821'` shows who uses it; use `--port` to pick another |

## 4. Update (keeps your data)

```bash
bash ./install-linux.sh --dry-run --update --version v0.1.1
sudo bash ./install-linux.sh --update --version v0.1.1
```

`--update` replaces the program only. Your bots, files, secrets, display size and host mode stay. If Praxi is not installed yet, it stops with a clear message instead of installing something new.

## 5. Uninstall

```bash
sudo bash ./install-linux.sh --uninstall          # removes Praxi, KEEPS your data
sudo bash ./install-linux.sh --uninstall --purge  # also deletes all data. Cannot be undone.
```

Preview either with `--dry-run`.

Older installers wrote a server-wide journal limit (`/etc/systemd/journald.conf.d/50-praxi.conf`). Both `--update` and `--uninstall` delete that file, but only if it still carries the "Written by the Praxi installer" line. A file you wrote yourself under that name is left alone. journald is not restarted, so the old limit stops applying at the next reboot (or when you run `sudo systemctl restart systemd-journald` yourself).

## 6. Logs

```bash
sudo journalctl -u praxi --since "1 hour ago"
sudo journalctl --disk-usage
```

Want a size limit for the whole journal? That is your decision as the server owner: put `SystemMaxUse=500M` under `[Journal]` in a file in `/etc/systemd/journald.conf.d/` yourself. Praxi does not do it for you.
