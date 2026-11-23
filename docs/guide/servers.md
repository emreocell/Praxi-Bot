# Adding a server

A **server** is another computer that stays on day and night, usually rented online. Your bots can live there, so they keep working when your own computer is off.

**You do not need one.** Your bots already run on this computer. Add a server only if you want them to work around the clock.

Open the computer menu at the top and choose **Add a server**. The wizard has three short steps.

## Step 1: choose what fits you

- **I already have a server.** You know its address and have a key file to sign in (SSH).
- **Get a free always-on server.** A text guide for the Oracle Cloud free tier. You do everything on Oracle's own website; Praxi never sees your Oracle account, password or card.
- **Use another computer at home.** An old PC or mini PC that stays on, ideally running Ubuntu 22.04 or newer with `openssh-server`.

### Oracle Cloud free tier, in short

1. Create a free account at oracle.com/cloud/free (a card is needed to prove you are a person; the free tier is not charged).
2. In the console choose Compute, then Create instance. Pick Ubuntu 22.04 or newer and a shape marked Always Free.
3. Choose Generate a key pair and download the private key. Keep that file safe.
4. Make sure incoming TCP on port 22 is allowed (the default network allows it).
5. When the instance is Running, copy its Public IP. The user name is `ubuntu`.

## Step 2: what Praxi needs

- **Server address**, for example `203.0.113.7`. Do not type `ssh://` or `user@`.
- **User**, `ubuntu` on Ubuntu images, `opc` on Oracle Linux.
- **Key file** (or your SSH agent). Only the path is used; the key is never copied or stored. Praxi does not use passwords, so no password is typed, saved or sent. If your provider only gave you a password, create a key with your provider's help page first.
- **Advanced**: the port (almost always 22).

Typing mistakes are explained as you type (a `.pub` file instead of the private key, a `:port` glued to the address, and so on).

## Step 3: checks, install, done

Praxi shows a checklist: the server answers, signing in works, the system is supported, enough memory and disk, network and ports. A problem shows what to do next. The first time you connect, you confirm the server's fingerprint; compare it with your provider's console if it shows one.

A progress bar and an expandable technical log show the install. **Try again from here** repeats the run and skips what is already in place; **Cancel** lets the running step finish first.

When it is done, pick the server in the computer menu to chat with its bots, watch its screen and open its terminal.

## Your servers

Computer menu, **Your servers**, lists every server with its state. **Remove** forgets the server on this computer only. The server itself, its bots and its files are not touched and keep running; you can add it again later.

## Updating a server

**Your servers** (and the Computer menu while the server is selected) shows each server's version next to this app's. When the server is older you see **Update available**; **Check again** asks the server once more. If Terminal or another feature says the server runs an older Praxi-Bot, the same **Update server** button is offered there.

**Update server** first says what will happen: the server restarts for a few seconds, bots running there pause and carry on, your data and settings are kept, and the previous version stays on the server (`/opt/praxi.backup-<time>`, the newest one only). Then it connects with the login details saved when you added the server (nothing new is stored), takes a copy of the data, and runs the same verified update as `bot host upgrade`: only the release that matches this app is installed, after its signature is checked. If anything fails, the installer puts the previous version back and the app tells you in plain words what happened and what to do. **Show details** lists every step.

For a server that was added another way (no saved login on this computer), or when this computer cannot use SSH, the app shows the exact commands to paste on the server instead: the verified bootstrap from [Releasing the Linux server packages](release-linux.md) with `--update --version <tag>`. Only the computer's owner can start an update; a paired phone or computer cannot.

## Fixing common problems

| Message | What it means | Fix |
| --- | --- | --- |
| The server did not accept your key | "permission denied (publickey)" | Check the user name, pick the key that belongs to this server, make sure it was added when the server was created. |
| Your key file is readable by other people | The key file is too open | Mac/Linux: `chmod 600 <key file>`. Windows: Properties, Security, leave only your own user. |
| This key is locked with a passphrase | Praxi cannot type it | Unlock it with `ssh-add` and choose SSH agent, or use a key without a passphrase. |
| The server did not answer in time | Port 22 is probably blocked | Allow incoming TCP on port 22 in your provider's firewall and use the public address. |
| The server said no to the connection | The SSH service is not running | `sudo systemctl start ssh`, or reinstall `openssh-server`. |

## Limits today

- Installing Praxi for you needs a published release; until then the server must already run Praxi.
- There is no clock-sync check yet.
- The update needs the saved SSH login of the "Add server" wizard; the server-side updater unit (`praxi-update.service`) is not installed by the installer yet, so there is no login-free update.
