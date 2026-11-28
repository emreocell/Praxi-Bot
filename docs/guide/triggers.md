# When something happens

Open a bot's panel, choose **Automation**, then **When something happens**.
Add a rule for new mail, a signed incoming call, a new item in a feed, a
change on a web page, a calendar event about to start, a Home Assistant device
changing state, or a time schedule. Write what
the bot should do, such as “Prepare a reply for me to review.” Save it, review
the rule, and turn it on. New rules always start off.

Existing mail schedules, prompt schedules and signed addresses appear in this same list. Open one to add quiet hours and limits; saving pauses it until you turn the reviewed rule on. Advanced workflow, precheck and file-watch schedules remain in Schedules.

For mail, connect an IMAP mailbox first and allow the bot's scheduled mailbox
reads. Choose a folder, sender (an exact address, or a domain that also covers its subdomains; never a
part of the text), and optional
subject or attachment filter. Checks run every two minutes by default, never
more often than once a minute. The first check establishes a starting point;
old mail does not start runs. Bursts become one run with a list of headers.
The connector's newest-100 search window is not a guaranteed delivery queue.
AgentMail cannot currently supply the complete loop-guard headers this feature
requires, so the builder offers IMAP mailboxes only. Attachment matching requires
the server to return MIME structure metadata; unknown metadata never matches.

Mail from the mailbox's own address, automatic replies, bulk/list mail and
mail with a list-unsubscribe header are ignored. No-reply addresses are ignored
unless the sender filter names them. Sender filters do not authenticate senders.
The existing e-mail conversation channel is independent: enabling both for the
same mailbox can produce both a conversation and a rule run.

For an incoming call, create a signed address under **Signed addresses**, then
select it in the rule. Signature, replay and incoming-call limits still apply.
Once linked, that address queues events for this rule instead of starting its
original run. The rule's instruction takes over. Turning off a linked rule
does not restore the original webhook instruction; calls are refused until it
is enabled or the rule is removed.

## Feeds, web pages, calendar events and Home Assistant

These four rules work like mail: the first check only notes what already
exists, so old items never start a run, and a burst becomes one run. They share
the quiet hours, limits, queue, **Test with a sample** and approval behaviour
below. The builder shows each one only for you: a bot cannot create them, and
the schedule API and tools refuse them.

**A feed has something new.** Give an RSS or Atom address. A new item starts the
bot with its title, link, date and a short summary (not the full article). The
default check is every 30 minutes; the fastest allowed is every 15.

**A web page changes.** Give the address and a CSS selector for the part to
watch, such as `#price` or `h1.title`. The bot receives the selected text when
it differs from the last check (at most 4,000 characters). Pages whose selected
text always changes, such as a clock, would start a run at every check; pick a
steadier part. If the selector stops finding anything, or the page cannot be
read, the check fails instead of reporting “the text became empty”; ten failed
checks in a row pause the rule. Default every hour, at most every 15 minutes.

Both fetch only the address you saved, never a link found inside the data.
Requests go out with the same web guard as the bot's browsing tools: only
`http` and `https`, never to this computer, your local network, link-local or
cloud-metadata addresses (checked on every redirect and every resolved
address), at most three redirects, a 15-second limit, and 1 MB for a feed or 2
MB for a page. The address cannot carry a user name or password. The bot must
be allowed to use the web at all: safe mode, a locked or denied web category,
or a profile without it stops the rule. Saving the rule with the address is
your approval of exactly that address; the web is not asked about again for
each check.

**A calendar event is about to start.** Choose a calendar and how many minutes
before (1 to 1,440). The rule needs a connected calendar the bot may read,
allowed for scheduled runs the same way as mailbox reads. It checks about every
two minutes, so the run starts up to a couple of minutes later than the time
you chose, once per event. An event you move to another time counts as a new
one. Events already inside the window when you turn the rule on, all-day
events and events marked free do not start it. The bot receives the title,
start, minutes until start and place, never the description or attendees.

**A Home Assistant device changes.** Enter the entity (for example
`binary_sensor.front_door`) and, if you like, the state it must become (`on`,
`unlocked`, …). Home Assistant must be connected and this bot allowed to use
it; otherwise the rule cannot be saved. It checks about once a minute and
compares the state only, not attributes such as a brightness or a reading, so
changes that flip and flip back within a minute can be missed. The bot receives
the entity, its name, the new state and the previous one.

Event text from all four is written by someone else. It is shortened, stripped
of control and invisible characters, and handed over as untrusted data like
every other event. Ten checks in a row that fail pause the rule and show
“Needs attention”.

Quiet hours use the time zone you choose, including daylight-saving changes.
You can skip events or save them for one summary after the window. By default
a rule may start 10 runs per rolling hour and 50 per rolling day; its bot may
start 30 and 150 across all rules. The lowest configured bot ceiling applies
to every rule for that bot. Extra events wait and join the next summary. These
queues and counters survive a restart. “Today” counts use UTC.

Queues hold up to 100 events and 48 KB per rule. A full mail queue or a failed
run pauses the rule. Ten consecutive mailbox-check failures also pause it.
Check the activity, connection and filters before turning it on again. Editing
a rule discards its old queued events and restarts mailbox baselining; rate
counters are preserved. Delivery uses the scheduler's existing best-effort
crash boundary: a crash after a batch is reserved can lose that batch. It is
not exactly-once delivery. Keep critical mail in your mailbox.

**Test with a sample** shows the exact instruction and wrapped event text the
bot receives, with the same size cap. It uses no model and runs no tools.
Mail supplies headers only; message bodies and attachments are never fetched
by the rule. Incoming call text is limited to a 4,096-character snippet and
the final event block to 12 KB. Invisible characters are removed from headers.
Event text is untrusted data, even if it asks the bot to ignore your instruction.

Only the local owner can create, edit or enable these rules. The bot uses its
own permissions and normal approvals; external-event runs also keep the
scheduler's once-only approval floor. An action needing approval waits and
notifies you. Rules do not auto-approve anything. Credentials stay in the
mailbox or signed-address configuration, never in the rule.
