#!/usr/bin/env bash
# Starts the REAL security scan gateway (test runtime) in front of a real clamd for CI.
#
#   scripts/security-scan/start-ci-gateway.sh <gateway-binary> <clamd host:port> <workdir>
#
# Generates throwaway CI keys (a cfmeet.messaging test signer and a verdict key), a
# trust bundle and a client registry, starts the gateway on 127.0.0.1:18099 and waits
# until it is ready (engine reachable, signatures fresh). Appends the caller-side
# MESSAGING_TEST_SCAN_GATEWAY_* variables to $GITHUB_ENV when set, else prints them.
# Nothing here is a production secret.
set -euo pipefail
bin="$1"; clamd="$2"; dir="$3"
mkdir -p "$dir"; chmod 700 "$dir"
openssl genpkey -algorithm ed25519 -out "$dir/messaging.pem" 2>/dev/null
x=$(openssl pkey -in "$dir/messaging.pem" -pubout -outform DER | tail -c 32 | base64 | tr '+/' '-_' | tr -d '=\n')
cat > "$dir/trust.json" <<JSON
{"bundle_version":1,"receiver":"cfmeet.security-scan","runtime_environment":"test",
 "trusted_issuers":[{"issuer":"cfmeet.messaging","audience":"cfmeet.security-scan","runtime_environment":"test",
   "keys":[{"kid":"messaging-ci-scan-1","kty":"OKP","crv":"Ed25519","x":"$x"}],
   "permissions":["security-scan.submit"]}]}
JSON
cat > "$dir/clients.json" <<JSON
{"version":1,"environment":"test","clients":[{"issuer":"cfmeet.messaging","product":"cfmeet",
  "maxBytes":26214400,"maxConcurrent":2,"maxPerMinute":600}]}
JSON
"$(dirname "$bin")/security-scan-gateway-keygen" scan-verdict-ci-1 "$dir/verdict.pem" > "$dir/verdict-keys.json"
CF_RUNTIME_ENVIRONMENT=test \
CF_SERVICE_TRUST_BUNDLE_FILE="$dir/trust.json" \
SCAN_GATEWAY_CLIENTS_FILE="$dir/clients.json" \
SCAN_GATEWAY_VERDICT_KEY_FILE="$dir/verdict.pem" \
SCAN_GATEWAY_VERDICT_KEY_ID=scan-verdict-ci-1 \
SCAN_GATEWAY_BACKEND_ADDRESS="$clamd" \
SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES=26214400 \
SCAN_GATEWAY_BIND_ADDRESS=127.0.0.1:18099 \
SCAN_GATEWAY_METRICS_ADDRESS=127.0.0.1:19199 \
  nohup "$bin" > "$dir/gateway.log" 2>&1 &
for _ in $(seq 1 90); do
  if curl -fsS http://127.0.0.1:18099/ready > "$dir/ready.json" 2>/dev/null; then
    cat "$dir/ready.json"; echo
    vars="MESSAGING_TEST_SCAN_GATEWAY_URL=http://127.0.0.1:18099
MESSAGING_TEST_SCAN_GATEWAY_KEYS_FILE=$dir/verdict-keys.json
MESSAGING_TEST_SCAN_GATEWAY_SIGNER_KEY_FILE=$dir/messaging.pem
MESSAGING_TEST_SCAN_GATEWAY_SIGNER_KID=messaging-ci-scan-1"
    if [[ -n "${GITHUB_ENV:-}" ]]; then printf '%s\n' "$vars" >> "$GITHUB_ENV"; else printf '%s\n' "$vars"; fi
    exit 0
  fi
  sleep 2
done
cat "$dir/gateway.log"; curl -sS http://127.0.0.1:18099/ready || true
exit 1
