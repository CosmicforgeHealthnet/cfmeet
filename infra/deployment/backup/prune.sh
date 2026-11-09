#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Local retention for backup run directories.
#
#   prune.sh --dir D --keep-days N --keep-min M [--dry-run]
#
# Considers only immediate sub-directories of D whose names start with a UTC stamp
# (YYYYMMDDTHHMMSSZ, optionally followed by -<label>) AND that hold a manifest or a run
# summary written by this tooling; anything else is left alone. The M newest are always
# kept; of the rest, those whose stamp is older than N days are deleted. Off-host copies are
# never pruned from here (remote retention belongs to the target).
#
# Exit status: 0 ok; 2 usage.
set -euo pipefail
CFB_PROG=prune.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

dir="" keep_days="" keep_min="" dry=0
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) dir="$2"; shift 2 ;;
    --keep-days) keep_days="$2"; shift 2 ;;
    --keep-min) keep_min="$2"; shift 2 ;;
    --dry-run) dry=1; shift ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
[[ "$keep_days" =~ ^[0-9]+$ ]] && [[ "$keep_min" =~ ^[0-9]+$ ]] || cfb_die 2 "--keep-days and --keep-min must be whole numbers"
[ "$keep_min" -ge 1 ] || cfb_die 2 "--keep-min must be at least 1 (never prune every backup)"
[ -n "$dir" ] && [ "$dir" != / ] || cfb_die 2 "--dir is required"
[ -d "$dir" ] || exit 0

now="$(date -u +%s)"
cutoff=$((now - keep_days * 86400))
kept=0
while IFS= read -r name; do
  path="$dir/$name"
  [ -n "$(find "$path" -maxdepth 2 -type f \( -name '*.manifest.json' -o -name 'run-summary.json' \) -print -quit)" ] || continue
  if [ "$kept" -lt "$keep_min" ]; then
    kept=$((kept + 1))
    continue
  fi
  stamp="${name:0:16}"
  epoch="$(date -u -d "${stamp:0:4}-${stamp:4:2}-${stamp:6:2}T${stamp:9:2}:${stamp:11:2}:${stamp:13:2}Z" +%s 2>/dev/null)" || continue
  if [ "$epoch" -lt "$cutoff" ]; then
    if [ "$dry" -eq 1 ]; then
      echo "would prune $path"
    else
      rm -rf -- "$path"
      echo "pruned $path"
    fi
  fi
done < <(find "$dir" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | grep -E '^[0-9]{8}T[0-9]{6}Z(-[A-Za-z0-9._:-]+)?$' | sort -r)
