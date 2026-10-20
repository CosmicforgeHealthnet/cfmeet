#!/usr/bin/env bash
# Reproducible build of the browser crypto core shipped with apps/web.
#
#   scripts/messaging/build-web-crypto.sh           rebuild and write apps/web/public/messaging-crypto/
#   scripts/messaging/build-web-crypto.sh --check   rebuild and fail unless the committed artifact is byte-identical
#
# The web app is built without a Rust toolchain (Vercel runs `npm run build` only), so
# the WebAssembly module and its JavaScript glue are committed, content-addressed and
# hash-pinned (the worker refuses a module whose SHA-384 differs). This script is the
# provenance of that artifact: CI rebuilds it from source and compares byte for byte.
#
# Reproducibility: the same rustc and wasm-bindgen versions, the locked dependency
# graph, and the sources copied to ONE fixed absolute path (path dependencies hash their
# absolute path into type ids), with registry and build paths remapped.
set -euo pipefail

RUSTC_VERSION="1.97.0"
WASM_BINDGEN_VERSION="0.2.129"
CANON="/tmp/cfmeet-web-crypto-build"
MODE="${1:-write}"
case "$MODE" in write|--check) ;; *) echo "usage: $0 [--check]" >&2; exit 2 ;; esac

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
have_rustc="$(rustc -V | awk '{print $2}')"
if [ "$have_rustc" != "$RUSTC_VERSION" ]; then
  echo "error: rustc $RUSTC_VERSION is required for a reproducible build (found $have_rustc)" >&2
  exit 1
fi
have_bindgen="$(wasm-bindgen --version | awk '{print $2}')"
if [ "$have_bindgen" != "$WASM_BINDGEN_VERSION" ]; then
  echo "error: wasm-bindgen-cli $WASM_BINDGEN_VERSION is required (found $have_bindgen)" >&2
  exit 1
fi
locked="$(awk '/^name = "wasm-bindgen"$/{getline; print}' "$ROOT/packages/messaging-crypto/wasm/Cargo.lock" | cut -d'"' -f2)"
if [ "$locked" != "$WASM_BINDGEN_VERSION" ]; then
  echo "error: Cargo.lock pins wasm-bindgen $locked, expected $WASM_BINDGEN_VERSION" >&2
  exit 1
fi

rm -rf "$CANON"
mkdir -p "$CANON/messaging-crypto"
( cd "$ROOT/packages/messaging-crypto" && \
  tar --exclude=./target --exclude=./wasm/target --exclude=./web-harness -cf - . ) | tar -xf - -C "$CANON/messaging-crypto"

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
(
  cd "$CANON/messaging-crypto/wasm"
  CARGO_INCREMENTAL=0 \
  RUSTFLAGS="--remap-path-prefix=$cargo_home/registry/src=/cargo-src --remap-path-prefix=$CANON=/build" \
    cargo build --locked --release --target wasm32-unknown-unknown
)
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section \
  --out-dir "$CANON/out" \
  "$CANON/messaging-crypto/wasm/target/wasm32-unknown-unknown/release/cf_messaging_crypto_wasm.wasm"

node "$ROOT/apps/web/scripts/package-messaging-crypto.mjs" "$CANON/out" \
  "$([ "$MODE" = "--check" ] && echo check || echo write)" \
  "rustc $RUSTC_VERSION, wasm-bindgen $WASM_BINDGEN_VERSION"
