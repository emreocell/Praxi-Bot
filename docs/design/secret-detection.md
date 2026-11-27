# Secret detection in the chat composer

Status: detector, corpus, composer card and the channel second check are implemented (issue #421).
The composer warning is in the desktop UI (`lib/secretDetect.ts`, `components/PastedSecretWarning.tsx`);
`lib/secretDetect.ts` is a TypeScript port of the Rust detector, and both test suites read the same
corpus fixture, so they cannot drift. Telegram and Discord messages get a daemon-side second check
(see [Other channels](#other-channels)). Not done: a Settings switch and "do not ask again for this
kind".

When someone pastes something that is very likely a credential into the chat box, the desktop app
should *offer* to keep it out of the chat and store it in the vault instead. Today a pasted value is
ordinary chat text: it goes to the model provider and into the chat history (see
[Secrets the assistant may type](../guide/security.md#secrets-the-assistant-may-type-fill-secrets)).

The rule is **offer, never act**. The detector never stores, removes or sends anything. It only
reports where it thinks a secret is.

## Rules

The detector reports a candidate only in the three cases below. Everything else is ignored.

### 1. Known shapes (high confidence)

A provider format with a fixed prefix and length, anchored at a word edge (`task-...` is not
`sk-...`; a longer alphanumeric run around it disqualifies the match).

| Kind | Shape | Source |
|---|---|---|
| `openai_style_key` | `sk-` + 16 or more of `[A-Za-z0-9_-]`, with at least one digit | `Redactor` |
| `google_api_key` | `AIza` + 30 or more of `[0-9A-Za-z_-]` | `Redactor` |
| `github_token` | `ghp_` / `gho_` / `ghu_` / `ghs_` / `ghr_` + 30 or more alphanumerics, `github_pat_...` | `Redactor` |
| `slack_token` | `xox[abprs]-` + 10 or more | `Redactor` |
| `nvidia_key` | `nvapi-` + 20 or more | `Redactor` |
| `jwt` | three base64url parts, the first two starting `eyJ` | `Redactor` |
| `aws_access_key_id` | `AKIA` or `ASIA` + 16 uppercase alphanumerics | detector only |
| `stripe_key` | `sk_live_` or `rk_live_` + 20 or more alphanumerics | detector only |
| `telegram_bot_token` | 8 to 10 digits, `:`, 35 of `[A-Za-z0-9_-]` (one uppercase at least) | detector only |
| `npm_token` | `npm_` + 36 alphanumerics | detector only |
| `gitlab_token` | `glpat-` + 20 or more | detector only |
| `huggingface_token` | `hf_` + 34 letters | detector only |
| `groq_key` | `gsk_` + 40 or more alphanumerics | detector only |
| `digitalocean_token` | `dop_v1_` + 64 lowercase hex | detector only |
| `private_key` | a `-----BEGIN ... PRIVATE KEY-----` block (to its `END` line, or to the end of the text when truncated) | detector only |

The shapes marked `Redactor` come from the one table in `crates/secret-store/src/redact.rs`, so the
composer and the output redactor cannot disagree about them. The "detector only" shapes are
documented, distinctive formats; promoting them to the `Redactor` is a separate decision because it
changes what is masked in tool output.

The `sk-` shape additionally needs a digit, because `sk-learn-is-a-fine-library` is a sentence, not a
key. New shapes are added only when the format is documented by the provider and distinctive.

### 1b. Card numbers (medium confidence)

A payment card number is reported only when **all** of these hold:

* 13 to 19 digits, Luhn-valid, not all the same digit;
* a real network prefix with the matching length (Visa, Mastercard incl. the 2-series, Amex,
  Discover, Diners, JCB, UnionPay, Troy);
* it is written the way people write a card, either in groups (`4111 1111 1111 1111`,
  `4111-1111-1111-1111`, Amex `3782 822463 10005`, one separator kind only), or as one run of digits
  right after a card word (`card`, `kart`, `kredi kartı`, `visa`, ...);
* it is not glued to a letter, `+`, `#`, `_` or a decimal point (`+4111...` is a phone number,
  `4111...50` after a `.` is an amount).

So an order number, a tracking number, a Unix time in milliseconds, an IBAN, a Turkish ID number or an
ISBN is not a card, even when it has 16 digits. A bare 16-digit run with no card word is not reported
even if it passes the Luhn check, because roughly one in ten random numbers does.

### 2. Labelled context (medium confidence)

A value directly after a label, with only separators and a few glue words in between
(`password: X`, `my api key is X`, `API_KEY="X"`, `Şifre: X`, `anahtarım şu: X`). The value is the
first token after the label; the detector does not search further.

Labels are matched case-insensitively with Turkish letters folded, so `ŞİFRE`, `Şifre` and `sifre`
are the same word. Turkish suffixes such as `şifrem`, `parolası`, `anahtarı` are accepted.

| Label class | English | Turkish |
|---|---|---|
| api key | `api key`, `api-key`, `api_key`, `apikey`, `access key` | `anahtar`, `api anahtarı`, `erişim anahtarı` |
| token | `token`, `bearer` | `jeton` |
| secret | `secret`, `client secret`, `secret key`, `private key` | `gizli anahtar` |
| password | `password`, `passwd`, `pwd`, `passphrase` | `şifre`, `sifre`, `parola` |

Environment-variable spellings (`OPENAI_API_KEY=`, `DB_PASSWORD=`) work because `_` separates words.
A placeholder `[secret: name]` is never a candidate.

The value must then pass a check that depends on the label class.

**Machine values (api key, token, secret):**

* 16 to 512 characters from `[A-Za-z0-9_.+/=~-]`;
* at least one digit and at least one letter;
* not a URL or a path (`://`, leading `/ ~ . \`, two or more `/`), not a version string
  (two or more dots between short segments);
* at least 8 distinct characters and Shannon entropy of at least 3.0 bits per character;
* it must look random, not like words: mixed upper and lower case, or at least three
  letter/digit changes. `my-long-token-name-2024` and `internationalization2024` fail this.

**Passwords** are chosen by people and are shorter, so the checks differ:

* 8 to 128 characters, at least one letter;
* at least one digit or one "strong" symbol (anything except `. _ -`), at least two character
  classes among lower, upper, digit and symbol, and at least 5 distinct characters;
* not a placeholder or reference (`<...>`, `[...]`, `{...}`, `${...}`, `%...%`, `*...`), not a URL
  and not code (`(`).

### 3. Entropy alone is never enough

Length, character mix and entropy are only *conditions on a labelled value*. Text with no known
shape and no label is never reported, however random it looks. That is why `Ajzashkjdgasda`,
`sjdhfjksdh`, `asdfghjkl`, UUIDs, git hashes and base64 image data are never reported: they carry
neither a provider prefix nor a label. A key that merely starts with `AJz` is not detected either.

### Thresholds and why

| Threshold | Value | Reasoning |
|---|---|---|
| Machine value length | at least 16 | Provider keys are 20 characters or more; shorter labelled tokens are usually ids, ports or words. |
| Entropy | at least 3.0 bits/char | A 16-character random base62 string scores about 3.7 to 4.0; repeated characters and plain words score lower. It supports a label, never replaces one. |
| Distinct characters | at least 8 (machine), 5 (password) | Rejects `aaaa1111aaaa1111` and `abababab1!`. |
| Password length | at least 8 | Below that, a labelled word is far more often prose (`parola: kedi`) than a password. |
| Gap between label and value | at most 24 characters, separators and glue words only | A label far from a value is a different sentence. |
| One value per label | first token only | `token limit is 4096` must not look further than `limit`. |

The thresholds trade recall for precision on purpose. Known misses, accepted in this version: a
short or purely numeric password (`password: 12345678`), a password with no label, a dictionary
passphrase, a value on the line after its label, and labels in languages other than English and
Turkish. A detector that guesses gets ignored or turned off.

## Output and the placeholder

The detector returns candidates sorted by position, without overlaps. A known shape wins over a
labelled match on the same bytes. Each candidate has:

* `kind` (stable `snake_case` ids for counters; the UI localises its own wording),
* `confidence` (`High` for shapes, `Medium` for labelled),
* `start` and `end`, byte offsets into the scanned string.

A candidate **does not contain the value**, and `Debug` and `Display` print only the kind and the
confidence. The caller reads the value from its own text with `slice(text)` and must treat it as a
secret from that moment.

When the owner chooses "Store in vault as ...", the composer replaces the span with a placeholder
and sends the message with it:

    [secret: login/openai]

`placeholder(name)` builds it and refuses names that are not valid secret names (the vault rules:
`[A-Za-z0-9._:/-]`, at most 128 bytes, no `__` prefix). The placeholder is plain text; it carries a
name, never a value, and the detector never reports one as a secret.

## Privacy

* The detector runs **locally**, in the desktop composer, **before** send. No network, no lookups to
  validate a key, no telemetry.
* It never writes the matched text anywhere: no logging, no events, no error messages. Only
  counters (how many candidates of which `kind`) may be recorded, if at all.
* The result types hold no value, so printing one cannot leak it. A test asserts this.
* The existing `Redactor` keeps masking known shapes in tool output, events and logs whether or not
  this feature exists. A miss by the detector is therefore no worse than today.
* Other channels cannot show a card; see [Other channels](#other-channels).

## Other channels

Telegram and Discord cannot show the card, and a message from there reaches the daemon without the
owner having seen a choice. So the adapters (`channel-telegram`, `channel-discord`) run the same
detector once more, on the daemon side, with `secret_store::screen_inbound`, at the single point where
a text enters the adapter, **before** anything else holds it: before the inbox file, the daemon (so
the model, the chat history, the event log and the run log), a command argument or a log line:

* Telegram: `handle_updates`, ahead of `Inbox::enqueue` (the durable inbox writes each update to
  disk, and keeps completed payloads). The text in the stored update *is* the redacted text, and a
  flag on the update (`secret_cut`, set only there, never trusted from the wire) carries "tell the
  owner" through retries and restarts. A voice transcript is cut before it is echoed back.
* Discord: `dispatch`, before `on_text` (which also runs `!answer` and `!use`). The original string is
  consumed there; only the redacted one is passed on.
* each suspected value is replaced by `[REDACTED]` and the rest of the message goes on;
* when nothing but secrets was in the message (a bare key), nothing is passed on. "Nothing but" is
  decided from the text outside the detected spans (no letter or digit there), not from the
  redacted string, so a `[REDACTED]` the sender typed counts as a word;
* the owner gets a reply in the chat, in their language (English or Turkish; Discord also German),
  saying a secret was cut out and pointing at the vault in the desktop app. The reply names no kind
  and never repeats the value. A retried Telegram update does not repeat it.

What this cannot do: the original message stays in the Telegram or Discord chat (and with that
service), so the reply asks the owner to delete it. A message a paired device sends through the daemon
API is not screened by this second check.

## Limits

The detector runs on text a person pasted, so it must stay cheap on hostile input:

* **Windows.** `detect_secrets` reads a text in windows of 64 KiB (`SCAN_WINDOW`), each with 4 KiB of
  the neighbouring text on both sides (`SCAN_CONTEXT`) so a value or its label across an edge is seen
  whole; a candidate belongs to the window that holds its first byte. No part of a long text goes
  unscanned. The TypeScript port does the same in UTF-16 units. A private-key block is found over the
  whole text because it can be long.
* **Labelled values.** A value is read for at most 520 characters and the gap after a label for at
  most 24, so `token:token:token:...` does not make every label rescan the text.
* **Regexes.** Rust uses the `regex` crate (automata, linear time, no backtracking). The TypeScript
  shapes are the ones a backtracking engine could be made to rescan: the JWT segments are bounded
  (`{8,512}`, `{8,8192}`, `{8,2048}`); the others are a literal prefix, one character class run and a
  one-character lookahead, which cannot backtrack across a run. There is no nested quantifier.
* **Overlaps** between known shapes and labelled values are found by binary search, not by comparing
  every pair.
* **Callers.** `redact_candidates` never slices on a stale or mid-character span (it skips it).

## Out of scope

Storing anything automatically, scanning old chat history, a password with no label and no shape,
validating a key against a provider, telemetry, and deleting the original message in Telegram or
Discord.

## How the composer calls it

The Rust detector lives in the `secret-store` crate (`detect_secrets(&str) -> Vec<SecretCandidate>`)
and the channels use it directly. The composer does not call the daemon: the draft would have to leave
the composer (and, with a paired host, the computer) before the owner has chosen anything. Instead
`apps/desktop/ui/src/lib/secretDetect.ts` is a port that runs in the page. On send, the composer:

1. calls the detector on the draft (also for slash text and voice text);
2. if there are candidates, shows a card naming the kind (never the value) with *Store in vault as
   ...*, *Remove it from the message*, *Send anyway* and *Keep editing*;
3. on "Store", writes the value through the vault route under the name the owner picks, replaces the
   span with `[secret: name]`, and does not send yet.

"Send anyway" is a single-use argument of that one send, never a stored flag. Nothing is sent, stored
or removed without a click.

## Tests

`crates/secret-store/tests/secret_detection_corpus.rs` holds a positive corpus and a negative one.
The provider-shape, labelled, card, negative and stress rows live in `crates/secret-store/tests/fixtures/detect-corpus.json`,
which `apps/desktop/ui/src/lib/secretDetect.test.ts` reads too: a row added there is checked against
the Rust detector and its TypeScript port.

* Positive: every known shape with fake values assembled at run time (so no secret-shaped literal
  sits in the source and trips secret scanners), labelled cases in English and Turkish, and card
  numbers (public test numbers) in groups and after a card word. Each is checked for kind,
  confidence and exact span.
* Negative: laughs and keyboard mashes, words, UUIDs, git and SHA hashes, paths, URLs, version
  strings, base64 image data, code snippets, labels followed by prose or placeholders, numbers that
  are not cards (order and tracking numbers, IBAN, ID numbers, timestamps, Luhn-invalid numbers), and
  realistic Turkish and English chat messages, also wrapped in longer unicode text.
* Channels: Telegram and Discord tests send a message with a key and a card number and check what
  reaches the daemon fake, what is on disk (the whole inbox file), what a raw `receive` consumer gets,
  what a retry sends and what the owner is told.
* Stress: the `stress` rows of the corpus are long hostile inputs (`unit` repeated `times` times:
  `token:token:...`, `eyJ-eyJ-...`, a million digits, a wall of emoji, many private-key headers). Both
  test suites run them with a time limit and, where the row says so, an exact candidate count.

The target is zero false positives on the negatives and every positive found. Add a message to the
negative corpus whenever someone reports a false alarm.
