#!/usr/bin/env bash
# SEC-015: asserts that each given migration binary refuses destructive subcommands
# (down, fresh, refresh, reset) outside a non-production APP_ENV unless
# CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand, before any database connection.
#
# No database is needed: DATABASE_URL names a scheme no driver supports, so a binary that
# got past the guard fails with a driver error instead of the refusal (exit status 2).
#
# Usage: scripts/migrations/assert_destructive_guard.sh <migration-binary>...
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <migration-binary>..." >&2
  exit 64
fi

refusal="SEC-015: refusing to run destructive migration subcommand"
url="sec015-no-such-driver://guard-check/none"
cwd="$(mktemp -d)" # empty: no .env can add variables to the cleared environment
trap 'rm -rf "$cwd"' EXIT
failures=0

# run <binary> <env assignments...> -- <args...>: prints "<exit status>" then the output.
run() {
  local bin="$1"
  shift
  local -a vars=()
  while [[ "$1" != "--" ]]; do
    vars+=("$1")
    shift
  done
  shift
  local out status=0
  out="$(cd "$cwd" && env -i PATH="$PATH" DATABASE_URL="$url" "${vars[@]}" "$bin" "$@" 2>&1)" || status=$?
  printf '%s\n%s' "$status" "$out"
}

expect_refused() {
  local bin="$1" result status
  shift
  result="$(run "$bin" "$@")"
  status="${result%%$'\n'*}"
  if [[ "$status" != 2 || "$result" != *"$refusal"* || "$result" == *"no-such-driver"* ]]; then
    echo "FAIL: $(basename "$bin") $* was not refused (status $status):" >&2
    echo "${result#*$'\n'}" >&2
    failures=$((failures + 1))
  fi
}

expect_passed() {
  local bin="$1" result status
  shift
  result="$(run "$bin" "$@")"
  status="${result%%$'\n'*}"
  if [[ "$status" == 0 || "$status" == 2 || "$result" == *"$refusal"* || "$result" != *"no-such-driver"* ]]; then
    echo "FAIL: $(basename "$bin") $* did not pass the guard to the database layer (status $status):" >&2
    echo "${result#*$'\n'}" >&2
    failures=$((failures + 1))
  fi
}

for bin in "$@"; do
  bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
  [[ -x "$bin" ]] || { echo "FAIL: $bin is not an executable" >&2; exit 1; }
  before=$failures
  for sub in reset fresh refresh down; do
    expect_refused "$bin" -- "$sub"
    expect_refused "$bin" APP_ENV=production -- "$sub"
    expect_refused "$bin" APP_ENV=staging -- "$sub"
    expect_refused "$bin" APP_ENV=production -- -u "$url" "$sub"
  done
  expect_refused "$bin" APP_ENV=production -- down -n 1
  expect_refused "$bin" APP_ENV=production CF_MIGRATION_ALLOW_DESTRUCTIVE=down -- fresh
  expect_passed "$bin" APP_ENV=production CF_MIGRATION_ALLOW_DESTRUCTIVE=down -- down -n 1
  expect_passed "$bin" APP_ENV=test -- reset
  expect_passed "$bin" APP_ENV=production -- status
  if [[ $failures -eq $before ]]; then
    echo "ok: $(basename "$bin") refuses destructive subcommands outside a non-production APP_ENV"
  fi
done

if [[ $failures -gt 0 ]]; then
  echo "$failures destructive-guard assertion(s) failed" >&2
  exit 1
fi
