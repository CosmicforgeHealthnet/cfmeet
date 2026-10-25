#!/usr/bin/env bash
# Part VI Chapter 21 deployment preflight (read-only; changes nothing).
#
#   preflight.sh --dp-env FILE --gateway-env FILE [--migrator-env FILE] [--bin-dir DIR]
#                [--expect dark|any] [--dp-url URL] [--gateway-url URL]
#
# 1. `developer-platform --check-config` and `api-gateway --check-config` with the env
#    files the deployment will use: configuration, key / trust-bundle files and the route
#    catalogue are valid in THIS environment (no network, no database).
# 2. With --expect dark (default) every rollout control must be OFF — a release never
#    turns a control on by accident; switching one is a separate, reviewed change.
# 3. `developer-platform-migration status` with the MIGRATOR env file (--migrator-env,
#    default: the DP env file): nothing pending — the API and worker refuse to start
#    otherwise. The migration CLI needs the migrator (owner) role; the runtime role reads
#    the migration state read-only and reports it on /ready (step 4).
# 4. With --dp-url / --gateway-url: their /ready answers 200 (the Developer Platform: DB
#    reachable and migrations current; the gateway: Platform Access reachable).
#
# Exit 0 only when every check passes. Secrets are never printed.
set -euo pipefail

dp_env="" gw_env="" mig_env="" bin_dir="${CFMEET_BIN_DIR:-/app}" expect="dark" dp_url="" gw_url=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dp-env) dp_env="$2"; shift 2 ;;
    --gateway-env) gw_env="$2"; shift 2 ;;
    --migrator-env) mig_env="$2"; shift 2 ;;
    --bin-dir) bin_dir="$2"; shift 2 ;;
    --expect) expect="$2"; shift 2 ;;
    --dp-url) dp_url="$2"; shift 2 ;;
    --gateway-url) gw_url="$2"; shift 2 ;;
    *) echo "preflight: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$dp_env" ] && [ -n "$gw_env" ] || { echo "preflight: --dp-env and --gateway-env are required" >&2; exit 2; }
[ -n "$mig_env" ] || mig_env="$dp_env"
case "$expect" in dark|any) ;; *) echo "preflight: --expect dark|any" >&2; exit 2 ;; esac

fail=0
pass() { echo "PASS  $*"; }
bad() { echo "FAIL  $*"; fail=1; }

# Runs a binary with exactly the variables of a dotenv file (KEY=VALUE lines, no shell
# expansion; the inherited environment is NOT passed on, PATH aside).
with_env() {
  python3 -c '
import os, subprocess, sys
env = {"PATH": os.environ.get("PATH", "")}
for raw in open(sys.argv[1], encoding="utf-8"):
    line = raw.strip()
    if not line or line.startswith("#") or "=" not in line:
        continue
    key, value = line.split("=", 1)
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"\x27":
        value = value[1:-1]
    env[key.strip()] = value
sys.exit(subprocess.run(sys.argv[2:], env=env).returncode)' "$@"
}

check() {
  local name="$1" bin="$2" file="$3" out
  if out="$(with_env "$file" "$bin" --check-config 2>/dev/null)"; then
    pass "$name configuration valid"
    if [ "$expect" = dark ]; then
      if python3 -c '
import json, sys
c = json.loads(sys.argv[1])["controls"]
on = [k for k, v in c.items() if v is True or (k.endswith("DELIVERY_MODE") and str(v).lower() != "shadow")]
print("\n".join(on)); sys.exit(1 if on else 0)' "$out" >/tmp/preflight.$$ 2>&1; then
        pass "$name rollout controls all off"
      else
        bad "$name rollout controls ON: $(tr '\n' ' ' </tmp/preflight.$$)"
      fi
      rm -f /tmp/preflight.$$
    fi
  else
    bad "$name configuration refused (run '$bin --check-config' with the env file for the reason)"
  fi
}

check developer-platform "$bin_dir/developer-platform" "$dp_env"
check api-gateway "$bin_dir/api-gateway" "$gw_env"

if status="$(with_env "$mig_env" "$bin_dir/developer-platform-migration" status 2>&1)"; then
  if echo "$status" | grep -q "Pending"; then
    bad "developer-platform migrations pending: $(echo "$status" | grep -c Pending)"
  else
    pass "developer-platform migrations current"
  fi
else
  bad "developer-platform migration status unavailable (database unreachable?)"
fi

probe() {
  local name="$1" url="$2" code
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$url/ready" || true)"
  if [ "$code" = 200 ]; then pass "$name ready"; else bad "$name not ready ($code)"; fi
}
[ -z "$dp_url" ] || probe developer-platform "$dp_url"
[ -z "$gw_url" ] || probe api-gateway "$gw_url"

exit "$fail"
