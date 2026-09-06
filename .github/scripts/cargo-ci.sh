#!/usr/bin/env bash
set -euo pipefail

# Keep this version in sync with the toolchain actions in ../workflows/ci.yml.
# Selecting Cargo alone is insufficient: it finds rustc, cargo-fmt, and
# cargo-clippy on PATH, where a system Rust installation can take precedence.
toolchain=1.98.0
cargo_path="$(rustup which --toolchain "$toolchain" cargo)"
export PATH="$(dirname "$cargo_path"):$PATH"
export RUSTUP_TOOLCHAIN="$toolchain"
exec "$cargo_path" "$@"
