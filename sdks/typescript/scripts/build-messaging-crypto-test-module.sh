#!/bin/sh
# Builds the Messaging crypto core (packages/messaging-crypto/wasm) for the SDK's
# integration test: `wasm-bindgen --target nodejs` into .wasm-test/nodejs and
# `--target web` into .wasm-test/web. The SDK never ships these files; the test loads
# them through the MessagingCrypto adapter exactly as an application would.
#
#   sh scripts/build-messaging-crypto-test-module.sh [out dir]
#
# Requires the wasm32-unknown-unknown target and wasm-bindgen-cli at the version the
# crate's Cargo.lock pins.
set -eu
HERE="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
CRATE="$ROOT/packages/messaging-crypto/wasm"
OUT="${1:-$HERE/.wasm-test}"
TARGET_DIR="${CARGO_TARGET_DIR:-$CRATE/target}"

locked="$(awk '/^name = "wasm-bindgen"$/{getline; print}' "$CRATE/Cargo.lock" | cut -d'"' -f2)"
have="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" || have=""
if [ "$have" != "$locked" ]; then
  echo "error: wasm-bindgen-cli $locked is required (found '${have:-none}'): cargo install wasm-bindgen-cli --version $locked --locked" >&2
  exit 1
fi

( cd "$CRATE" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build --locked --release --target wasm32-unknown-unknown )
WASM="$TARGET_DIR/wasm32-unknown-unknown/release/cf_messaging_crypto_wasm.wasm"

rm -rf "$OUT"
wasm-bindgen --target nodejs --out-dir "$OUT/nodejs" "$WASM"
wasm-bindgen --target web --out-dir "$OUT/web" "$WASM"
# The SDK package is ESM; the nodejs glue is CommonJS.
printf '{ "type": "commonjs" }\n' > "$OUT/nodejs/package.json"
printf '{ "type": "module" }\n' > "$OUT/web/package.json"
echo "built $OUT (wasm-bindgen $have)"
