#!/usr/bin/env bash
# Installs or updates nanoleaf2mqtt on a systemd host and restarts it. Run as root:
#   install.sh <binary> <unit file>
# Creates the system user on the first run, replaces binary and unit, restarts the service and
# waits for its start line in the journal. Leaves the data directory alone.
set -euo pipefail

readonly NAME=nanoleaf2mqtt
readonly BIN_PATH=/usr/local/bin/$NAME
readonly UNIT_PATH=/etc/systemd/system/$NAME.service
readonly START_LINE="$NAME starting"
readonly WAIT_SECONDS=20

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <binary> <unit file>" >&2
    exit 2
fi
binary=$1
unit=$2

if [ "$(id -u)" -ne 0 ]; then
    echo "$0 must run as root" >&2
    exit 1
fi

if ! id -u "$NAME" >/dev/null 2>&1; then
    echo "==> creating system user $NAME"
    useradd --system --user-group --home-dir "/var/lib/$NAME" --no-create-home \
        --shell /usr/sbin/nologin "$NAME"
fi

# Install next to the target and rename, so a running service never sees a half-written file.
install -m 0755 -o root -g root "$binary" "$BIN_PATH.new"
echo "==> installing $("$BIN_PATH.new" --version) to $BIN_PATH"
mv -f "$BIN_PATH.new" "$BIN_PATH"
install -m 0644 -o root -g root "$unit" "$UNIT_PATH"

systemctl daemon-reload
systemctl enable --quiet "$NAME"
echo "==> restarting $NAME"
systemctl restart "$NAME"

# The journal lines of exactly this start, not of an earlier one.
invocation=$(systemctl show --property=InvocationID --value "$NAME")
started=false
for _ in $(seq "$WAIT_SECONDS"); do
    if journalctl --quiet --output=cat "_SYSTEMD_INVOCATION_ID=$invocation" | grep -q "$START_LINE"; then
        started=true
        break
    fi
    sleep 1
done
# A service that fails right after its start line (for example because the UI address is taken)
# is restarted by systemd under a new invocation id.
sleep 2
current=$(systemctl show --property=InvocationID --value "$NAME")

if ! $started || [ "$current" != "$invocation" ] || ! systemctl is-active --quiet "$NAME"; then
    echo "FAILED: $NAME did not start" >&2
    systemctl status --no-pager "$NAME" >&2 || true
    journalctl --no-pager -n 30 -u "$NAME" >&2 || true
    exit 1
fi

echo "==> $NAME is $(systemctl is-active "$NAME")"
journalctl --quiet --no-pager --output=short-iso "_SYSTEMD_INVOCATION_ID=$invocation"
