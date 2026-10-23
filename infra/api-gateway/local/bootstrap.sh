#!/usr/bin/env bash
# LOCAL-ONLY bootstrap of the Part VI services (Developer Platform + API Gateway).
#
#   infra/api-gateway/local/bootstrap.sh [--admin-url postgres://postgres:postgres@127.0.0.1:5432/postgres]
#
# * generates one Ed25519 signing key per service (runtime environment `local`) with
#   packages/service-auth's cf-service-keygen into infra/api-gateway/local/.generated/
#   (git-ignored; never commit it);
# * writes the Developer Platform's trust bundle (the gateway may forward;
#   developer-request.forward) and env files for both services;
# * with --admin-url, provisions the Developer Platform's OWN database and roles
#   (infra/deployment/postgres/provision-service-database.sh --service developer-platform);
# * prints the two issuer entries to ADD to your local Platform Access trust bundle
#   (Platform Access stays the authority: this script never edits its configuration).
#
# Nothing here is used by staging or production deployment.
set -euo pipefail

admin=""
while [ $# -gt 0 ]; do
  case "$1" in
    --admin-url) admin="$2"; shift 2 ;;
    *) echo "bootstrap: unknown argument $1" >&2; exit 2 ;;
  esac
done
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
out="$here/.generated"
mkdir -p "$out"
chmod 700 "$out"
keygen=(cargo run -q --manifest-path "$root/packages/service-auth/Cargo.toml" --bin cf-service-keygen --)

for svc in api-gateway developer-platform; do
  kid="$svc-local-1"
  [ -f "$out/$svc.pem" ] || "${keygen[@]}" generate --kid "$kid" --out "$out/$svc.pem" >/dev/null
  "${keygen[@]}" public-jwk --kid "$kid" --key "$out/$svc.pem" > "$out/$svc.jwk.json"
done

python3 - "$out" <<'PY'
import json, sys, pathlib
out = pathlib.Path(sys.argv[1])
jwk = lambda s: json.loads((out / f"{s}.jwk.json").read_text())
issuer = lambda i, k, p, a: {"issuer": i, "audience": a, "runtime_environment": "local", "keys": [jwk(k)], "permissions": p}
(out / "developer-platform-trust.json").write_text(json.dumps({
    "bundle_version": 1, "receiver": "cfmeet.developer-platform", "runtime_environment": "local",
    "trusted_issuers": [issuer("cfmeet.api-gateway", "api-gateway", ["developer-request.forward"], "cfmeet.developer-platform")],
}, indent=2) + "\n")
(out / "platform-access-issuers.json").write_text(json.dumps([
    issuer("cfmeet.api-gateway", "api-gateway", ["platform-credential.authenticate"], "cfmeet.platform-access"),
    issuer("cfmeet.developer-platform", "developer-platform", ["platform-identity.read"], "cfmeet.platform-access"),
], indent=2) + "\n")
PY

cat > "$out/developer-platform.env" <<ENV
DATABASE_URL=postgres://developer_platform:<password>@127.0.0.1:5432/cfmeet_developer_platform
DEVELOPER_PLATFORM_BIND_ADDRESS=127.0.0.1:8098
DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=false
CF_RUNTIME_ENVIRONMENT=local
CF_SERVICE_AUTH_KEY_ID=developer-platform-local-1
CF_SERVICE_AUTH_PRIVATE_KEY_FILE=$out/developer-platform.pem
CF_SERVICE_TRUST_BUNDLE_FILE=$out/developer-platform-trust.json
CF_PLATFORM_ACCESS_BASE_URL=http://127.0.0.1:8094
ENV
cat > "$out/api-gateway.env" <<ENV
API_GATEWAY_BIND_ADDRESS=127.0.0.1:8099
API_GATEWAY_PUBLIC_ROUTING_ENABLED=false
API_GATEWAY_WEBSOCKETS_ENABLED=false
CF_RUNTIME_ENVIRONMENT=local
CF_SERVICE_AUTH_KEY_ID=api-gateway-local-1
CF_SERVICE_AUTH_PRIVATE_KEY_FILE=$out/api-gateway.pem
CF_PLATFORM_ACCESS_BASE_URL=http://127.0.0.1:8094
CF_DEVELOPER_PLATFORM_BASE_URL=http://127.0.0.1:8098
ENV

# Container variants (paths inside /run/cfmeet, host services via host.docker.internal).
sed -e "s#$out#/run/cfmeet#g" -e "s#127.0.0.1:8094#host.docker.internal:8094#g" -e "s#@127.0.0.1:5432#@host.docker.internal:5432#g" \
  -e "/_BIND_ADDRESS=/d" "$out/developer-platform.env" > "$out/developer-platform.container.env"
sed -e "s#$out#/run/cfmeet#g" -e "s#127.0.0.1:8094#host.docker.internal:8094#g" \
  -e "/_BIND_ADDRESS=/d" -e "/CF_DEVELOPER_PLATFORM_BASE_URL/d" "$out/api-gateway.env" > "$out/api-gateway.container.env"

if [ -n "$admin" ]; then
  for f in migrator runtime; do
    [ -f "$out/devplat-$f.password" ] || (od -An -N24 -tx1 /dev/urandom | tr -d ' \n' > "$out/devplat-$f.password")
    chmod 600 "$out/devplat-$f.password"
  done
  "$root/infra/deployment/postgres/provision-service-database.sh" --service developer-platform \
    --admin-url "$admin" \
    --migrator-password-file "$out/devplat-migrator.password" \
    --runtime-password-file "$out/devplat-runtime.password"
  echo "bootstrap: provisioned cfmeet_developer_platform (passwords in $out/devplat-*.password)"
fi

echo "bootstrap: keys, Developer Platform trust bundle and env files in $out"
echo "bootstrap: add the issuers in $out/platform-access-issuers.json to your LOCAL Platform Access trust bundle"
