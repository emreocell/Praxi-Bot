# Memory: what your bots remember

In another language: [Türkçe](tr/memory.md) · [Deutsch](de/memory.md).

Praxi bots can remember useful things between chats, like your name, how you like answers written, or a project you keep coming back to. This page explains in plain words what is remembered, where it lives, and how to see, correct, forget and switch off.

## What is remembered

After a normal chat turn, the bot looks at what **you** said and may keep short notes such as "Lives in Berlin" or "Prefers short answers". Rules that are enforced in code, not just asked of the model:

* Only things you stated are kept, with the exact words you used as a quote. Questions, "what if" ideas and jokes are not kept.
* Passwords, API keys and anything that looks like a secret are never kept.
* Before an answer, the few notes that fit your message are placed in front of it as background. They are never used as instructions.

Not every note is treated the same:

* **Things you said** are saved and used right away (unless you chose "Ask before saving").
* **Things picked up from outside**, such as a web page, a file, an email, a tool result or another bot, are labelled *untrusted*. They wait in **Needs your OK** and are **not used until you approve them**.

A bot can also save or forget a note when you ask it to ("remember that my birthday is March 3"). It must quote your own message for that.

## Where it lives

Everything is stored on your computer in the Praxi data folder (the file `memory.db`), separate from your chats. The same file also holds a searchable copy of your past messages, for the bot's "search earlier chats" tool; a bot whose memory is **Off** stops adding to it. Nothing is sent to anyone to be remembered. Each bot sees your shared notes plus its own; other people's notes are never visible.

If you switch on **Smart** finding (below) with an online embedding provider, the text of your notes is sent to that provider to be turned into search data. That only happens after you tick an explicit agreement, and it is off by default.

## See, correct and forget

Open **Settings → Personal memory**, choose a bot, and you can:

* read every note, with its category, the bot it belongs to, when it was saved and the quote it came from;
* **Edit** a note (your edit counts as something you said), or **Forget** it;
* **Undo** the last change to a note;
* add a note yourself;
* see a **preview** of exactly what would be placed in front of your next message.

A note that is only true for a while shows **expires** with a date. When that time has passed the bot stops using it. When you say things like "until Friday", "this week" or "for 3 days", the bot sets this date itself; it understands English, German and Turkish phrases like these and does not guess dates it cannot read.

**Forget** hides a note and stops it being used. It stays in your undo history on this computer. To remove everything from disk, close Praxi and delete `memory.db` in the data folder; the bots then start with an empty memory.

## What a bot noted on its own

A bot that works without you (a scheduled run, a mission session, a sub-agent) cannot quote a message of yours, so it cannot save a memory the way it does in a chat. It can still keep what it **learned while working** (a site moved, a command that works). Such a note is kept apart from what you said: it belongs to that bot only, it never describes you, it records which run saved it and whether that run had read outside content, and the bot sees it in its own section, labelled as not confirmed by you. If the run had read outside content, it is **untrusted**: it waits in **Needs your OK** until you approve it, and is then quoted as data. Each bot keeps at most 200 of these. Editing the text of one in the memory settings makes it yours. See [Notes and memory tools](agent-tools.md).

## Needs your OK

Notes picked up from web pages, files, tools or other bots appear in the **Needs your OK** list at the top of the memory settings. **Approve** lets the bot use it (it is still shown to the model as quoted data, not as a command). **Discard** forgets it. Until you decide, the bot does not use it and it never appears in the preview.

## Switch it off, or ask first

Each bot has its own setting under **Edit bot → Advanced → Memory** (and at the top of **Settings → Personal memory**):

| Setting | What happens |
| --- | --- |
| **On** (default) | Learns from what you say. Things from outside wait for your OK. |
| **Ask before saving** | Every new note waits in **Needs your OK** first. |
| **Off** | The bot learns nothing, recalls nothing and has no memory tools. Existing notes stay stored but are not used. |

Under **Advanced**:

* **Memory read into one answer** sets how much background (about 150 to 1500 words, default 600) may be placed in front of a message. More gives more context but makes each answer a little slower and costlier.
* **Share what I learn with my other bots** (default on) shares facts about you and past events with all your bots. Turn it off and this bot keeps everything it learns to itself. Procedures and lessons always stay with the bot that learned them.

Only you, on the computer that runs the bot, can change these settings; a paired phone or another computer cannot. **Safe mode** keeps memory off entirely.

## How notes are found: Basic or Smart

* **Basic (word match)** is the default. A note is found when it shares words with your message ("tea" finds "likes green tea"). It needs no model and sends nothing anywhere. It does not understand that "car" and "automobile" mean the same.
* **Smart (meaning)** uses an embedding model you already set up for a bot's knowledge (see [Knowledge embeddings](knowledge-embeddings.md)), so "car" can find "drives an automobile". Switching on re-indexes your notes in the background and shows progress; until it is done, Basic is used. If the model fails, Praxi falls back to Basic and nothing is lost.

Use **Settings → Personal memory → How memories are found** to switch.
