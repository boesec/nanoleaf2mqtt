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

Topics and payloads follow mqtt-smarthome 2.0 and are specified in
[CONTRACT.md](CONTRACT.md) (version 2): state under `nanoleaf/status/<id>/…`, commands under
`nanoleaf/set/<id>/…`, the service in `nanoleaf/connected` (`2` running, `0` down) and each
device's reachability in `nanoleaf/status/<id>/online`. Home Assistant discovery is on by default.
Version 1 published `nanoleaf/status/<id>/reachable` instead of `online` and set `connected` to
`1` while a device was unreachable; the changes are listed in the contract.

Without a subcommand the service runs until Ctrl-C or SIGTERM and then publishes
`connected = 0`.

## Build and check

```sh
cargo build --release
scripts/check.sh      # or scripts/check.ps1 on Windows: fmt, clippy, tests
```

## Deployment

The service runs as a systemd unit on Linux (x86_64, glibc 2.36 or later, for example Debian 12
or 13), under its own system user `nanoleaf2mqtt`. The unit is
[packaging/systemd/nanoleaf2mqtt.service](packaging/systemd/nanoleaf2mqtt.service): it restarts
the service whenever it stops and never gives up, and it is hardened (read-only system, no
capabilities, private `/tmp`, system call filter).

### Build on Windows for Linux

The Linux binary is cross-compiled with [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild),
which uses [Zig](https://ziglang.org) as C compiler and linker. Once:

```powershell
winget install zig.zig
cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu
```

Then:

```powershell
scripts/build-linux.ps1   # -> target/x86_64-unknown-linux-gnu/release/nanoleaf2mqtt
```

On Linux, `cargo build --release` builds it directly.

### Install and update

From Windows, one command builds, copies binary and unit to the host over SSH, installs or
updates them, restarts the service and waits for its start line in the journal:

```powershell
scripts/deploy.ps1 -Target root@<host>              # -SkipBuild deploys the last build
```

On the host itself (as root), with the binary and the repository's `packaging/` directory at
hand, [packaging/install.sh](packaging/install.sh) does the same:

```sh
packaging/install.sh ./nanoleaf2mqtt packaging/systemd/nanoleaf2mqtt.service
```

The first run creates the system user; the data directory is never touched.

### Where things live

| Path | Content |
|---|---|
| `/usr/local/bin/nanoleaf2mqtt` | the binary |
| `/etc/systemd/system/nanoleaf2mqtt.service` | the unit |
| `/var/lib/nanoleaf2mqtt/nanoleaf2mqtt.db` | the configuration store, including the secrets; directory 0700, file 0600, owned by `nanoleaf2mqtt`. Back it up and keep the backup private |

Logs: `journalctl -u nanoleaf2mqtt -f`. Bootstrap settings are changed with a drop-in
(`systemctl edit nanoleaf2mqtt`); nothing secret belongs in the unit or a drop-in.

### Forgotten admin password

Remove the stored password, then set a new one in the UI (the first-start page appears again).
Run the command as the service user, with the same data directory as the service, and restart
the service afterwards so that existing sessions end:

```sh
sudo -u nanoleaf2mqtt env NANOLEAF2MQTT_DATA_DIR=/var/lib/nanoleaf2mqtt nanoleaf2mqtt reset-password
systemctl restart nanoleaf2mqtt
```

The command never sets, asks for or prints a password. It can run while the service runs.

### Reaching the UI

The unit binds the UI to `127.0.0.1:8080`, so it is not reachable from the network directly.
Either put a TLS reverse proxy on the same host in front of it (for example nginx with
`proxy_pass http://127.0.0.1:8080;` and `proxy_set_header Host $host;`, because the UI compares
`Origin` with `Host` and its session cookie needs TLS), or open an SSH tunnel and browse to
`http://localhost:8080`:

```sh
ssh -L 8080:127.0.0.1:8080 root@<host>
```

### Testing on Linux from Windows

`scripts/linux-test.ps1 -Target root@<host>` runs clippy for the Linux target, cross-compiles
the test binaries and runs them on a Linux host over SSH; the host needs no Rust toolchain.
Everything is copied to a temporary directory under `/var/tmp` and removed afterwards.

## Third-party files

`assets/htmx.min.js` is [htmx](https://htmx.org) 2.0.4, unmodified (Zero-Clause BSD licence),
served by the binary; nothing is loaded from a CDN.

## Licence

MIT, see [LICENSE](LICENSE).
