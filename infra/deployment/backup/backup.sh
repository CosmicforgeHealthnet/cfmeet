#!/usr/bin/env bash
# Logical backup of ONE service-owned PostgreSQL database (Calling, Platform Access,
# Numbering, Privacy Relay, Telephony or the SMS Gateway). Each service is backed up on its own; there is no shared platform dump.
#
#   backup.sh --service <calling|platform-access|numbering|privacy-relay|telephony|sms-gateway> --database-url <url> --out <file.dump>
#
# Writes <file.dump> (pg_dump custom format, no owner / privilege statements so it restores
# into a role-separated target) and <file.dump.sha256>. Key material is never part of a
# database backup: Contact Vault keyrings, provider secrets and signing keys live in files
# outside PostgreSQL and are backed up (or re-provisioned) through the secret store.
set -euo pipefail

service="" url="" out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --service) service="$2"; shift 2 ;;
    --database-url) url="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "backup.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
case "$service" in
  calling|platform-access|numbering|privacy-relay|telephony|sms-gateway) ;;
  *) echo "backup.sh: --service must be calling, platform-access, numbering, privacy-relay, telephony or sms-gateway" >&2; exit 2 ;;
esac
[ -n "$url" ] && [ -n "$out" ] || { echo "backup.sh: --database-url and --out are required" >&2; exit 2; }
command -v pg_dump >/dev/null || { echo "backup.sh: pg_dump not found" >&2; exit 3; }

# The migration-history table proves the dump belongs to the named service.
history_table="$(echo "$service" | tr '-' '_')_seaql_migrations"
has_history="$(psql "$url" -XAtq -v ON_ERROR_STOP=1 \
  -c "SELECT count(*) FROM pg_tables WHERE schemaname = 'public' AND tablename = '$history_table'")"
if [ "$has_history" != "1" ]; then
  echo "backup.sh: $history_table not found — this is not the $service database" >&2
  exit 4
fi

umask 077
tmp="$out.partial"
pg_dump --format=custom --no-owner --no-privileges --file="$tmp" "$url"
mv "$tmp" "$out"
( cd "$(dirname "$out")" && sha256sum "$(basename "$out")" > "$(basename "$out").sha256" )
echo "backup.sh: $service -> $out"
