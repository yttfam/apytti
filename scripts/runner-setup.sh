#!/usr/bin/env bash
# One-time setup for a Gitea runner host that builds apytti.
# Run as the user that owns the runner (NOT root, brew refuses).
# Speedwagon and giorno both need this if they're going to handle build jobs.

set -euo pipefail

echo "==> Checking Rust toolchain"
if ! command -v rustup >/dev/null; then
    echo "fatal: rustup not found. Install: https://rustup.rs"
    exit 1
fi

echo "==> Adding cross-compile targets"
rustup target add x86_64-unknown-linux-musl
rustup target add x86_64-pc-windows-gnu

echo "==> Installing cross-linkers via Homebrew"
if ! command -v x86_64-linux-musl-gcc >/dev/null; then
    brew install filosottile/musl-cross/musl-cross
fi

if ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
    brew install mingw-w64
fi

echo "==> Verifying"
rustup target list --installed
echo
which x86_64-linux-musl-gcc
which x86_64-w64-mingw32-gcc
echo
echo "Setup complete. The runner can now build apytti for macOS arm64, Linux x86_64, and Windows x86_64."
