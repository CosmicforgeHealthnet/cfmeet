#!/usr/bin/env bash
# CF Meet: gated cutover of public API to the already-running Meeting Service.
# Modes: audit (read-only), cutover, rollback. No migrations or restarts.
set -Eeuo pipefail

MODE="audit"
if (( $# > 0 )); then MODE="$1"; fi
SITE="/etc/nginx/sites-available/api.cfmeet.com"
BACKUP_DIR="/var/backups/cfmeet-v2-cutover"
BACKUP_POINTER="$BACKUP_DIR/last-pre-cutover.conf"
BACKEND="http://127.0.0.1:18081"
PUBLIC="https://api.cfmeet.com"
CONTAINER="cfmeet-production-backend"

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
log() { printf '[cfmeet-v2] %s\n' "$*"; }
[[ "$MODE" =~ ^(audit|cutover|rollback)$ ]] || die "Usage: $0 audit|cutover|rollback"
[[ "$(id -u)" -eq 0 ]] || die "Run on the VPS as root."
[[ -f "$SITE" ]] || die "Nginx site missing: $SITE"
for cmd in curl python3 nginx systemctl docker; do
  command -v "$cmd" >/dev/null 2>&1 || die "Missing prerequisite: $cmd"
done

route_counts() {
  # Mixed upstreams can be intentional: Nginx can serve distinct locations from
  # distinct services. The audit must show context, not block on a raw count.
  python3 - "$SITE" "$MODE" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
mode = sys.argv[2]
old = r'proxy_pass\s+http://(?:127\.0\.0\.1|localhost):8080(?=[/;\s])'
new = r'proxy_pass\s+http://127\.0\.0\.1:18081(?=[/;\s])'
legacy = len(re.findall(old, text))
modern = len(re.findall(new, text))
print(f"Existing Nginx proxy_pass directives: legacy_8080={legacy}, meeting_v2_18081={modern}", flush=True)
print("Read-only site routing inventory (directive locations and anonymized upstream types):", flush=True)
current_location = "[server/global context]"
for line_number, raw_line in enumerate(text.splitlines(), 1):
    line = raw_line.split("#", 1)[0]
    location = re.search(r'^\s*location\s+(.+?)\s*\{', line)
    if location:
        # This is a diagnostic label, not a full Nginx precedence parser.
        current_location = location.group(1).strip()[:100]
        print(f"  line {line_number}: location {current_location}", flush=True)
    proxy = re.search(r'\bproxy_pass\s+([^\s;]+)', line)
    if proxy:
        destination = proxy.group(1)
        target = ("legacy:8080" if re.search(r'^http://(?:127\.0\.0\.1|localhost):8080(?:/|$)', destination)
                  else "meeting-v2:18081" if re.search(r'^http://127\.0\.0\.1:18081(?:/|$)', destination)
                  else "other/indirect (value redacted)")
        print(f"  line {line_number}: proxy_pass -> {target}; most_recent_location={current_location}", flush=True)
if not legacy and not modern:
    sys.exit("Neither expected upstream appears; manual mapping is needed.")
if mode == "cutover" and legacy != 1:
    sys.exit("Cutover requires exactly one legacy upstream in this verified site; refuse ambiguous changes.")
PY
}


require_active_nginx_site() {
  # Avoid editing a site file that exists but is not actually included in the
  # running Nginx configuration. Print metadata only, never full config/secrets.
  python3 - "$SITE" <<'PY'
import pathlib, re, subprocess, sys
target = pathlib.Path(sys.argv[1]).resolve(strict=True)
proc = subprocess.run(["nginx", "-T"], capture_output=True, text=True)
if proc.returncode:
    sys.exit("nginx -T failed; refusing to assume the site is active.")
configs = re.findall(r'(?m)^# configuration file ([^:\n]+):\s*  log "Testing independent backend at $BACKEND"
  [[ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null)" == "true" ]] \
    || die "New backend container is not running."
  curl -fsS --max-time 8 "$BACKEND/health" >/dev/null || die "New backend health check failed."
  local tmp status
  tmp="$(mktemp)"
  if ! curl -fsS --max-time 12 "$BACKEND/api-docs/openapi.json" -o "$tmp"; then
    rm -f "$tmp"
    die "New backend OpenAPI unavailable."
  fi
  if ! python3 - "$tmp" <<'PY'
import json, sys
with open(sys.argv[1]) as f: doc = json.load(f)
paths = doc.get("paths", {})
needed = [("get", "/api/v2/meeting-spaces"), ("post", "/api/v2/meeting-spaces"),
          ("post", "/api/v2/instant-meetings")]
for method, path in needed:
    if method not in paths.get(path, {}):
        sys.exit(f"Missing backend operation: {method.upper()} {path}")
if not any(p.startswith("/api/v1/") for p in paths):
    sys.exit("Legacy V1 routes missing.")
def check(obj):
    if isinstance(obj, dict):
        if "$ref" in obj:
            ref = obj["$ref"]
            if not ref.startswith("#/"): raise ValueError(f"Unsupported schema ref: {ref}")
            ptr = doc
            for part in ref[2:].split("/"):
                part = part.replace("~1", "/").replace("~0", "~")
                try: ptr = ptr[int(part)] if isinstance(ptr, list) else ptr[part]
                except (KeyError, IndexError, ValueError, TypeError):
                    raise ValueError(f"Unresolved schema ref: {ref}")
        for value in obj.values(): check(value)
    elif isinstance(obj, list):
        for value in obj: check(value)
check(doc)
print(f"OpenAPI verified: {len(paths)} paths, required V1/V2 operations, all local refs.")
PY
  then
    rm -f "$tmp"
    die "Backend OpenAPI contract failed."
  fi
  rm -f "$tmp"
  status="$(curl -sS --max-time 8 -o /dev/null -w '%{http_code}' "$BACKEND/api/v2/meeting-spaces")"
  [[ "$status" == "401" || "$status" == "403" ]] \
    || die "Unauthenticated new V2 endpoint returned $status (expected 401/403)."
  log "Backend V2 authentication boundary PASS ($status)."
}

verify_public() {
  curl -fsS --max-time 15 "$PUBLIC/health" >/dev/null || die "Public API health failed."
  local status
  status="$(curl -sS --max-time 15 -o /dev/null -w '%{http_code}' "$PUBLIC/api/v2/meeting-spaces")"
  [[ "$status" == "401" || "$status" == "403" ]] \
    || die "Public V2 endpoint returned $status, expected 401/403."
  curl -fsS --max-time 15 "$PUBLIC/api-docs/openapi.json" -o /dev/null \
    || die "Public OpenAPI unavailable."
  log "Public health, V2 auth and OpenAPI PASS."
}

restore_config() {
  local saved="$1"
  [[ -f "$saved" ]] || die "No rollback file: $saved"
  cp -a "$saved" "$SITE"
  nginx -t || die "Restored config failed Nginx validation: intervene immediately."
  systemctl reload nginx || die "Nginx failed to reload restored config."
  log "Restored prior Nginx config."
}

rollback_on_error() {
  local rc=$?
  trap - EXIT
  if (( rc != 0 )); then
    log "Cutover failed; restoring previous config from $BACKUP"
    restore_config "$BACKUP"
  fi
  exit "$rc"
}

route_counts
require_active_nginx_site
case "$MODE" in
  audit)
    nginx -t
    verify_backend
    local_code="$(local_tls_status)"
    public_code="$(curl --noproxy '*' -sS --max-time 15 -o /dev/null -w '%{http_code}' -H 'Cache-Control: no-cache' "$PUBLIC/api/v2/meeting-spaces" || true)"
    log "VPS-local TLS GET /api/v2/meeting-spaces returned HTTP $local_code (Host/SNI api.cfmeet.com)"
    log "External public DNS GET /api/v2/meeting-spaces returned HTTP $public_code"
    if [[ "$public_code" != "401" && "$public_code" != "403" ]]; then
      die "Public Meeting V2 is not healthy. Compare inventory above with active Nginx locations; do not cut over blindly."
    fi
    log "READ-ONLY V2 audit passed. Authenticated browser flows and real RTC are NOT certified."
    ;;
  cutover)
    verify_backend
    [[ ! -e "$BACKUP_POINTER" ]] || die "Previous cutover backup exists; review before repeating."
    mkdir -p "$BACKUP_DIR"
    chmod 700 "$BACKUP_DIR"
    BACKUP="$BACKUP_DIR/api.cfmeet.com.$(date -u +%Y%m%dT%H%M%SZ).conf"
    cp -a "$SITE" "$BACKUP"
    trap rollback_on_error EXIT
    python3 - "$SITE" <<'PY'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
lines = p.read_text().splitlines(keepends=True)
# Only the exact catch-all location / may be modified.
# Existing /swagger-ui/ and /api-docs/ proxies to port 18081 stay intact.
roots = [i for i, line in enumerate(lines)
         if re.match(r'^\s*location\s+/\s*\{\s*(?:#.*)?$', line)]
if len(roots) != 1:
    sys.exit(f"Expected one simple location / block, found {len(roots)}; no config changes.")
root = roots[0]
depth = 0
proxy_indices = []
for i in range(root, len(lines)):
    code = lines[i].split("#", 1)[0]
    if i == root:
        depth = code.count("{") - code.count("}")
        if depth != 1:
            sys.exit("Ambiguous catch-all block, no config changes.")
        continue
    if depth == 1 and re.search(r'^\s*proxy_pass\s+', code):
        proxy_indices.append(i)
    depth += code.count("{") - code.count("}")
    if depth == 0:
        break
    if depth < 0:
        sys.exit("Invalid block depth, no config changes.")
if depth != 0 or len(proxy_indices) != 1:
    sys.exit(f"Expected one proxy_pass directly inside location /; found {len(proxy_indices)}.")
target = proxy_indices[0]
pattern = r'(proxy_pass\s+)http://(?:localhost|127\.0\.0\.1):8080(?=[/;\s])'
updated, count = re.subn(pattern, r'\1http://127.0.0.1:18081', lines[target], count=1)
if count != 1:
    sys.exit("Catch-all upstream is not legacy port 8080; no config changes.")
lines[target] = updated
p.write_text("".join(lines))
print(f"Changed ONLY location / proxy_pass on line {target + 1}; all other Nginx routes unchanged.")
PY
    nginx -t
    systemctl reload nginx
    verify_public
    ln -s "$BACKUP" "$BACKUP_POINTER"
    trap - EXIT
    log "CUTOVER PASS. Backup: $BACKUP"
    log "REQUIRED NEXT: authenticated browser E2E (login, meeting create, schedule, join, LiveKit) and monitoring."
    ;;
  rollback)
    [[ -f "$BACKUP_POINTER" ]] || die "No pre-cutover backup."
    restore_config "$BACKUP_POINTER"
    rm -f "$BACKUP_POINTER"
    curl -fsS --max-time 12 "$PUBLIC/health" >/dev/null || die "Old public health failed."
    log "ROLLBACK PASS. Verify legacy meeting functionality."
    ;;
esac
, proc.stdout)
active = []
for config in configs:
    try:
        if pathlib.Path(config).resolve(strict=True) == target:
            active.append(config)
    except OSError:
        pass
