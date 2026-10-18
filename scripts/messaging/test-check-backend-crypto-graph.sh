#!/usr/bin/env bash
# Self-test of check-backend-crypto-graph.sh: an allowed graph passes, deliberately
# prohibited graphs (client feature in the crypto core; a backend-shaped crate that enables
# `client` on a normal dependency) fail, and a broken invocation is a command failure (2),
# never a pass.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
check="$here/check-backend-crypto-graph.sh"
fail=0
expect_exit() {
  local want="$1"; shift
  "$@" >/dev/null 2>&1; local got=$?
  if [ "$got" -ne "$want" ]; then echo "FAIL (exit $got, want $want): $*"; fail=1; else echo "ok   (exit $got): ${*:2}"; fi
}

crypto="$root/packages/messaging-crypto"
expect_exit 0 "$check" expect-absent  "$crypto"
expect_exit 1 "$check" expect-absent  "$crypto" --features client
expect_exit 1 "$check" expect-absent  "$crypto" --features web
expect_exit 0 "$check" expect-present "$crypto" --features client
expect_exit 1 "$check" expect-present "$crypto"

# A deliberately prohibited backend: depends on the crypto core with `client` as a NORMAL
# dependency. Built from the real lockfile of services/messaging so `--locked` resolves offline.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bad/src"; echo 'fn main() {}' > "$tmp/bad/src/main.rs"
cat > "$tmp/bad/Cargo.toml" <<TOML
[package]
name = "prohibited-backend"
version = "0.0.0"
edition = "2021"
[dependencies]
cf-messaging-crypto = { path = "$crypto", features = ["client"] }
[workspace]
TOML
(cd "$tmp/bad" && cargo generate-lockfile --offline >/dev/null 2>&1 || cargo generate-lockfile >/dev/null 2>&1)
expect_exit 1 "$check" expect-absent "$tmp/bad"
sed -i 's/, features = \["client"\]//' "$tmp/bad/Cargo.toml"
(cd "$tmp/bad" && cargo generate-lockfile --offline >/dev/null 2>&1 || cargo generate-lockfile >/dev/null 2>&1)
expect_exit 0 "$check" expect-absent "$tmp/bad"

# Command failures are not "absent".
expect_exit 2 "$check" expect-absent "$tmp/does-not-exist"
expect_exit 2 "$check" expect-absent "$crypto" --features no-such-feature
mkdir -p "$tmp/empty/src"; echo 'fn main() {}' > "$tmp/empty/src/main.rs"
printf '[package]\nname = "no-mls"\nversion = "0.0.0"\nedition = "2021"\n[workspace]\n' > "$tmp/empty/Cargo.toml"
(cd "$tmp/empty" && cargo generate-lockfile --offline >/dev/null 2>&1)
expect_exit 2 "$check" expect-absent "$tmp/empty"

exit "$fail"
