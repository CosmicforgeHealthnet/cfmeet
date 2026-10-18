#!/usr/bin/env bash
# Asserts, from Cargo's resolved dependency graph, whether a crate links the CLIENT half of
# packages/messaging-crypto (MLS private-key generation: the `client` / `web` features and
# the openmls_basic_credential crate). The Messaging backend must never link it (ADR 0045).
#
#   check-backend-crypto-graph.sh <expect-absent|expect-present> <manifest-dir> [cargo tree args...]
#
# Exit codes: 0 = the expectation holds; 1 = the expectation is violated; 2 = the graph could
# not be produced (cargo failed, or the output is not a graph that contains OpenMLS) — a
# command failure is never mistaken for "dependency absent".
set -uo pipefail

expect="${1:?expect-absent|expect-present}"
dir="${2:?manifest directory}"
shift 2
case "$expect" in expect-absent|expect-present) ;; *) echo "unknown expectation: $expect" >&2; exit 2 ;; esac

# The whole graph (not an inverse query, which prints "nothing to print" or "did not match"
# depending on the Cargo version), normal edges only (what ships), every target platform,
# with the features enabled on each package.
graph="$(cd "$dir" && cargo tree --locked -e normal --target all --prefix none -f '{p}|{f}' "$@")"
status=$?
if [ "$status" -ne 0 ]; then
  echo "::error::cargo tree failed (exit $status) in $dir" >&2
  exit 2
fi
if ! grep -qE '^openmls v[0-9]' <<<"$graph"; then
  echo "::error::the dependency graph of $dir does not contain openmls: refusing to conclude anything" >&2
  exit 2
fi

violations="$(
  grep -E '^openmls_basic_credential v[0-9]' <<<"$graph"
  grep -E '^cf-messaging-crypto v[^|]*\|(.*,)?(client|web)(,|$| )' <<<"$graph"
)"
violations="$(sort -u <<<"$violations" | sed '/^$/d')"

if [ "$expect" = expect-absent ]; then
  if [ -n "$violations" ]; then
    echo "::error::$dir links the client key-generation half of the messaging crypto core:" >&2
    echo "$violations" >&2
    exit 1
  fi
  echo "OK: $dir does not link client key generation ($*)"
else
  if [ -z "$violations" ]; then
    echo "::error::expected $dir to link client key generation ($*), but it does not" >&2
    exit 1
  fi
  echo "OK: client key generation detected in $dir ($*)"
fi
