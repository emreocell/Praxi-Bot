# Home Assistant

Bots can read your Home Assistant entities and call services (turn a light on,
set a thermostat). You set it up in the app: there is no file to edit. Every
call goes through the permission engine.

## Set it up in the app

1. In Home Assistant, open your profile, go to **Security** and create a
   **long-lived access token**.
2. In Praxi, open **Apps** and the **Home Assistant** card.
3. Enter the address (for example `http://homeassistant.local:8123`), paste the
   key (the long-lived access token you just made) and press **Save and test**. Praxi asks Home Assistant `/api/` and counts
   your entities. The setup is saved only when Home Assistant accepts it.
4. Under **Who can use Home Assistant**, choose **All bots** or **Only these
   bots**, then press **Save access**.

Bots can use it right away, with no restart. The token is kept in the secure
store and is never shown again: to change it, enter a new one. **Test
connection** checks the saved setup again, and **Remove** deletes the setup and
the stored token, so the tools disappear from every bot.

Only the owner can do this: the desktop app, or the host's owner key. A bot, a
CLI engine or a script that only holds the API token cannot read or change the
setup.

## From the command line

```
HA_TOKEN=... bot ha setup --url http://homeassistant.local:8123 --token-env HA_TOKEN
bot ha status
bot ha test
bot ha access --bot Atlas          # or --all; with neither, no bot
bot ha remove
```

`bot ha setup` reads the token from the environment variable you name (or a
hidden prompt) and never prints it. Use `--allow cover.open_cover` (repeatable)
for the list of services described below. See the [CLI reference](cli-reference.md).

## Tools and what asks

| Tool | Effect | Notes |
| --- | --- | --- |
| `ha_list_entities` | read | States, optionally of one domain. Names are untrusted text. |
| `ha_get_state` | read | One entity with attributes. |
| `ha_call_service` | write | Acts on the real world. It follows the bot's access level like any other write, so on **Full access** it runs without a card. |

Only plain device services follow the bot's access level: lights, switches,
fans, climate, media players, vacuums, humidifiers, water heaters, numbers and
selects, notifications and speech. Everything else asks on **every** level, Full
access included, unless you allowed it:

- any other domain: locks, alarm panels, covers (garage doors, blinds), valves,
  buttons, helpers, sirens, scripts, automations and scenes, and raw protocols
  such as MQTT or Z-Wave, which can reach a lock without naming one;
- any service whose name contains `unlock`, `disarm` or `open`;
- any call on an entity whose name contains `garage`, `gate`, `door`, `lock` or
  `alarm` (a garage door is often a plain switch);
- a call aimed at an area, a device, a floor, a label, `all` or nothing in
  particular, except for lights, fans, climate, media players, vacuums,
  humidifiers and water heaters;
- odd entity ids (anything but a lowercase `domain.object`): they are refused
  before they reach Home Assistant.

To stop the card for one service, write it in **Services allowed without
asking**, one per line as `domain.service` (for example `cover.open_cover`). A
service in the list runs without asking for every entity it is called on. Only
exact names count, and `homeassistant.*` can never be listed. The other safety
rules that can ask on Full access (for example data leaving your computer) still
apply.

## Safety

- Bots only ever talk to the address you gave. Redirects are never followed, so
  the token cannot be sent elsewhere, and the token is never returned by the
  app, the API or the command line.
- `https`, or plain `http` only for a home-network (including Tailscale) or
  localhost address. The address may not point at Praxi's own API, cloud
  metadata or link-local space.
- Services that run code or stop the hub are blocked for bots: `shell_command`,
  `python_script`, `pyscript`, `hassio`, `rest_command`, and
  `homeassistant.restart|stop|reload_all`, `recorder.purge*`.
- Entity names and attributes are text someone else can edit: bots treat them as
  data, never as instructions.

## By hand (headless hosts)

The app and `bot ha` write `<data dir>/homeassistant.toml`, and a file you write
yourself works the same way. The token is a secret reference, never the token:

```toml
url = "http://homeassistant.local:8123"
token = "homeassistant/token"             # name of a stored secret
allowed_domains = ["light", "switch"]     # optional: only these domains may be called
trusted_services = ["cover.open_cover"]   # optional: no card for these
bots = ["<bot id>"]                       # optional: absent = every bot, [] = none
```

Store the token with `bot secrets set homeassistant/token`. Restart `bot serve`
to read a file you edited; saving from the app or `bot ha` needs no restart (and
rewrites the file without its comments).
