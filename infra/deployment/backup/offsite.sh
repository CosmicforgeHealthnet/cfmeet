#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Pluggable off-host copy of ENCRYPTED backup files.
#
#   offsite.sh status                     prints OFFSITE_KIND=<kind> (no secrets)
#   offsite.sh push --root ROOT --path REL  copies ROOT/REL to <target>/<source>/REL and verifies
#
# The target is configured by environment variables (normally from backup.env):
#
#   CFMEET_BACKUP_OFFSITE_KIND        none (default) | rsync | rclone | dir
#   CFMEET_BACKUP_SOURCE_NAME         folder name for this host at the target (default: hostname)
#   rsync  CFMEET_BACKUP_OFFSITE_RSYNC_DEST         user@host:/path (owner-provided host)
#          CFMEET_BACKUP_OFFSITE_SSH_KEY            private key file dedicated to backups
#          CFMEET_BACKUP_OFFSITE_SSH_KNOWN_HOSTS    pinned host key file (required for remote)
#          CFMEET_BACKUP_OFFSITE_SSH_PORT           optional, default 22
#   rclone CFMEET_BACKUP_OFFSITE_RCLONE_REMOTE      remote:path (any non-AWS rclone backend)
#          CFMEET_BACKUP_OFFSITE_RCLONE_CONFIG      rclone.conf file (mode 0600)
#   dir    CFMEET_BACKUP_OFFSITE_DIR                a mounted path. Counts as off-host ONLY if
#                                                   the mount is on independent hardware; the
#                                                   CI drill uses it to simulate a remote.
#
# Only *.age, *.sha256, *.manifest.json and run-summary.json files are ever sent: a
# directory holding anything else (e.g. a plaintext .dump) is refused. Nothing is deleted at
# the target; remote retention belongs to the target (prefer append-only credentials so a
# compromised production host cannot erase its own backups).
#
# Output: OFFSITE_STATUS=COPIED | NOT_CONFIGURED | FAILED on stdout.
# Exit status: 0 copied and verified, or NOT_CONFIGURED (kind none); 2 usage / config;
# 3 refused (unencrypted or unexpected file); 4 copy or verification failed.
set -euo pipefail
CFB_PROG=offsite.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

kind="${CFMEET_BACKUP_OFFSITE_KIND:-none}"
source_name="${CFMEET_BACKUP_SOURCE_NAME:-$(hostname -s 2>/dev/null || hostname)}"
[[ "$source_name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$ ]] || cfb_die 2 "CFMEET_BACKUP_SOURCE_NAME is not a simple name"

command="${1:-}"
[ $# -eq 0 ] || shift
case "$command" in
  status)
    echo "OFFSITE_KIND=$kind"
    exit 0
    ;;
  push) ;;
  *) cfb_die 2 "usage: offsite.sh status | push --root ROOT --path REL" ;;
esac

root="" rel=""
while [ $# -gt 0 ]; do
  case "$1" in
    --root) root="$2"; shift 2 ;;
    --path) rel="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
