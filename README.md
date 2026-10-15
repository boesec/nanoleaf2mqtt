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

## Build and check

```sh
cargo build --release
scripts/check.sh      # or scripts/check.ps1 on Windows: fmt, clippy, tests
```

## Licence

MIT, see [LICENSE](LICENSE).
