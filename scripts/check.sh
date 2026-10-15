#!/usr/bin/env bash
# Runs the same checks as the gate: format, lints, tests. Stops at the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
