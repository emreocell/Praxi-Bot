# Connect your apps

An **app** lets your bot work with another service, such as Gmail, Notion, Linear or your files. Without apps, a bot can only chat and use its basic tools.

Some apps are called **MCP servers**. That is only a technical name for "a plug-in that gives a bot new tools". You do not need to know more than this.

> **Pictures:** lines that start with "Picture:" describe a screenshot that will be added later.

## Connect an app

1. Click **Apps** in the left rail.
2. Use **Search apps** or the category chips to find the app. Apps you can add are under **Discover**.
3. Press **Connect** on the app.
4. A page opens in your browser. Sign in there and allow access. Then go back to Praxi.
5. The app moves to **Connected**.

For some apps, Praxi first asks you to sign in once to a free service called Composio. After that it opens the app's own page by itself. You see "Step 1 of 2" and "Step 2 of 2".

> **Picture:** the **Apps** window with **Search apps**, **Discover** and **Connected**.

## Who can use the app

Press **Manage** on a connected app. Under **Who can use this app** choose:

- **All bots**, or
- **Only these bots**, and tick the bots. Then press **Save**.

If you tick no bot, **no bot can use the app**. Give each app only to the bots that need it.

## What each permission means

| What you see | What it means in plain words |
| --- | --- |
| **Who can use this app** | Which of your bots may use the app at all. |
| **Tools bots may use** | The list of things the app can do. You can turn single tools off. |
| **changes data** | This tool can change things in the app, for example edit a page. |
| **can delete or send** | This tool can delete things or send things out, for example send an email. |
| **Folder the bot may read and write** | Some file apps ask for one folder. Only that folder is shared. Praxi's own data and your password or sign-in folders can never be shared. |
| **Sign out** | Forget the sign-in. The app stays in the list and shows **Sign-in required**. |
| **Disconnect** | Remove the app. Praxi asks "Disconnect this server?" first. |

Safe default: keep **changes data** and **can delete or send** tools off until you need them.

## You still approve risky steps

Connecting an app does not skip approvals. When a bot wants to use an app to change or send something, you see an **Approval needed** card. For apps through Composio it says "Approve only this action with all arguments shown above". Press **Show all arguments** to read everything before you decide. See [Keep your bot safe](keep-your-bot-safe.md).

## Data goes to the app's service

When your bot uses an app, the data it sends goes to that service. With Composio apps it passes through Composio's servers. Connect only services you trust.

## When an app changes

An app can change how it describes one of its tools. If that happens after you approved it, Praxi switches the tool off. You see **A tool changed after you approved it** with **What you approved** and **What it says now**.

- Read both texts.
- Press **Trust the new version** only if you trust the app and the new wording looks right.
- If you are unsure, do nothing. The tool stays off and the bot cannot use it.

## Status words

| Word | What to do |
| --- | --- |
| **Connected** | Nothing. It works. |
| **Connecting** | Wait a few seconds. |
| **Sign-in required** | Press **Sign in**. |
| **Disconnected** | Press **Reconnect** under **Manage**. |
| **Error** | Open the details under the row. Fix what they say, then press **Reconnect**. |
| **Quarantined** | The app failed several times and was paused. Fix the problem, then press **Reconnect**. |

More: [Connecting apps](apps.md) (full details) and [MCP servers](mcp-servers.md) (for people who set them up by hand).
