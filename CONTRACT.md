# MQTT contract

Contract version **2**. Topic names and payload shapes below are stable; renaming a topic or
changing a payload is a breaking change and raises the contract version (published in
`<prefix>/info`). It follows mqtt-smarthome 2.0
(<https://github.com/mqtt-smarthome/mqtt-smarthome>, `SPEC.md`); where it deviates, the section
[Deviations from mqtt-smarthome 2.0](#deviations-from-mqtt-smarthome-20) says so and why.

## Conventions

- `<prefix>` is `nanoleaf` (configurable). `<id>` is the device id configured per device
  (lower case `a-z0-9-`, e.g. `kellertreppe`), stable across renames and device swaps.
- Everything is published with **QoS 0**, and the command subscription is QoS 0 as well.
- `status/*` topics are **retained**, payload `{"val": …, "ts": <ms>, "lc": <ms>}`: `ts` when
  the value was obtained (ms since epoch), `lc` when it last changed. The *Example* columns
  below show `val`.
- **The device is the truth**: status topics carry what the panels report (initial read, event
  stream, re-read), never what a command asked for.
- **Only on change**: a value is published when it changes. After the device (re)connects, and
  after every (re)connect to the broker, all values are published again, changed or not.
- A value the device does not report is **not published**. While a device is unreachable its last
  values stay; `status/<id>/online` says they are old.
- Commands under `set/` are not retained by us; retained `set` messages are ignored (a broker
  replaying an old command after a restart must not switch the light).

## Service topics

| Topic | Retained | Payload |
|---|---|---|
| `<prefix>/connected` | yes, last will `0` | plain text `0` service down (last will, shutdown), `2` connected to the broker and running, whether or not the devices are reachable; `1` is not used |
| `<prefix>/info` | yes | `{"name":"nanoleaf2mqtt","version":"0.1.0","spec":"2.0","host":"nanoleaf2mqtt","pid":1234,"started":"2026-10-16T12:00:00.000Z","maintenance":false,"contract_version":2}` |

`info`: `spec` is the mqtt-smarthome version implemented, `host` the host name (left out when it
cannot be found), `pid` the process id, `started` the start time (RFC 3339), `maintenance` whether
maintenance topics are enabled (there are none), `contract_version` the version of this contract.

## Device status topics (`<prefix>/status/<id>/…`)

| Path | `val` | Example |
|---|---|---|
| `online` | boolean: the device is reachable | `true` |
| `on` | boolean | `true` |
| `brightness` | integer 0–100 (%; the device reports 0 while off) | `30` |
| `hue` | integer 0–360 (°) | `120` |
| `sat` | integer 0–100 (%) | `80` |
| `hs` | array `[hue, sat]`, published when `hue` or `sat` changes and both are known | `[120,80]` |
| `ct` | integer, Kelvin, range as reported (1200–6500 on Shapes) | `2700` |
| `colormode` | `effect`, `hs`, `ct` (as the device reports) | `"effect"` |
| `effect` | string, name of the selected effect, as reported (incl. pseudo values like `*Shuffle*`) | `"Northern Lights"` |
| `effects` | array of strings, the device's effect list | `["Beatdrop", …]` |
| `device` | object: `model`, `firmware`, `name`, `panels` | `{"model":"NL42","firmware":"12.0.3","name":"…","panels":15}` |
| `layout` | object: `orientation` (°), `side_length`, `panels`: array of `{id, x, y, rotation, shape, shape_type}`; not published at all when the device reports no position data | `{"orientation":120,"side_length":134,"panels":[{"id":1001,"x":201,"y":77,"rotation":0,"shape":"triangle_shapes","shape_type":8}]}` |
| `touch` | **not retained**, object `{"panel": <id>, "gesture": "<g>"}`; no `lc` | `{"panel":1234,"gesture":"swipe_up"}` |

`online` is published after the values next to it: `true` means they are current.

Gestures: `tap`, `double_tap`, `swipe_up`, `swipe_down`, `swipe_left`, `swipe_right`,
`unknown_<n>` for anything else.

A complete `touch` message: `{"val":{"panel":1234,"gesture":"swipe_up"},"ts":1760529600000}`.
`layout` details: `orientation` is the global orientation in degrees, `side_length` the side length
of one panel, per panel `id` is the panel id (the same as `panel` in `touch`; the controller unit
is part of the list, as the device reports it), `x`/`y` the position, `rotation` the panel's
rotation in degrees and `shape_type` the device's shape number. `shape` is its name, from the
shape-type table of the Nanoleaf OpenAPI documentation: `triangle` (0), `rhythm` (1), `square`
(2), `control_square_primary` (3), `control_square_passive` (4), `power_supply` (5),
`hexagon_shapes` (7), `triangle_shapes` (8), `mini_triangle_shapes` (9), `controller` (12),
`elements_hexagon` (14), `elements_hexagon_corner` (15), `lines_connector` (16), `light_lines`
(17), `light_lines_single_zone` (18), `controller_cap` (19), `power_connector` (20), and
`unknown` for any other number (`shape_type` is always kept). Not exposed through Home Assistant
discovery.

Within `device`, `layout` (also per panel) and `touch`, a field the device does not report is left out (swipes are not tied
to one panel and carry no `panel`).

## Commands (`<prefix>/set/<id>/…`)

Payload either a plain value or `{"val": …}` (both accepted). Numbers may be decimal text; they
are rounded to whole numbers and **clamped** into the range: a value outside it is moved to the
nearest end, sent, and logged at `warn` with topic and payload. A payload that cannot be read
(not a number where one is needed, an unknown effect, …) is logged at `warn` and ignored, nothing
is sent to the device.

| Path | Payload | Effect |
|---|---|---|
| `on` | `true`/`false`, `1`/`0`, `on`/`off` | switch |
| `brightness` | 0–100, optionally `{"val":50,"transition":2}` (seconds, 0 or more) | set brightness (does not switch on by itself – the device decides) |
| `hue` | 0–360 | colour, switches the device to `hs` mode |
| `sat` | 0–100 | saturation |
| `hs` | `"h,s"` or `[h,s]`, e.g. `120,80` or `{"val":[120,80]}` | hue, then saturation (two device requests); each is clamped on its own, and nothing is sent if either cannot be read |
| `ct` | Kelvin within the reported range | colour temperature |
| `effect` | effect name from `effects` | select effect |
| `identify` | anything | panels flash |

Ranges: brightness and saturation 0–100, hue 0–360, `ct` the range the device reports, and the
device's own range for the others where it reports a narrower one.

## Lifecycle

- On connect: `connected = 2`, `info`, then every device's full status; subscribe to `set/#`.
- **Read-back** on every connect: the retained `<prefix>/status/#` and, while discovery is on,
  this service's discovery topics are read back. Every status topic that does not belong to a
  configured device and a status path above is cleared (empty retained publish): topics of
  devices removed while the service was down, unknown paths, version 1 `reachable`. Topics of a
  configured device that has not reported yet (switched off, say) are kept. The read-back ends
  when the broker hands back the `info` message just published; the discovery messages go out
  after it.
- Graceful shutdown publishes `connected = 0`; a crash leaves it to the last will.
- A removed device gets every retained topic cleared (empty retained publish).
- Broker change: disconnect with `connected = 0`, connect to the new broker, republish all.

## Home Assistant

Home Assistant MQTT discovery (<https://www.home-assistant.io/integrations/mqtt/>), in the
device-based form (Home Assistant 2024.11 and later, mqtt-smarthome 2.0 section 8), is switchable
in the configuration UI and **on by default**. The discovery message points directly at the
topics above and reads `val` through value templates; there are no additional state topics.

`<ha>` is the discovery prefix (default `homeassistant`), `<id>` the device id. One retained
JSON message per device:

| Topic | Content |
|---|---|
| `<ha>/device/nanoleaf2mqtt_<id>/config` | `device`, `origin` (`name`, `sw_version`), `availability`, `availability_mode`, `qos` 0, and the three components below |

| Component | `p` | `unique_id` | Entity |
|---|---|---|---|
| `light` | `light` | `nanoleaf2mqtt_<id>_light` | `light` (default schema), named after the device: `on`, `brightness` (scale 100), `hs`, `ct` in Kelvin (range as reported), `effect` with the device's effect list |
| `touch` | `event` | `nanoleaf2mqtt_<id>_touch` | `event` on `touch`, event types = the gestures above (`unknown_<n>` is not announced); the event carries the touch object's fields |
| `identify` | `button` | `nanoleaf2mqtt_<id>_identify` | `button` on `set/<id>/identify` |

- One Home Assistant device per device: identifier `nanoleaf2mqtt_<id>`, name, manufacturer,
  model and firmware as the device reports them (the name falls back to `<id>`). Suggested entity
  ids `light.nanoleaf_<id>`, `event.nanoleaf_<id>_touch`, `button.nanoleaf_<id>_identify` (`-`
  in the id becomes `_`). The unique ids are those of version 1, so Home Assistant keeps the
  entities and their settings.
- Availability (`availability_mode: all`): `<prefix>/connected` is `2` **and**
  `status/<id>/online` is `true`. Each entry carries only `topic`, `payload_available`,
  `payload_not_available` and `value_template`. The `online` entry is named only once the topic
  has a value; until then the message names `connected` only.
- A device's message is published once it has reported (identity, ranges, effect list) and
  again when that changes, and on every connect, like the status topics.
- Switching discovery off, or removing or disabling a device, clears its discovery topic (empty
  retained publish).
- Version 1 messages (`<ha>/light/nanoleaf2mqtt_<id>/light/config`,
  `<ha>/event/nanoleaf2mqtt_<id>/touch/config`, `<ha>/button/nanoleaf2mqtt_<id>/identify/config`)
  found by the read-back are moved over once, as Home Assistant documents it: a migration request
  `{"migrate_discovery": true}` on each, then the device-based message, then each cleared. Those
  of a device that is no longer configured are cleared. Discovery messages whose availability
  names another prefix belong to another instance and are left alone.
- No address, token or password is ever part of a discovery message.

## Changes from version 1

- `status/<id>/online` replaces `status/<id>/reachable` (same payload); `reachable` is no longer
  published and is cleared by the read-back.
- `connected` is `2` while the service is connected to the broker, whatever its devices do (it
  was `1` while a device was unreachable); `1` is not used. Device reachability is `online`.
- `info`: `started` replaces `started_at`; new `host`, `pid`, `maintenance`; `spec` is `2.0`
  (was `mqtt-smarthome 2.0`); `contract_version` is `2`.
- QoS 0 for every publish, the last will and the command subscription (was QoS 1).
- Values are published only when they change; the device's echo of a command on its event
  stream publishes nothing more. Reconnects still publish everything.
- Out-of-range numbers in commands are clamped and logged (they were rejected); decimals are
  rounded (they were rejected).
- Retained topics a previous run left are read back and cleared on every connect.
- Home Assistant discovery: one device-based message per device instead of three per-entity
  messages; availability from `connected = 2` and `online`; on by default. Unique ids are
  unchanged; version 1 messages are migrated and cleared.

## Deviations from mqtt-smarthome 2.0

- **Device ids may contain `-`** (section 3: item names SHOULD be `snake_case`). The ids are
  chosen by the owner and stable; changing them would rename every topic and Home Assistant
  entity.
- **`connected = 1` is not used** (section 3.1): this is a bridge for several devices, so it
  publishes `2` whenever it runs and reports each device in `status/<id>/online`, as section 3.1
  describes for multi-device adapters.
- **The discovery topic is `<ha>/device/nanoleaf2mqtt_<id>/config`** without the instance name
  (section 8 suggests `<adapter>_<name>_<device>`, linked to a bridge device with `via_device`):
  it is the node version 1 used, which lets Home Assistant migrate the existing entities in
  place. A second instance on the same broker needs its own discovery prefix or other device ids.
  There is no bridge device and no `via_device`.
- **No runtime version in `info`** (section 6 `node`, the runtime version, SHOULD): a compiled
  binary has no runtime of its own to report.
- **No `support_url` in `origin`** (section 8 SHOULD): there is no public URL yet.
