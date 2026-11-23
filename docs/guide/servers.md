# Adding a server

A **server** is another computer that stays on day and night, usually rented online. Your bots can live there, so they keep working when your own computer is off.

**You do not need one.** Your bots already run on this computer. Add a server only if you want them to work around the clock.

Open the computer menu at the top and choose **Add a server**. The first screen explains this in two sentences and offers three big choices. You can go back or close the window at any step; when you open it again it continues where you left off (it remembers only the page, never an address, a key or a code).

## The three choices

- **Run on this computer (recommended to start).** Nothing to set up.
- **I have a server.** One command, no key files: see below.
- **I need a server.** A plain-words guide with a checklist for three options: the Oracle Cloud free tier, a rented server from any provider, or a spare computer at home. You do everything on the provider's own website or on that computer. Praxi never sees your account, password or card.

## I have a server: one command

You do not need SSH, a key file or a terminal on your own computer.

1. Open your server's **console or terminal**. Cloud providers have a button named Console, Web console or Terminal on the server's page. On a computer at home, open the Terminal app.
2. In Praxi, press **Copy command** and paste it there. The command:
   - makes sure the GitHub CLI (2.80 or newer) is installed, because it checks signatures;
   - downloads the installer for **exactly the release this app was built from**;
   - **verifies its signed build provenance** (the same check as in [Linux always-on host](linux-host.md) and [Release](release-linux.md)); if the check fails, nothing is installed;
   - installs the always-on preset with a desktop session, and
   - prints **one pairing code** that starts with `bot-pair:`.
3. Paste the code into Praxi (you can paste everything the console printed; Praxi finds the code) and press **Connect**.

The command contains no password, key or token of any kind. Only the release number is filled in. The pairing code is single-use, valid for 30 minutes, and can only pair one desktop; a wrong code is refused and an unused one expires on its own. Run the command again at any time to get a new code: it is safe to repeat.

Then Praxi shows four live checks, each with a plain fix when something is off:

| Check | Meaning | If it is not green |
| --- | --- | --- |
| The server answers | This app can talk to it | Make sure the server is on and online; get a new code if the old one was used |
| Same Praxi version | Server and app run the same release | Run the command again on the server |
| A screen to watch | The server has a desktop you can watch | Run the command again to add it; bots work without it |
| Models can be sent | This app may send model keys to the server | On the server: `bot host admin --all-desktops` |

Only a server that cannot be reached blocks you; the other three are advice.

If you already have a pairing ticket (for example from `bot host invite --role desktop` on the server), use **I have a pairing ticket instead** at the bottom of the window.

### On the server (for administrators)

The command is the script `crates/host-provision/scripts/bootstrap.sh` with the release tag filled in, run through `bash -s`. It uses `bot host invite --role desktop --ttl-secs 1800 --ticket-only`, which prints only the ticket. Both options also work by hand. A build that is not an official release shows a warning, because its release may not be published.

## I need a server

Pick a card. Each guide is a short checklist of plain steps and ends with "My server is ready", which leads to the one-command page above.

### Oracle Cloud free tier, in short

1. Create a free account at oracle.com/cloud/free (a card proves you are a person; the free tier is not charged).
2. In the console choose Compute, then Create instance. Pick Ubuntu 22.04 or newer and a shape marked Always Free.
3. Leave the network rules as they are.
4. When the instance is Running, open its **Console connection** or Cloud Shell and paste the command from Praxi.

### A rented server

Any provider with Ubuntu 22.04 or newer works (about 2 GB of memory and 10 GB of disk). Create the server, open its web console and paste the command.

### A spare computer at home

Install Ubuntu 22.04 or newer, keep it on and online, open the Terminal app and paste the command.

## Advanced: connect over SSH

If you already sign in to your server with an SSH key file or agent, open **Advanced: connect over SSH** on the "I have a server" page. This is the older path and is never needed by beginners.

- **Server address**, for example `203.0.113.7`. Do not type `ssh://` or `user@`.
- **User**: `ubuntu` on Ubuntu images, `opc` on Oracle Linux.
- **Key file** or your SSH agent. The key files found on this computer are listed under **Keys found on this computer**; none is chosen for you, and only the path is used. The key is never copied or stored. Praxi does not use passwords.
- **Advanced**: the port (almost always 22).

Praxi shows a checklist (the server answers, signing in works, the system is supported, enough memory and disk, network and ports), asks you once to confirm the server's fingerprint, installs Praxi if needed and pairs.

Common messages:

| Message | What it means | Fix |
| --- | --- | --- |
| The server did not accept your key | "permission denied (publickey)" | Check the user name, pick the key that belongs to this server |
| Your key file is readable by other people | The key file is too open | Mac/Linux: `chmod 600 <key file>`. Windows: Properties, Security, leave only your own user |
| This key is locked with a passphrase | Praxi cannot type it | Unlock it with `ssh-add` and choose SSH agent |
| The server did not answer in time | Port 22 is probably blocked | Allow incoming TCP on port 22 in your provider's firewall |
| The server said no to the connection | The SSH service is not running | `sudo systemctl start ssh` |

## Your servers

Computer menu, **Your servers**, lists every server with its state. **Remove** forgets the server on this computer only. The server itself, its bots and its files are not touched and keep running; you can add it again later.

## Updating a server

**Your servers** (and the Computer menu while the server is selected) shows each server's version next to this app's. When the server is older you see **Update available**; **Check again** asks the server once more. If Terminal or another feature says the server runs an older Praxi-Bot, the same **Update server** button is offered there.

**Update server** first says what will happen: the server restarts for a few seconds, bots running there pause and carry on, your data and settings are kept, and the previous version stays on the server (`/opt/praxi.backup-<time>`, the newest one only). Then it connects with the login details saved when you added the server (nothing new is stored), takes a copy of the data, and runs the same verified update as `bot host upgrade`: only the release that matches this app is installed, after its signature is checked. If anything fails, the installer puts the previous version back and the app tells you in plain words what happened and what to do. **Show details** lists every step.

For a server that was added another way (no saved login on this computer), or when this computer cannot use SSH, the app shows the exact commands to paste on the server instead: the verified bootstrap from [Releasing the Linux server packages](release-linux.md) with `--update --version <tag>`. Only the computer's owner can start an update; a paired phone or computer cannot.

## Limits today

- Installing Praxi needs a published release of the same version as your app; a development build shows a warning.
- The server needs outgoing internet access to github.com, systemd, and Ubuntu 22.04 or newer (or a recent Oracle Linux / Fedora family system).
- The update needs the saved SSH login of the "Add server" wizard; there is no login-free update yet. Without it, paste the update command on the server (see above).
