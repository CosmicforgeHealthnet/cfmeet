# Admin bridge runbook (HQ → CF Meet, ADR 0068)

The admin bridge (`services/admin-bridge`, identity `cfmeet.admin-bridge`) is the only doorway from the
CosmicForge central admin (HQ) into CF Meet. Design: `docs/admin/cfmeet-admin-bridge.md`. Contract HQ pins:
`services/admin-bridge/docs/contract/`.

## 1. What `release` deploys (dark)

- Container `cfmeet-admin-bridge`, private network only, no published port, 128 MiB, uid 10001, read-only, no capabilities.
- Database `cfmeet_admin_bridge` on `cfmeet-core-db`. The migrator `admin_bridge_migrator` owns every table.
  - After every bridge migration the tool narrows the runtime role `admin_bridge` to `deploy/db-roles.sql`'s grants (`ADMIN_BRIDGE_GRANTS_SQL`).
  - The runtime role cannot UPDATE, DELETE or TRUNCATE the audit, revocations or policy revisions, and owns nothing.
  - If that check fails, the release fails and the step is rolled back.
- Environment (`ADMIN_BRIDGE_DARK_ENV`):
  - `ADMIN_BRIDGE_ENABLED=false`, `ADMIN_BRIDGE_WRITES_ENABLED=false`, and every `ADMIN_BRIDGE_MODULE_*=false`;
  - no `CF_<OWNER>_BASE_URL`;
  - the ops listener on `127.0.0.1:8190`;
  - the HQ mTLS listener is not started while disabled.
