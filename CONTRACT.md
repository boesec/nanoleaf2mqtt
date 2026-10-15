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
| `ct` | integer, Kelvin, range as reported (1200–6500 on Shapes) | `2700` |
| `colormode` | `effect`, `hs`, `ct` (as the device reports) | `"effect"` |
| `effect` | string, name of the selected effect, as reported (incl. pseudo values like `*Shuffle*`) | `"Northern Lights"` |
| `effects` | array of strings, the device's effect list | `["Beatdrop", …]` |
| `device` | object: `model`, `firmware`, `name`, `panels` | `{"model":"NL42","firmware":"12.0.3","name":"…","panels":15}` |
| `touch` | **not retained, QoS 0**, object `{"panel": <id>, "gesture": "<g>"}`; no `lc` | `{"panel":1234,"gesture":"swipe_up"}` |

Gestures: `tap`, `double_tap`, `swipe_up`, `swipe_down`, `swipe_left`, `swipe_right`,
`unknown_<n>` for anything else.

A complete `touch` message: `{"val":{"panel":1234,"gesture":"swipe_up"},"ts":1760529600000}`.
Within `device` and `touch`, a field the device does not report is left out (swipes are not tied
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
| `ct` | Kelvin within the reported range | colour temperature |
| `effect` | effect name from `effects` | select effect |
| `identify` | anything | panels flash |

## Lifecycle

- On connect: `connected`, `info`, then every device's full status; subscribe to `set/#`.
- Graceful shutdown publishes `connected = 0`; a crash leaves it to the last will.
- A removed device gets every retained topic cleared (empty retained publish).
- Broker change: disconnect with `connected = 0`, connect to the new broker, republish all.

## Home Assistant

Home Assistant MQTT discovery (planned, switchable) points directly at these topics via value
templates; no additional state topics.