print(f"Active Nginx include for api.cfmeet.com site: {'YES' if active else 'NO'} (matches={len(active)})", flush=True)
if not active:
    sys.exit("Target Nginx site is not loaded; editing it cannot fix public traffic.")
PY
}

local_tls_status() {
  # Resolve the production hostname to loopback, preserving Host and SNI and
  # bypassing external CDN/DNS and any HTTP_PROXY environment variable.
  curl --noproxy '*' --resolve 'api.cfmeet.com:443:127.0.0.1' \
    --silent --show-error --max-time 12 --output /dev/null \
    --write-out '%{http_code}' \
    --header 'Cache-Control: no-cache' \
    'https://api.cfmeet.com/api/v2/meeting-spaces' || true
}

verify_backend() {
  log "Testing independent backend at $BACKEND"
  [[ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null)" == "true" ]] \
    || die "New backend container is not running."
  curl -fsS --max-time 8 "$BACKEND/health" >/dev/null || die "New backend health check failed."
  local tmp status
  tmp="$(mktemp)"
  if ! curl -fsS --max-time 12 "$BACKEND/api-docs/openapi.json" -o "$tmp"; then
    rm -f "$tmp"
    die "New backend OpenAPI unavailable."
  fi
  if ! python3 - "$tmp" <<'PY'
import json, sys
with open(sys.argv[1]) as f: doc = json.load(f)
paths = doc.get("paths", {})
needed = [("get", "/api/v2/meeting-spaces"), ("post", "/api/v2/meeting-spaces"),
          ("post", "/api/v2/instant-meetings")]
for method, path in needed:
    if method not in paths.get(path, {}):
        sys.exit(f"Missing backend operation: {method.upper()} {path}")
if not any(p.startswith("/api/v1/") for p in paths):
    sys.exit("Legacy V1 routes missing.")
def check(obj):
    if isinstance(obj, dict):
        if "$ref" in obj:
            ref = obj["$ref"]
            if not ref.startswith("#/"): raise ValueError(f"Unsupported schema ref: {ref}")
            ptr = doc
            for part in ref[2:].split("/"):
                part = part.replace("~1", "/").replace("~0", "~")
                try: ptr = ptr[int(part)] if isinstance(ptr, list) else ptr[part]
                except (KeyError, IndexError, ValueError, TypeError):
                    raise ValueError(f"Unresolved schema ref: {ref}")
        for value in obj.values(): check(value)
    elif isinstance(obj, list):
        for value in obj: check(value)
check(doc)
print(f"OpenAPI verified: {len(paths)} paths, required V1/V2 operations, all local refs.")
PY
  then
    rm -f "$tmp"
    die "Backend OpenAPI contract failed."
  fi
  rm -f "$tmp"
  status="$(curl -sS --max-time 8 -o /dev/null -w '%{http_code}' "$BACKEND/api/v2/meeting-spaces")"
  [[ "$status" == "401" || "$status" == "403" ]] \
    || die "Unauthenticated new V2 endpoint returned $status (expected 401/403)."
  log "Backend V2 authentication boundary PASS ($status)."
}

verify_public() {
  curl -fsS --max-time 15 "$PUBLIC/health" >/dev/null || die "Public API health failed."
  local status
  status="$(curl -sS --max-time 15 -o /dev/null -w '%{http_code}' "$PUBLIC/api/v2/meeting-spaces")"
  [[ "$status" == "401" || "$status" == "403" ]] \
    || die "Public V2 endpoint returned $status, expected 401/403."
  curl -fsS --max-time 15 "$PUBLIC/api-docs/openapi.json" -o /dev/null \
    || die "Public OpenAPI unavailable."
  log "Public health, V2 auth and OpenAPI PASS."
}

restore_config() {
  local saved="$1"
  [[ -f "$saved" ]] || die "No rollback file: $saved"
  cp -a "$saved" "$SITE"
  nginx -t || die "Restored config failed Nginx validation: intervene immediately."
  systemctl reload nginx || die "Nginx failed to reload restored config."
  log "Restored prior Nginx config."
}

rollback_on_error() {
  local rc=$?
  trap - EXIT
  if (( rc != 0 )); then
    log "Cutover failed; restoring previous config from $BACKUP"
    restore_config "$BACKUP"
  fi
  exit "$rc"
}

route_counts
case "$MODE" in
  audit)
    nginx -t
    verify_backend
    public_code="$(curl -sS --max-time 15 -o /dev/null -w '%{http_code}' "$PUBLIC/api/v2/meeting-spaces" || true)"
    log "Public unauthenticated GET /api/v2/meeting-spaces returned HTTP $public_code"
    if [[ "$public_code" != "401" && "$public_code" != "403" ]]; then
      die "Public Meeting V2 is not healthy. Compare inventory above with active Nginx locations; do not cut over blindly."
    fi
    log "READ-ONLY V2 audit passed. Authenticated browser flows and real RTC are NOT certified."
    ;;
  cutover)
    verify_backend
    [[ ! -e "$BACKUP_POINTER" ]] || die "Previous cutover backup exists; review before repeating."
    mkdir -p "$BACKUP_DIR"
    chmod 700 "$BACKUP_DIR"
    BACKUP="$BACKUP_DIR/api.cfmeet.com.$(date -u +%Y%m%dT%H%M%SZ).conf"
    cp -a "$SITE" "$BACKUP"
    trap rollback_on_error EXIT
    python3 - "$SITE" <<'PY'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
