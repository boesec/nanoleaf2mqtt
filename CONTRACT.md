# MQTT contract

Contract version **1**. Topic names and payload shapes below are stable; renaming a topic or
changing a payload is a breaking change and raises the contract version (published in
`<prefix>/info`). Modelled on mqtt-smarthome 2.0
(<https://github.com/mqtt-smarthome/mqtt-smarthome>).

## Conventions

- `<prefix>` is `nanoleaf` (configurable). `<id>` is the device id configured per device
  (lower case `a-z0-9-`, e.g. `kellertreppe`), stable across renames and device swaps.
- `status/*` topics are **retained**, **QoS 1**, payload `{"val": …, "ts": <ms>, "lc": <ms>}`:
  `ts` when the value was obtained (ms since epoch), `lc` when it last changed. The *Example*
  columns below show `val`.
- **The device is the truth**: status topics carry what the panels report (initial read, event
  stream, re-read), never what a command asked for.
- A value the device does not report is **not published**. While a device is unreachable its last
  values stay; `status/<id>/reachable` says they are old.
- Commands under `set/` are not retained by us; retained `set` messages are ignored (a broker
  replaying an old command after a restart must not switch the light).

## Service topics

| Topic | Retained | Payload |
|---|---|---|
| `<prefix>/connected` | yes, last will `0` | plain text `0` service down, `1` broker connected but at least one device unreachable (or none configured), `2` all devices reachable |
| `<prefix>/info` | yes | `{"name":"nanoleaf2mqtt","version":"0.1.0","spec":"mqtt-smarthome 2.0","contract_version":1,"started_at":"<RFC 3339>"}` |

## Device status topics (`<prefix>/status/<id>/…`)

| Path | `val` | Example |
|---|---|---|
| `reachable` | boolean | `true` |
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
| `touch` | **not retained, QoS 0**, object `{"panel": <id>, "gesture": "<g>"}`; no `lc` | `{"panel":1234,"gesture":"swipe_up"}` |

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

Payload either a plain value or `{"val": …}` (both accepted); unparsable or out-of-range payloads
are logged and ignored, nothing is sent to the device.

| Path | Payload | Effect |
|---|---|---|
| `on` | `true`/`false`, `1`/`0`, `on`/`off` | switch |
| `brightness` | 0–100, optionally `{"val":50,"transition":2}` (seconds) | set brightness (does not switch on by itself – the device decides) |
| `hue` | 0–360 | colour, switches the device to `hs` mode |
| `sat` | 0–100 | saturation |
| `hs` | `"h,s"` or `[h,s]`, e.g. `120,80` or `{"val":[120,80]}` | hue, then saturation (two device requests); both are checked first, nothing is sent if either is invalid |
| `ct` | Kelvin within the reported range | colour temperature |
| `effect` | effect name from `effects` | select effect |
| `identify` | anything | panels flash |

## Lifecycle

- On connect: `connected`, `info`, then every device's full status; subscribe to `set/#`.
- Graceful shutdown publishes `connected = 0`; a crash leaves it to the last will.
- A removed device gets every retained topic cleared (empty retained publish).
- Broker change: disconnect with `connected = 0`, connect to the new broker, republish all.

## Home Assistant

Home Assistant MQTT discovery (<https://www.home-assistant.io/integrations/mqtt/>) is switchable
in the configuration UI, off by default. The discovery messages point directly at the topics
above and read `val` through value templates; there are no additional state topics.

`<ha>` is the discovery prefix (default `homeassistant`), `<id>` the device id. Every message is
retained, QoS 1, a JSON object:

| Topic | Entity |
|---|---|
| `<ha>/light/nanoleaf2mqtt_<id>/light/config` | `light` (default schema), named after the device: `on`, `brightness` (scale 100), `hs`, `ct` in Kelvin (range as reported), `effect` with the device's effect list |
| `<ha>/event/nanoleaf2mqtt_<id>/touch/config` | `event` on `touch`, event types = the gestures above (`unknown_<n>` is not announced); the event carries the touch object's fields |
| `<ha>/button/nanoleaf2mqtt_<id>/identify/config` | `button` on `set/<id>/identify` |

- One Home Assistant device per device: identifier `nanoleaf2mqtt_<id>`, name, manufacturer,
  model and firmware as the device reports them (the name falls back to `<id>`). Unique ids are
  `nanoleaf2mqtt_<id>_light`, `…_touch`, `…_identify`; suggested entity ids
  `light.nanoleaf_<id>`, `event.nanoleaf_<id>_touch`, `button.nanoleaf_<id>_identify` (`-` in
  the id becomes `_`).
- Availability (`availability_mode: all`): `<prefix>/connected` is `1` or `2` (service up, not `0`) **and**
  `status/<id>/reachable` is `true`.
- A device's messages are published once it has reported (identity, ranges, effect list) and
  again when that changes, and on every connect, like the status topics.
- Switching discovery off, or removing or disabling a device, clears its discovery topics (empty
  retained publish).
- No address, token or password is ever part of a discovery message.
