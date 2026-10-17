#!/usr/bin/env bash
# Blocking "no NEW formatting debt" gate (Part M §47).
#
# `fmt-baseline.txt` lists the files whose rustfmt drift pre-dates Phase 2 (imported
# unchanged from rtc_backend). Any other file with drift fails the check; a baseline
# file that becomes clean is reported so the baseline can shrink.
set -euo pipefail
cd "$(dirname "$0")/../.."
baseline=scripts/certification/fmt-baseline.txt
current="$(cargo fmt --all -- --check -l 2>/dev/null | sed "s#^$PWD/##" | sort -u || true)"
new="$(comm -13 <(sort -u "$baseline") <(printf '%s\n' "$current" | sed '/^$/d'))"
cleaned="$(comm -23 <(sort -u "$baseline") <(printf '%s\n' "$current" | sed '/^$/d'))"
count="$(printf '%s\n' "$current" | sed '/^$/d' | wc -l | tr -d ' ')"
if [[ -n "$cleaned" ]]; then
  echo "Baseline files that are now rustfmt-clean (remove them from $baseline):"
  echo "$cleaned"
fi
if [[ -n "$new" ]]; then
  echo "NEW rustfmt drift outside the documented baseline:"
  echo "$new"
  echo "Run: rustfmt --edition 2021 <file>"
  # Show the exact diff for new files without altering the documented legacy baseline.
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    rustfmt --edition 2021 --check "$file" 2>&1 || true
  done <<< "$new"
  exit 1
fi
echo "rustfmt: ${count} documented baseline file(s) with pre-existing drift; no new drift."
