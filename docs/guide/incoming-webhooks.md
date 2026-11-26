# Incoming webhooks

A webhook starts one of your bots when something outside calls an address. GitHub, a form, or a switch at home can do that. You create the webhook in the bot panel, on the **Automation** tab.

Praxi does not open a new port. The address is on the daemon that already listens at `127.0.0.1`. To let the outside world reach it, point a tunnel you choose at that address. Cloudflare Tunnel and `ssh -L` are two ways. Do not forward a new port on your router. A paired phone or another computer cannot create, test, or call these webhooks.

## What you set up

1. Open the bot and choose **Automation**.
2. Give the webhook a short name. Optionally write what the bot should do with the data. If you leave that blank, the bot is told to summarize the data and not to follow orders inside it.
3. Press **Create webhook**.
4. Copy the secret now. Later, the list shows the address and the name, never the secret.
5. Turn a webhook **Off** when you want calls to stop. **Remove** deletes it. **Send a test** starts one run from the app.

## What the caller sends

`POST` the raw body to `/v1/incoming-webhooks/{id}`.

Two headers are required:

- `X-Webhook-Timestamp`: the current time in Unix milliseconds.
- `X-Signature-256`: `sha256=` followed by the hex HMAC-SHA256 of `{timestamp}.{raw body}`, using the secret. `X-Hub-Signature-256` is accepted with the same value.

A signature of the body alone is not enough. The time must be within 5 minutes, and that exact signature works once. A second copy is refused, even after Praxi restarts. The body can be at most 64 KB. More than 30 accepted calls in a minute are refused. A webhook that is off does not count toward that limit. Failed calls are limited too: after 10 failures in a minute for one address (or 60 across all addresses) Praxi answers 429 without looking anything up. A wrong address answers exactly like a wrong signature, so an address cannot be guessed. Only the first few refused calls each minute are written to the audit log; the rest are counted.

## What the bot does with the text

The body is data, never an instruction. Your instruction is the only task the bot is given. The body is shown to the model as a quoted, labelled data block (cut to the first 4096 bytes, even though up to 64 KB is accepted), marks the run as having read untrusted content, and is never a message from you. Praxi does not turn a line such as `call:something` into a tool. The run uses the bot's own permission level. If a tool needs your OK, the question is queued as a notification, the same way a scheduled job asks.

The test button sends a tiny payload, `{"praxi_test":true}`, through the same path. It does not spend the outside rate limit.

## Where the secret is kept, and replacing it

The signing secret is kept in the same protected secret store as your other secrets, never in the local database. Webhooks made by older versions had their secret in the database: the first start of this version moves it into the secret store and blanks the database copy, and the sender that already holds that secret keeps working. Nothing changes for it.

To replace a secret (it leaked, a colleague left, or you imported the webhook on a new computer), open the webhook in the Automation tab and choose **Replace secret**. The old secret stops working at once, so whatever calls the webhook (GitHub, a form, a script) must be updated with the new secret, which is shown once. Until then its calls are refused with `401`. The same action is `POST /v1/bots/{bot}/incoming-webhooks/{hook}/rotate-secret` (owner only).

If the secret store cannot be reached, creating or replacing a webhook fails with `503` and calls to existing webhooks are refused; no secret is ever written to the database as a fallback.

## What is written down

Creating, editing, replacing the secret, deleting, testing, accepting, and refusing a call are written to the audit log. The log stores the reason (bad signature, old timestamp, replay, rate limit, turned off, or safe mode). It does not store the secret or the body.

## Moving to another computer

Webhook secrets do not travel in a state bundle. A normal export blanks them (and the list of already-seen signatures); only when you choose to export all secrets do the secret bytes stay in the file. Either way, on import every webhook comes back turned off with no secret, and so do webhook triggers of scheduled jobs. Create the webhook again (or turn it on with a new secret) on the new computer.