lines = p.read_text().splitlines(keepends=True)
# Only the exact catch-all location / may be modified.
# Existing /swagger-ui/ and /api-docs/ proxies to port 18081 stay intact.
roots = [i for i, line in enumerate(lines)
         if re.match(r'^\s*location\s+/\s*\{\s*(?:#.*)?$', line)]
if len(roots) != 1:
    sys.exit(f"Expected one simple location / block, found {len(roots)}; no config changes.")
root = roots[0]
depth = 0
proxy_indices = []
for i in range(root, len(lines)):
    code = lines[i].split("#", 1)[0]
    if i == root:
        depth = code.count("{") - code.count("}")
        if depth != 1:
            sys.exit("Ambiguous catch-all block, no config changes.")
        continue
    if depth == 1 and re.search(r'^\s*proxy_pass\s+', code):
        proxy_indices.append(i)
    depth += code.count("{") - code.count("}")
    if depth == 0:
        break
    if depth < 0:
        sys.exit("Invalid block depth, no config changes.")
if depth != 0 or len(proxy_indices) != 1:
    sys.exit(f"Expected one proxy_pass directly inside location /; found {len(proxy_indices)}.")
target = proxy_indices[0]
pattern = r'(proxy_pass\s+)http://(?:localhost|127\.0\.0\.1):8080(?=[/;\s])'
updated, count = re.subn(pattern, r'\1http://127.0.0.1:18081', lines[target], count=1)
if count != 1:
    sys.exit("Catch-all upstream is not legacy port 8080; no config changes.")
lines[target] = updated
p.write_text("".join(lines))
print(f"Changed ONLY location / proxy_pass on line {target + 1}; all other Nginx routes unchanged.")
PY
    nginx -t
    systemctl reload nginx
    verify_public
    ln -s "$BACKUP" "$BACKUP_POINTER"
    trap - EXIT
    log "CUTOVER PASS. Backup: $BACKUP"
    log "REQUIRED NEXT: authenticated browser E2E (login, meeting create, schedule, join, LiveKit) and monitoring."
    ;;
  rollback)
    [[ -f "$BACKUP_POINTER" ]] || die "No pre-cutover backup."
    restore_config "$BACKUP_POINTER"
    rm -f "$BACKUP_POINTER"
    curl -fsS --max-time 12 "$PUBLIC/health" >/dev/null || die "Old public health failed."
    log "ROLLBACK PASS. Verify legacy meeting functionality."
    ;;
esac
