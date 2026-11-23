# Fix common problems

Find the message you see. Do the steps in order. The message titles below are the words the app shows.

If nothing helps: open **Help** from the account menu, press **Show what will be copied**, read it, then press **Copy**. Keys, passwords and e-mail addresses are masked. Nothing is sent anywhere. Paste the text into a bug report. See also the technical [Troubleshooting](troubleshooting.md) page.

## 1. "Praxi's local service stopped" or "Praxi cannot reach its local service"

The background part of Praxi is not running.

1. Wait 10 seconds. It may still be starting ("Praxi's local service is starting"). Praxi tries again by itself.
2. Press **Restart the local service** (or **Restart service**).
3. Still no? Close Praxi completely and open it again.

Your bots and chats are safe.

## 2. "Windows blocked Praxi's service" or "a Windows component is missing"

Your antivirus or Windows refused to start the service, or a Windows part is missing.

1. Blocked: open your antivirus or SmartScreen and allow `bot.exe` in Praxi's folder. Then press **Try again**.
2. Component missing: install the "Microsoft Visual C++ Redistributable (x64)" from Microsoft. Then press **Try again**.
3. "Another program is using the connection Praxi needs": close that program or restart the computer.
4. If it repeats: press **Copy details** and send them. **Open log folder** shows the log.

## 3. The bot does not answer, or "The model could not answer"

The AI service did not give an answer.

1. Read the details under the message. They say what the service reported.
2. Press **Try again**.
3. Open **Settings → Bots & models** and check that your key is there. If not, add it again.
4. Pick another model and try again.

## 4. "did not accept this key"

The key was not copied completely, or it is for another service.

1. Go back to the key page and copy the key again. Copy the whole line.
2. Paste it again. The box checks it right away.
3. Make sure you chose the right service (for example Google Gemini for a Gemini key).
4. If you use a coding tool as the brain: "You are signed out of that coding tool" means you must sign in to it in **Settings**. "That subscription has reached its usage limit" means wait until it resets or pick another model.

## 5. "Could not reach …" or "That took too long"

Your internet connection is the likely cause.

1. Check that your internet works in a browser.
2. Press **Try again**.
3. If you use a VPN or a company network, switch it off for a moment and try again.

If a message did not go through ("Your message was not sent"), check the connection and send it again.

## 6. "You did not allow that step"

You pressed **Deny**, or the card timed out and the answer became no.

1. Ask the bot again.
2. When the card **Approval needed** appears, read it and press **Allow once**.
3. Do not want to be asked each time for something you trust? Use **Always** on the card. See [Keep your bot safe](keep-your-bot-safe.md).

## 7. "The bot could not find the file or program", or "… is not installed"

1. Ask again and say the file's full name or folder.
2. If a program is named in the message, install that program. Then ask again.
3. If the bot was told to use a folder it may not touch, add it as a trusted folder: **Settings → Safety & permissions**.

## 8. "Safe mode is on"

Safe mode switches off tools, workflows and schedules. It is for fixing problems.

1. Close Praxi.
2. Open it normally (not in safe mode).

Everything comes back. Nothing was deleted.

## 9. "… did not answer" or "The bot could not reach the target computer"

A server or another computer is off or not connected.

1. Turn it on and check its network.
2. Wait a minute and try again.
3. Do you not need that computer? Pick **This computer** in the computer menu.

More: [Run it on this computer or on a server](local-or-server.md).

## 10. "Bot limit reached" or "This bot is close to its limit"

You set a spending limit and the bot used it. The AI service was not called.

1. Open the bot's settings and raise the limit, or wait until it resets.
2. Or leave the run stopped.

See [Bot spending limits](bot-spending-limits.md).

## 11. An app shows "Error", "Sign-in required" or "Quarantined"

1. Open **Apps**, find the app and press **Manage**.
2. **Sign-in required**: press **Sign in**.
3. **Error**: open the details under the row and do what they say.
4. Then press **Reconnect**.
5. "Composio is not set up yet": press **Connect** on an app and sign in once.
6. "A tool changed after you approved it": read both texts. Press **Trust the new version** only if you trust the app.

See [Connect your apps](connect-your-apps.md).

## 12. A scheduled job did not run, or "That schedule is not valid"

1. Jobs run only while Praxi is open and the computer is awake. If it was off, the job shows as missed. Press **Retry**.
2. Open **Scheduled tasks** and check that the job is not paused. The **Next 5 runs** list shows when it will run.
3. "That schedule is not valid": check the time or the repeat rule. Use the **Easy** tab instead of typing words.
4. Is Safe mode on? See problem 8.
5. The job may be waiting for your OK. Look in **Needs you**.

See [Automate things](automate.md).
