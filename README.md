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

Everything else is configured in the web UI at `NANOLEAF2MQTT_HTTP_BIND` and stored in
`nanoleaf2mqtt.db` in the data directory: the MQTT broker (URL, user, password), the topic
prefix, the devices (name, id, address, token) and the Home Assistant discovery switch. Changes
apply at once, without a restart.

- **First start:** the UI asks for an admin password (at least 8 characters); every other page
  needs it. Until a broker is configured the service publishes nothing.
- **Devices:** add a device with its address, then pair it on its page: hold the device's power
  button for 5–7 seconds until the lights flash and click "Pair" within 30 seconds. The token is
  stored and never shown. The id is the device's topic level (`a-z0-9-`); it is suggested from
  the name and cannot be changed later.
- **Secrets** (broker password, tokens) are stored as plain text in the database; on Unix the data
  directory is created with mode 0700 and the database file with mode 0600. The UI never shows
  them: an empty password or token field keeps the stored value.
- **Sessions** last 12 hours without use and 7 days at most; a restart logs out. After a wrong
  password further attempts are refused for a while, doubling up to a minute.
- The session cookie is marked `Secure`: browsers accept it over plain HTTP only from
  `localhost`. Reach the UI through a TLS reverse proxy (which must pass the `Host` header on
  unchanged) or an SSH tunnel.

`nanoleaf2mqtt pair --host <address>` requests a token on the command line instead and prints it
once.

## MQTT

Topics and payloads are specified in [CONTRACT.md](CONTRACT.md): state under
`nanoleaf/status/<id>/…`, commands under `nanoleaf/set/<id>/…`, availability in
`nanoleaf/connected`.

Without a subcommand the service runs until Ctrl-C or SIGTERM and then publishes
`connected = 0`.

## Build and check

```sh
cargo build --release
scripts/check.sh      # or scripts/check.ps1 on Windows: fmt, clippy, tests
```

## Third-party files

`assets/htmx.min.js` is [htmx](https://htmx.org) 2.0.4, unmodified (Zero-Clause BSD licence),
served by the binary; nothing is loaded from a CDN.

## Licence

MIT, see [LICENSE](LICENSE).
