#!/bin/sh
# wrangler's build command. Workers Builds' image has no Rust, so install
# what is missing (rustup, the wasm target, worker-build), then build.
set -eu
if ! command -v cargo >/dev/null 2>&1; then
    if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    fi
    PATH="$HOME/.cargo/bin:$PATH"
fi
if command -v rustup >/dev/null 2>&1; then
    rustup target add wasm32-unknown-unknown
fi
if ! command -v worker-build >/dev/null 2>&1; then
    # The image has libssl-dev but no pkg-config, which openssl-sys looks for.
    if ! command -v pkg-config >/dev/null 2>&1 && [ -f /usr/include/openssl/ssl.h ]; then
        export OPENSSL_INCLUDE_DIR=/usr/include
        export OPENSSL_LIB_DIR="/usr/lib/$(uname -m)-linux-gnu"
    fi
    cargo install --locked "worker-build@^0.8"
fi
exec worker-build --release