[ -d "$root" ] && [ -n "$rel" ] && [ -d "$root/$rel" ] || cfb_die 2 "push needs --root ROOT --path REL (an existing directory under ROOT)"
[[ "$rel" =~ ^[A-Za-z0-9._/-]+$ ]] && [[ "$rel" != *..* ]] && [[ "$rel" != /* ]] || cfb_die 2 "--path must be a relative path without '..'"

if [ "$kind" = none ]; then
  echo "OFFSITE_STATUS=NOT_CONFIGURED"
  echo "OFFSITE_NOTE=$CFB_LOCAL_ONLY_NOTE"
  exit 0
fi

# Refuse anything that is not encrypted or integrity metadata.
while IFS= read -r -d '' file; do
  case "$(basename "$file")" in
    *.age | *.sha256 | *.manifest.json | run-summary.json) ;;
    *) cfb_die 3 "refusing to send $file off-host: only encrypted archives and their checksums / manifests leave this host" ;;
  esac
done < <(find "$root/$rel" -type f -print0)
while IFS= read -r -d '' file; do
  [ "$(head -c 21 "$file")" = "age-encryption.org/v1" ] || cfb_die 3 "refusing $file: not an age-encrypted file"
done < <(find "$root/$rel" -type f -name '*.age' -print0)

failed() {
  echo "OFFSITE_STATUS=FAILED"
  cfb_die 4 "$*"
}

case "$kind" in
  dir)
    base="${CFMEET_BACKUP_OFFSITE_DIR:-}"
    [ -n "$base" ] && [ -d "$base" ] || cfb_die 2 "CFMEET_BACKUP_OFFSITE_DIR must name an existing directory"
    base="$(cd "$base" && pwd)"
    dest="$base/$source_name/$rel"
    mkdir -p "$dest"
    (cd "$root/$rel" && find . -type f -print0 | while IFS= read -r -d '' f; do
      mkdir -p "$dest/$(dirname "$f")"
      cp -p "$f" "$dest/$f"
    done) || failed "copy to the off-host directory failed"
    while IFS= read -r -d '' f; do
      [ "$(cfb_sha256 "$root/$rel/$f")" = "$(cfb_sha256 "$dest/$f")" ] || failed "verification failed for $f"
    done < <(cd "$root/$rel" && find . -type f -print0)
    ;;
  rsync)
    cfb_require_tools rsync
    dest="${CFMEET_BACKUP_OFFSITE_RSYNC_DEST:-}"
    [ -n "$dest" ] || cfb_die 2 "CFMEET_BACKUP_OFFSITE_RSYNC_DEST is required for kind rsync"
    rsh=(ssh -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=20
      -o ServerAliveInterval=15 -o ServerAliveCountMax=8 -p "${CFMEET_BACKUP_OFFSITE_SSH_PORT:-22}")
    if [[ "$dest" == *:* ]]; then
      key="${CFMEET_BACKUP_OFFSITE_SSH_KEY:-}"
      known="${CFMEET_BACKUP_OFFSITE_SSH_KNOWN_HOSTS:-}"
      [ -r "$key" ] || cfb_die 2 "CFMEET_BACKUP_OFFSITE_SSH_KEY must name a readable key file"
      [ -s "$known" ] || cfb_die 2 "CFMEET_BACKUP_OFFSITE_SSH_KNOWN_HOSTS must name a pinned known_hosts file"
      rsh+=(-i "$key" -o "UserKnownHostsFile=$known")
    fi
    target="${dest%/}/$source_name/$rel/"
    rsync -a --mkpath --chmod=D0700,F0600 -e "${rsh[*]}" "$root/$rel/" "$target" ||
      failed "rsync to the off-host target failed"
    # Verification: a checksum dry run must find nothing left to transfer.
    pending="$(rsync -a --checksum --dry-run --itemize-changes --chmod=D0700,F0600 -e "${rsh[*]}" "$root/$rel/" "$target" | grep -v '^\.d' || true)"
    [ -z "$pending" ] || failed "off-host copy differs from the local files after rsync"
    ;;
  rclone)
    cfb_require_tools rclone
    remote="${CFMEET_BACKUP_OFFSITE_RCLONE_REMOTE:-}"
    config="${CFMEET_BACKUP_OFFSITE_RCLONE_CONFIG:-}"
    [ -n "$remote" ] || cfb_die 2 "CFMEET_BACKUP_OFFSITE_RCLONE_REMOTE is required for kind rclone"
    [ -r "$config" ] || cfb_die 2 "CFMEET_BACKUP_OFFSITE_RCLONE_CONFIG must name a readable rclone.conf"
    target="${remote%/}/$source_name/$rel"
    rclone --config "$config" copy --quiet "$root/$rel" "$target" || failed "rclone copy failed"
    rclone --config "$config" check --quiet --one-way "$root/$rel" "$target" || failed "rclone check found differences"
    ;;
  *) cfb_die 2 "CFMEET_BACKUP_OFFSITE_KIND must be none, rsync, rclone or dir" ;;
esac
echo "OFFSITE_STATUS=COPIED"
echo "OFFSITE_KIND=$kind"
