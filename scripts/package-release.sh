#!/usr/bin/env bash
# Packs one release archive, the same way locally (scripts/release-build.ps1) and in the release
# workflow:
#   package-release.sh <version> <target> <binary> <output directory>
# Writes <output directory>/nanoleaf2mqtt-<version>-<target>.tar.gz with one top-level directory
# of the same name, holding the binary, LICENSE, README.md, CONTRACT.md and packaging/.
# Needs GNU tar (Linux, or Git Bash on Windows). Owner, group and modes are set explicitly, so the
# archive does not depend on the file system it was packed on. Prints the archive's path.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "usage: $0 <version> <target> <binary> <output directory>" >&2
    exit 2
fi
version=$1
target=$2
binary=$3
out_dir=$4

readonly NAME=nanoleaf2mqtt
root=$(cd "$(dirname "$0")/.." && pwd)
base="$NAME-$version-$target"

if [ ! -f "$binary" ]; then
    echo "FAILED: binary $binary missing" >&2
    exit 1
fi

mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p "$stage/$base/packaging/systemd"
cp "$binary" "$stage/$base/$NAME"
cp "$root/LICENSE" "$root/README.md" "$root/CONTRACT.md" "$stage/$base/"
cp "$root/packaging/install.sh" "$stage/$base/packaging/"
cp "$root/packaging/systemd/$NAME.service" "$stage/$base/packaging/systemd/"

# Fixed owner, modes and order; the time stamp is that of the last commit, so a rebuild of the
# same commit packs the same entries.
mtime=$(git -C "$root" log -1 --format=%cI 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%SZ)
tar_opts=(--owner=0 --group=0 --numeric-owner --sort=name --mtime="$mtime" -C "$stage")
archive="$stage/$base.tar"
tar "${tar_opts[@]}" --mode=0755 --no-recursion -cf "$archive" \
    "$base" "$base/packaging" "$base/packaging/systemd"
tar "${tar_opts[@]}" --mode=0644 -rf "$archive" \
    "$base/LICENSE" "$base/README.md" "$base/CONTRACT.md" "$base/packaging/systemd/$NAME.service"
tar "${tar_opts[@]}" --mode=0755 -rf "$archive" "$base/$NAME" "$base/packaging/install.sh"
gzip -n -9 -c "$archive" >"$out_dir/$base.tar.gz"

echo "$out_dir/$base.tar.gz"
