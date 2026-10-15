# nanoleaf2mqtt

Bridge between Nanoleaf light panels and MQTT, written in Rust.

It reads the panels' state through the local Nanoleaf OpenAPI, follows their event stream,
publishes state and touch gestures following the
[mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome) conventions and executes
commands received over MQTT.

Status: in development.

## Configuration

Bootstrap settings come from the environment:

| Variable | Default | Meaning |
|---|---|---|
| `NANOLEAF2MQTT_DATA_DIR` | `./data` | directory of the SQLite store |
| `NANOLEAF2MQTT_HTTP_BIND` | `127.0.0.1:8080` | address of the configuration UI |
| `NANOLEAF2MQTT_LOG` | `info` | `tracing` filter |

## MQTT

Topics and payloads are specified in [CONTRACT.md](CONTRACT.md): state under
`nanoleaf/status/<id>/…`, commands under `nanoleaf/set/<id>/…`, availability in
`nanoleaf/connected`.

Until the configuration UI exists, one device and the broker are configured through the
environment:

| Variable | Required | Meaning |
|---|---|---|
| `NANOLEAF_HOST` | yes | address of the device |
| `NANOLEAF_PORT` | no, `16021` | port of the device's OpenAPI |
| `NANOLEAF_TOKEN` | yes | auth token, see `nanoleaf2mqtt pair --help` |
| `NANOLEAF_DEVICE_ID` | yes | device id in the topics, `a-z0-9-` |
| `MQTT_URL` | yes | `mqtt://host[:port]` (TLS is not supported yet) |
| `MQTT_USER`, `MQTT_PASSWORD` | no | broker credentials; anonymous when unset |
| `NANOLEAF2MQTT_PREFIX` | no, `nanoleaf` | topic prefix |

Without a subcommand the service runs until Ctrl-C or SIGTERM and then publishes
`connected = 0`.

## Build and check

```sh
cargo build --release
scripts/check.sh      # or scripts/check.ps1 on Windows: fmt, clippy, tests
```

## Licence

MIT, see [LICENSE](LICENSE).