- Readiness: `GET http://127.0.0.1:8190/ready` (in the container's network namespace) returns `{"ready":true,"enabled":false,"database":"OK",…}`.
- Live certification checks that the ops listener does **not** serve `/internal/cfmeet-admin/v1/*` (404).
- Safe defaults refuse `ADMIN_BRIDGE_ENABLED=true`, `ADMIN_BRIDGE_WRITES_ENABLED=true`, and any module switch whose activation is not recorded. A hand-edited container therefore fails the next `release` / `verify`.

## 2. `activate-admin-bridge` (phase A2.0)

Prerequisites: the bridge is released, its signing identity exists, and Organizations' private TLS route is installed.

1. Grants the bridge, in the **Organizations** trust bundle, `organization.administer` and `organization.operator-read`. The bundle is merged: no issuer, key or grant is dropped.
2. Sets `CF_ORGANIZATIONS_BASE_URL` (the private TLS route) and `ADMIN_BRIDGE_MODULE_ORGANIZATIONS=true` on the bridge.
3. Any failure restores both containers.

The bridge **stays disabled**.

### 2.1 The other owners (one activation per owner family)

Run each after `activate-admin-bridge`, in any order. Each operation does three things:
- Grants the bridge the permissions that owner's bridge routes check, in the owner's trust bundle. The bundle is merged: nothing is dropped.
- Sets the owner's private TLS base URL on the bridge.
- Switches on that owner's modules.

None of them touches `ADMIN_BRIDGE_ENABLED` or `ADMIN_BRIDGE_WRITES_ENABLED`. Any failure restores every container. Safe defaults refuse a module that is switched on without its activation.

| Operation | Owner grants | Bridge settings |
|---|---|---|
| `activate-admin-bridge-notifications` | both Notifications replicas: `notifications.administer`, `notifications-observability.read` | `CF_NOTIFICATIONS_BASE_URL`; `NOTIFICATIONS` |
| `activate-admin-bridge-partners` | Developer Platform: `developer-platform.administer`; Platform Access: `platform-identity.administer` | `CF_DEVELOPER_PLATFORM_BASE_URL`, `CF_PLATFORM_ACCESS_BASE_URL`; `PARTNERS`, `IDENTITY`, `APPS`, `VERIFICATION` |
| `activate-admin-bridge-communications` | `communications.administer` | `CF_COMMUNICATIONS_BASE_URL`; `COMMUNICATIONS` |
| `activate-admin-bridge-meetings` | `account.administer`, `meeting.administer`; also sets `ADMIN_OPERATOR_API_ENABLED=true` on the Meeting Service | `CF_MEETING_SERVICE_BASE_URL`; `ACCOUNTS`, `MEETINGS` |
| `activate-admin-bridge-messaging` | `messaging-firstparty.administer` | `CF_MESSAGING_BASE_URL`; `MESSAGING` |
| `activate-admin-bridge-calling` | `calling.administer` | `CF_CALLING_BASE_URL`; `CALLING` |

Every permission except `notifications-observability.read`, `developer-platform.administer` and `platform-identity.administer` is bridge-only in the registry. Even those three require the bridge's `hq:<staff>` attribution, or the console's named operator, on the routes the bridge uses.

## 3. Enabling for HQ (HQ stage S3), operator steps

**Scope (owner decision, HQ gate G1, 2026-11-20):** HQ staging may read **and modify** CF Meet production. This section switches on the **reads** (HQ stages S3/S5). Commands follow per family once A3+ serves them:
- `ADMIN_BRIDGE_WRITES_ENABLED=true`, through a write-enabled counterpart of `activate-admin-bridge-hq` for HQ stage S6 (not built yet);
- an access policy revision, signed offline, that adds the family's command permissions;
- HQ re-pins the contract and lists the family in `CFMEET_ADMIN_ENABLED_COMMANDS`.

Until then, safe defaults keep writes off.

Steps 1, 2 and 4 are operator steps; step 3 is the `activate-admin-bridge-hq` operation, which refuses to run until steps 1 and 2 are complete. Never hand-edit the container: safe defaults refuse `ADMIN_BRIDGE_ENABLED=true` until that operation is recorded.

Run each tool as `admin-bridge-admin` from the image (`/app/admin-bridge-admin`) on `cfmeet-core-private`, with the runtime `DATABASE_URL` in a 0600 env file, never on the command line.

1. **Enrol HQ.** Use the values HQ sends out of band; no secret passes through chat or tickets.
   - `enroll-cert --fingerprint <sha256 of HQ client cert DER> --not-before … --expires … --operator <you>`
   - `enroll-hmac --kid <X-HQ-Key-ID> --key-file /secure/<kid>.key --expires … --operator <you>`. Only the sha256 is stored. The key file itself is staged as `hmac/<kid>.key` (0400, owned by uid 10001; step 3), which the bridge reads as `ADMIN_BRIDGE_HMAC_KEY_DIR`. The file is **base64url text of at least 32 key bytes** (padding optional), the same file HQ reads as `CFMEET_BRIDGE_HMAC_KEY_FILE`; generate it once, for example `head -c 32 /dev/urandom | basenc --base64url -w0 | tr -d '='`, and deliver the same sealed file to both sides.
   - `enroll-staff-key --kid <jws kid> --public-key-hex … --issuer cosmicforge-hq --audience cfmeet-admin-bridge --expires … --operator <you>`
2. **Access policy.**
   - `policy-ceiling --revision 1 > policy.json`, then edit it down to a READ-ONLY policy: the reads only, no command permission.
   - `policy-sign --key-file /secure/policy.pem --document policy.json`, **offline**, on the policy signer's machine.
   - `policy-put --revision 1 --document policy.json --signature … --key-id … --public-key-hex … --operator <you>`. This runs the same verification as the bridge: signature, revision, and ceiling.
3. **Stage the files and run `activate-admin-bridge-hq`** (`confirm=hq-enrolled-read-only`; needs the owner's go-ahead and an agreed release window). Stage everything on the host under `/opt/cfmeet-core/admin-bridge/` first. Every file and both directories are owned by uid 10001 (the bridge). Keys are 0400; nothing is group- or world-writable; there are no symlinks and no other files.
   - `tls/server.crt`, `tls/server.key`, `tls/hq-client-ca.pem` (see the server certificate notes in step 4).
   - `hmac/<X-HQ-Key-ID>.key`: the same base64url file as in step 1.
   - `enable.json`, with exactly these keys (no secrets):
     ```json
     {"policy_revision": 1, "policy_key_id": "<key id>", "policy_public_key_hex": "<64 lowercase hex>",
      "hq_workload_uri": "cf://staging/product-admin/cfmeet", "enrolled_markets": ["GLOBAL"],
      "enrolled_tenants": ["<lowercase Platform Access tenant UUID>"],
      "listen_address": "10.250.0.1", "allowed_source": "10.250.0.2"}
     ```
   The operation refuses, changing nothing, unless all of these hold:
   - HQ has an active client certificate and an active staff-token key, each valid for at least another day.
   - Every staged HMAC file's decoded sha256 equals its active enrolment.
   - The pinned policy revision is stored, signed by `policy_key_id`, and no newer revision exists.
   - Every file is sealed as described above.

   It then:
   1. Recreates the bridge with `ADMIN_BRIDGE_ENABLED=true` and writes still off, the policy, workload, scope and file settings, and `tls/` and `hmac/` mounted read-only under `/run/admin-bridge/`.
   2. Checks that `/ready` reports `enabled: true` on the pinned revision.
   3. Installs the relay from step 4.

   Any failure removes the relay and restores the dark bridge.
4. **HQ connection: private tunnel only** (owner decision 2026-11-20). Bring the tunnel up **before** step 3. HQ connects only to a pinned private IP, so HQ staging reaches the bridge over a private tunnel (e.g. WireGuard) between the HQ staging host and the CF Meet host. Setting up the tunnel is a privileged operator step; the addresses move out of band, never via chat or git.
   - Bring up the tunnel. Note the CF Meet host's tunnel address (`ADMIN_BRIDGE_HQ_LISTEN_ADDRESS`) and HQ's tunnel address (`ADMIN_BRIDGE_HQ_ALLOWED_SOURCE`). Both must be **RFC 1918 only**. 100.64/10 is refused by HQ, which treats it as shared CGNAT space; the tool refuses it, and public addresses, too. Suggested: a dedicated /30 such as `10.250.0.0/30`, with CF Meet at `10.250.0.1` and HQ at `10.250.0.2`.
   - `activate-admin-bridge-hq` installs the passthrough relay `cfmeet-admin-bridge-hq-relay` (step 3). Do not install it by hand. It is an nginx `stream` relay with `allow <HQ tunnel address>; deny all;`, and TLS is **not** terminated.
     - It runs on its own network, `cfmeet-admin-bridge-hq-net`, joined to the private network, and is published **only** on `<CF Meet tunnel address>:8460`.
     - Traffic arriving on the tunnel interface keeps its source address, so `allow` sees HQ's address.
   - On the host firewall, allow TCP 8460 only on the tunnel interface and only from HQ's tunnel address. Keep 8460 closed on every public interface.
   - Never add 8460 to `api.cfmeet.com`, the public site, or the private TLS proxy.
   - **Server certificate:** issue it from CF Meet's private admin-bridge CA with DNS SAN `admin-bridge.cfmeet.internal` (HQ uses it as SNI), never a public name. Give HQ the CA bundle and the leaf SHA-256 fingerprint out of band. HQ pins both, so send the new fingerprint **before** each rotation.
   - **HQ's client certificate:** exactly one URI SAN, `cf://staging/product-admin/cfmeet`. Enrol the leaf's DER SHA-256 with `enroll-cert`.
   - **Enrolled scope:** `ADMIN_BRIDGE_ENROLLED_MARKETS=GLOBAL`; `ADMIN_BRIDGE_ENROLLED_TENANTS=<CF Meet first-party Platform Access tenant UUID>`. Tenant scopes are always Platform Access tenant **UUIDs in canonical lowercase** (8-4-4-4-12; normalise any upper-case value first), never slugs: owner reads are scoped by that UUID (Calling refuses anything else), and a partner's workspace id is its Platform Access tenant UUID.
5. **Verify.**
   - `/ready` returns `{"ready":true,"enabled":true,"policy_revision":1,"openapi_sha256":<pinned hash>}`.
   - HQ's `bridge.capabilities.read` returns the same `openapi_sha256` as HQ's pin.
   - `admin-bridge-admin verify-audit-chain` exits 0.

## 4. Rollback

- Run `rollback` right after `activate-admin-bridge-hq`. It removes the relay first, so HQ's way in closes, then restores the dark bridge. HQ then gets no answer on 8460, and any request that still reaches the bridge gets `503 BRIDGE_DISABLED`.
- Remove the firewall rule. The tunnel can stay up: with the relay gone, nothing listens on 8460.
- Grants added by `activate-admin-bridge` are undone by rolling that operation back.

## 5. Keys: rotation and revocation

- **HQ keys** (client certificate, HMAC key, staff-token key):
  1. Enrol the new key alongside the old one (different id).
  2. HQ switches.
  3. `revoke-principal --kind CLIENT_CERT|HMAC_KEY|STAFF_KEY --key-id <old> --operator <you>`. Revocation is one-way and takes effect on the next request.
- **Staff member or session:** `revoke-staff --sub <hq sub> [--session <id>] --reason <code> --operator <you>`.
- **Bridge service key:** `activate-service-key-publish` / `-switch` / `-retire` with `service=admin-bridge`.
- **Access policy:** put revision N+1 (signed offline), set `ADMIN_BRIDGE_POLICY_REVISION=N+1`, restart. Revisions are append-only.
- **Audit:** `verify-audit-chain` after any incident. A break is reported with the first bad sequence; the audit table cannot be updated, deleted or truncated.
