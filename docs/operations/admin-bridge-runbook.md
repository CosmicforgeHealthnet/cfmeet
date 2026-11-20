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

The bridge **stays disabled**. Later phases add one `ADMIN_BRIDGE_GRANTS` row per owner and permission, the matching owner base URL, and the module switch.

## 3. Enabling for HQ (HQ stage S3), operator steps

Enabling is **not automated yet**. It needs HQ's enrolment, the TLS material and the policy key mounted into the container. Until a dedicated `enable-admin-bridge` operation exists, do not hand-edit the container: safe defaults refuse it.

Run each tool as `admin-bridge-admin` from the image (`/app/admin-bridge-admin`) on `cfmeet-core-private`, with the runtime `DATABASE_URL` in a 0600 env file, never on the command line.

1. **Enrol HQ.** Use the values HQ sends out of band; no secret passes through chat or tickets.
   - `enroll-cert --fingerprint <sha256 of HQ client cert DER> --not-before … --expires … --operator <you>`
   - `enroll-hmac --kid <X-HQ-Key-ID> --key-file /secure/<kid>.key --expires … --operator <you>`. Only the sha256 is stored. The key file itself goes in `ADMIN_BRIDGE_HMAC_KEY_DIR` as `<kid>.key` (0400, owned by uid 10001).
   - `enroll-staff-key --kid <jws kid> --public-key-hex … --issuer cosmicforge-hq --audience cfmeet-admin-bridge --expires … --operator <you>`
2. **Access policy.**
   - `policy-ceiling --revision 1 > policy.json`, then edit it down to a READ-ONLY policy: the reads only, no command permission.
   - `policy-sign --key-file /secure/policy.pem --document policy.json`, **offline**, on the policy signer's machine.
   - `policy-put --revision 1 --document policy.json --signature … --key-id … --public-key-hex … --operator <you>`. This runs the same verification as the bridge: signature, revision, and ceiling.
3. **Container settings** (the future `enable-admin-bridge` operation):
   - `ADMIN_BRIDGE_ENABLED=true`
   - `ADMIN_BRIDGE_POLICY_REVISION=1`, `ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX`, `ADMIN_BRIDGE_POLICY_KEY_ID`
   - `ADMIN_BRIDGE_HQ_WORKLOAD_URI` (HQ's single URI SAN, e.g. `cf://staging/product-admin/cfmeet`)
   - `ADMIN_BRIDGE_ENROLLED_MARKETS`, `ADMIN_BRIDGE_ENROLLED_TENANTS`
   - mounts for `ADMIN_BRIDGE_TLS_CERT_FILE`, `ADMIN_BRIDGE_TLS_KEY_FILE`, `ADMIN_BRIDGE_CLIENT_CA_FILE` and `ADMIN_BRIDGE_HMAC_KEY_DIR`
4. **HQ connection: private tunnel only** (owner decision 2026-11-20). HQ connects only to a pinned private IP, so HQ staging reaches the bridge over a private tunnel (e.g. WireGuard) between the HQ staging host and the CF Meet host. Setting up the tunnel is a privileged operator step; the addresses move out of band, never via chat or git.
   - Bring up the tunnel. Note the CF Meet host's tunnel address (`ADMIN_BRIDGE_HQ_LISTEN_ADDRESS`) and HQ's tunnel address (`ADMIN_BRIDGE_HQ_ALLOWED_SOURCE`). Both must be **RFC 1918 only**. 100.64/10 is refused by HQ, which treats it as shared CGNAT space; the tool refuses it, and public addresses, too. Suggested: a dedicated /30 such as `10.250.0.0/30`, with CF Meet at `10.250.0.1` and HQ at `10.250.0.2`.
   - Install the passthrough relay `cfmeet-admin-bridge-hq-relay` from `admin_bridge_hq_relay_config(<listen>, <source>)`. It is an nginx `stream` relay with `listen <tunnel address>:8460; allow <HQ tunnel address>; deny all;`, and TLS is **not** terminated.
   - On the host firewall, allow TCP 8460 only on the tunnel interface and only from HQ's tunnel address. Keep 8460 closed on every public interface.
   - Never add 8460 to `api.cfmeet.com`, the public site, or the private TLS proxy.
   - **Server certificate:** issue it from CF Meet's private admin-bridge CA with DNS SAN `admin-bridge.cfmeet.internal` (HQ uses it as SNI), never a public name. Give HQ the CA bundle and the leaf SHA-256 fingerprint out of band. HQ pins both, so send the new fingerprint **before** each rotation.
   - **HQ's client certificate:** exactly one URI SAN, `cf://staging/product-admin/cfmeet`. Enrol the leaf's DER SHA-256 with `enroll-cert`.
   - **Enrolled scope:** `ADMIN_BRIDGE_ENROLLED_MARKETS=GLOBAL`; `ADMIN_BRIDGE_ENROLLED_TENANTS=<CF Meet first-party tenant id>`.
5. **Verify.**
   - `/ready` returns `{"ready":true,"enabled":true,"policy_revision":1,"openapi_sha256":<pinned hash>}`.
   - HQ's `bridge.capabilities.read` returns the same `openapi_sha256` as HQ's pin.
   - `admin-bridge-admin verify-audit-chain` exits 0.

## 4. Rollback

- Set `ADMIN_BRIDGE_ENABLED=false`, or run `rollback` on the operation. Every HQ request then answers `503 BRIDGE_DISABLED`.
- Remove the relay and the firewall rule. The tunnel can stay up: with the relay gone, nothing listens on 8460.
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
