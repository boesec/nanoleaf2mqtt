#!/usr/bin/env bash
# Runs cross-compiled test binaries on Linux; the Linux half of linux-test.ps1, which packs the
# bundle and calls this script with its path. On a Linux machine with a Rust toolchain, run
# scripts/check.sh instead.
#
# Bundle layout: tests/ (test binaries), bin/nanoleaf2mqtt (the service binary the tests start),
# source/ (CONTRACT.md and src/, read by tests), manifest-dir.txt and bin-exe.txt (the paths
# CARGO_MANIFEST_DIR and CARGO_BIN_EXE_nanoleaf2mqtt had at compile time).
set -uo pipefail
exec 2>&1

bundle=$(cd "$1" && pwd)
work=$(mktemp -d "${TMPDIR:-/var/tmp}/nanoleaf2mqtt-work.XXXXXX")
trap 'rm -rf "$work"' EXIT

manifest_dir=$(cat "$bundle/manifest-dir.txt")
bin_exe=$(cat "$bundle/bin-exe.txt")
chmod +x "$bundle"/bin/* "$bundle"/tests/*

# The compile-time paths are Windows paths. On Linux a backslash is an ordinary character, so
# they are relative paths: recreate them below the working directory the tests run in.
mkdir -p "$work/$manifest_dir"
cp -R "$bundle/source/." "$work/$manifest_dir/"
case "$bin_exe" in
    */*)
        mkdir -p "$work/$(dirname "$bin_exe")"
        ln -s "$bundle/bin/nanoleaf2mqtt" "$work/$bin_exe"
        ;;
    *)
        # Without a slash, the program is looked up in PATH.
        mkdir -p "$work/.path"
        ln -s "$bundle/bin/nanoleaf2mqtt" "$work/.path/$bin_exe"
        export PATH="$work/.path:$PATH"
        ;;
esac

cd "$work" || exit 1
echo "==> $(uname -srm), $(ldd --version | head -n 1)"
failed=()
for test in "$bundle"/tests/*; do
    name=$(basename "$test")
    echo "==> $name"
    if ! RUST_BACKTRACE=1 "$test"; then
        failed+=("$name")
    fi
done

if [ "${#failed[@]}" -ne 0 ]; then
    echo "FAILED: ${failed[*]}"
    exit 1
fi
echo "==> all test binaries passed"
