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
  - the HQ mTLS listener is not started while disabled;
  - no owner events listener (`ADMIN_BRIDGE_EVENTS_BIND_ADDRESS` unset).
- Private TLS route `api.cfmeet.com:8457` (installed by `provision`, like every new service's route). It forwards **only** to the owner events listener (`cfmeet-admin-bridge:8191`), never to the ops or HQ listener, and answers 502 until §2.2 starts that listener.
- Readiness: `GET http://127.0.0.1:8190/ready` (in the container's network namespace) returns `{"ready":true,"enabled":false,"database":"OK",…}`.
- Live certification checks that the ops listener does **not** serve `/internal/cfmeet-admin/v1/*` (404).
- Safe defaults refuse `ADMIN_BRIDGE_ENABLED=true`, `ADMIN_BRIDGE_WRITES_ENABLED=true`, any module switch whose activation is not recorded, and an events listener or owner `CF_ADMIN_BRIDGE_BASE_URL` before §2.2. A hand-edited container therefore fails the next `release` / `verify`.

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

**Daily datasets (A7).** The bridge pulls `GET /internal/v1/operator/datasets/{datasetId}/days/{day}` (attributed `hq:dataset-sync`) with the grants above; nothing more is granted:

| Datasets | Owner | Permission (activation) |
|---|---|---|
| `cfmeet.engagement_daily`, `cfmeet.meetings_daily` | Meeting Service | `account.administer`, `meeting.administer` (`activate-admin-bridge-meetings`) |
| `cfmeet.messaging_daily`, `cfmeet.operations_health` | Messaging | `messaging-firstparty.administer` (`activate-admin-bridge-messaging`) |
| `cfmeet.operations_health` (failed deliveries) | Notifications | `notifications.administer` (`activate-admin-bridge-notifications`) |
| `cfmeet.partners_daily`, `cfmeet.apps_daily` | Developer Platform | `developer-platform.administer` (`activate-admin-bridge-partners`) |

A source whose owner activation is not recorded never reports; the read's coverage stays PARTIAL.

### 2.2 Owner connections from the A4–A7 work

Each operation below is off by default and refused by safe defaults until it is recorded. Each restores every container it touched on failure, and `rollback` undoes it like any activation. The trust bundles are merged: no issuer, key or grant is dropped.

| Operation | Requires | Grants | Settings |
|---|---|---|---|
| `activate-admin-bridge-events` | `activate-admin-bridge-hq` | **bridge** bundle: `hq-bridge-events.publish` for the Developer Platform, Messaging, Calling, Communications | bridge: `ADMIN_BRIDGE_EVENTS_BIND_ADDRESS=0.0.0.0:8191`; Developer Platform API and worker, Messaging, Calling, Communications: `CF_ADMIN_BRIDGE_BASE_URL=https://api.cfmeet.com:8457` |
| `activate-plan-enforcement` (`confirm=plan-limits-reviewed`) | `activate-developer-platform` | Developer Platform bundle: `developer-entitlement.read` for the Meeting Service and Messaging | Meeting Service: `MEETING_PLAN_ENFORCEMENT=on`, `MEETING_PLAN_CACHE_SECONDS=60`; Messaging: `MESSAGING_PLAN_ENFORCEMENT=on`, `MESSAGING_PLAN_CACHE_SECONDS=60`; both: `CF_DEVELOPER_PLATFORM_BASE_URL` only where it is not set yet |
| `activate-developer-owner-invitations` | `activate-developer-publications` | none (I1 is granted by the publications activation) | Developer Platform API: `CF_NOTIFICATIONS_SEALING_KEY` |
| `activate-organization-export-delivery` | `activate-organizations-publications` | none (I1 as above) | Organizations: `ORGANIZATIONS_EXPORT_DELIVERY_ENABLED=true`, `CF_NOTIFICATIONS_SEALING_KEY`, `ORGANIZATIONS_EXPORT_KEYS_FILE=/run/secrets/organizations-export.keys` (mounted read-only) |

**Owner events (`activate-admin-bridge-events`).** One operation, not one per owner: there is one listener, one receiver bundle (the bridge's) and one owner decision, and the publishers' outboxes simply wait (up to 7 days) for a listener. Each publisher's switch is "on when `CF_ADMIN_BRIDGE_BASE_URL` is set": no companion flag. The operation refuses, changing nothing, unless:
- the bridge is enabled for HQ (the listener runs only while `ADMIN_BRIDGE_ENABLED=true`) and has its trust bundle;
- the route 8457 is installed;
- each publisher's outbox exists (`devplat_bridge_events`, `messaging_admin_bridge_events`, `calling_admin_bridge_events`, `communication_admin_bridge_events`), i.e. the released images carry the A7 publishers;
- every publishing container has its production signing identity.

It recreates the bridge first (new bundle and listener), then the publishers, then proves that an unsigned `POST /internal/v1/hq-bridge/events` through the route answers 401. The Meeting Service, Organizations, Platform Access and Notifications may be granted the permission by the registry but publish nothing yet. `rollback` always undoes the latest operation only: roll this one back before anything it depends on.

**Plan enforcement (`activate-plan-enforcement`).** From this point on the active CF Meet plan limits refuse real joins (`participants`), recordings (`recording`), conference extensions (`meetingMinutes`) and attachment uploads (`attachmentBytes`). Review the active plan set and overrides first (HQ pricing, `pricing.administer`), then confirm. Both services fail open: a failed, slow or unexpected plan read keeps today's limits. The Meeting Service's earlier name for its switch, `MEETING_PLAN_ENFORCEMENT_ENABLED`, is refused as `true` too and never set.

**Owner-delivered links.** A secret a bridge command produces goes to its rightful recipient, never to HQ:
- The Developer Platform's first-party owner invitation and the Organizations export link are sealed to the Notification Service's HPKE key. It is the same pair `activate-meeting-publications` / `activate-partner-push` use: generated once, never replaced. Each operation also mounts the private key on any replica that lacks it. The producer gets only the public key. The notices are relayed through I1, so they need the producer's publications activation, and they are delivered only once the Notification Service routing row of their category is ACTIVE.
- `activate-organization-export-delivery` generates `/opt/cfmeet-core/secrets/organizations-export.keys` once: one `exp-YYYY-MM:<base64url 32 bytes>` line, owned by uid 10001, mode 0400. It is never regenerated, because exports in flight must stay readable. To rotate, add a new first line and keep the old one for 7 days. Organizations refuses to start while its build lacks the Notification Service catalogue type `organizations.organization.export_ready`. Until then the activation fails its health check and restores the previous container.
- The partner API keys from Platform Access, the Messaging exports and the Developer Console reveal (`CONSOLE_REVEAL_ONCE`) need no setting. Neither does the Meeting Service account export: it uses its own email, sealed under its existing secret, and is served once `activate-admin-bridge-meetings` set `ADMIN_OPERATOR_API_ENABLED`.

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
4. **HQ connection, separate hosts: private tunnel** (owner decision 2026-11-20). Bring the tunnel up **before** step 3. HQ connects only to a pinned private IP, so HQ staging reaches the bridge over a private tunnel (e.g. WireGuard) between the HQ staging host and the CF Meet host. Setting up the tunnel is a privileged operator step; the addresses move out of band, never via chat or git.
   - Bring up the tunnel. Note the CF Meet host's tunnel address (`ADMIN_BRIDGE_HQ_LISTEN_ADDRESS`) and HQ's tunnel address (`ADMIN_BRIDGE_HQ_ALLOWED_SOURCE`). Both must be **RFC 1918 only**. 100.64/10 is refused by HQ, which treats it as shared CGNAT space; the tool refuses it, and public addresses, too. Suggested: a dedicated /30 such as `10.250.0.0/30`, with CF Meet at `10.250.0.1` and HQ at `10.250.0.2`.
   - `activate-admin-bridge-hq` installs the passthrough relay `cfmeet-admin-bridge-hq-relay` (step 3). Do not install it by hand. It is an nginx `stream` relay with `allow <HQ tunnel address>; deny all;`, and TLS is **not** terminated.
     - It runs on its own network, `cfmeet-admin-bridge-hq-net`, joined to the private network, and is published **only** on `<CF Meet tunnel address>:8460`.
     - Traffic arriving on the tunnel interface keeps its source address, so `allow` sees HQ's address.
   - On the host firewall, allow TCP 8460 only on the tunnel interface and only from HQ's tunnel address. Keep 8460 closed on every public interface.
   - Never add 8460 to `api.cfmeet.com`, the public site, or the private TLS proxy.
   - **Server certificate:** issue it from CF Meet's private admin-bridge CA with DNS SAN `admin-bridge.cfmeet.internal` (HQ uses it as SNI), never a public name. Give HQ the CA bundle and the leaf SHA-256 fingerprint out of band. HQ pins both, so send the new fingerprint **before** each rotation.
   - **HQ's client certificate:** exactly one URI SAN, `cf://staging/product-admin/cfmeet`. Enrol the leaf's DER SHA-256 with `enroll-cert`.
   - **Enrolled scope:** `ADMIN_BRIDGE_ENROLLED_MARKETS=GLOBAL`; `ADMIN_BRIDGE_ENROLLED_TENANTS=<CF Meet first-party Platform Access tenant UUID>`. Tenant scopes are always Platform Access tenant **UUIDs in canonical lowercase** (8-4-4-4-12; normalise any upper-case value first), never slugs: owner reads are scoped by that UUID (Calling refuses anything else), and a partner's workspace id is its Platform Access tenant UUID.
5. **HQ connection, same host (HQ and CF Meet on one VPS; agreed with HQ 2026-11-26).** No tunnel and no host port.
   - HQ creates and owns `cf-staging-cfmeet-bridge` (`pa-staging-ops.sh prepare-host`): internal, subnet `10.231.14.0/28`. `.1` is the Docker gateway, `.3` HQ's connector, `.4` HQ's worker.
   - Put `"transport": "same-host"` in `enable.json`, with no `listen_address` or `allowed_source`.
   - `activate-admin-bridge-hq` refuses unless that network exists, is internal and has exactly that subnet. It then joins the relay at `10.231.14.2`, admitting only `.3` and `.4`, publishing nothing, still passing TLS through.
   - The relay joins no other HQ network, and CF Meet never joins HQ's database network. HQ's checks fail if any other container joins `cf-staging-cfmeet-bridge`.
   - Leave 8460 closed on every host interface.
5. **Verify.**
   - `/ready` returns `{"ready":true,"enabled":true,"policy_revision":1,"openapi_sha256":<pinned hash>}`.
   - HQ's `bridge.capabilities.read` returns the same `openapi_sha256` as HQ's pin.
   - `admin-bridge-admin verify-audit-chain` exits 0.

## 3.1 Commands for HQ (HQ stage S6)

Do this only with the owner's go-ahead and an agreed release window; owner gate G1 already allows HQ to modify. Each command family additionally needs its module switch (§2.1) and its permissions in the policy.

1. Create the new access policy. Run `policy-ceiling --revision N+1 > policy.json`, then cut it down to the reads plus the command permissions of the families being switched on.
2. Sign it and store it: `policy-sign` (**offline**), then `policy-put --revision N+1 …`. The bridge refuses any policy outside the ceiling.
3. Set `policy_revision` to N+1 in `/opt/cfmeet-core/admin-bridge/enable.json`.
4. Run `activate-admin-bridge-hq-commands` (`confirm=hq-commands-two-person`). It refuses, changing nothing, unless all of these hold:
   - the bridge is enabled for HQ;
   - the operations ledger exists;
   - N+1 is newer than the running revision, stored, signed by the running key and the newest;
   - HQ is still fully enrolled.
   It then sets `ADMIN_BRIDGE_WRITES_ENABLED=true` and `ADMIN_BRIDGE_POLICY_REVISION=N+1`, and checks `/ready`.
5. HQ re-pins and lists the families in `CFMEET_ADMIN_ENABLED_COMMANDS`.

Rollback: run `rollback` on that operation. It restores writes off and the previous revision; revisions are append-only.

## 4. Rollback

- Run `rollback` right after `activate-admin-bridge-hq`. It removes the relay first, so HQ's way in closes, then restores the dark bridge. HQ then gets no answer on 8460, and any request that still reaches the bridge gets `503 BRIDGE_DISABLED`.
- Remove the firewall rule. The tunnel can stay up: with the relay gone, nothing listens on 8460.
- Grants added by `activate-admin-bridge` are undone by rolling that operation back.
- `rollback` right after a §2.2 operation restores every container it recreated. For `activate-admin-bridge-events` that means the bridge without its listener or the publishers' grants, and the publishers without `CF_ADMIN_BRIDGE_BASE_URL`. Events already in the publishers' outboxes stay there and expire after 7 days.

## 5. Keys: rotation and revocation

- **HQ keys** (client certificate, HMAC key, staff-token key):
  1. Enrol the new key alongside the old one (different id).
  2. HQ switches.
  3. `revoke-principal --kind CLIENT_CERT|HMAC_KEY|STAFF_KEY --key-id <old> --operator <you>`. Revocation is one-way and takes effect on the next request.
- **Staff member or session:** `revoke-staff --sub <hq sub> [--session <id>] --reason <code> --operator <you>`.
- **Bridge service key:** `activate-service-key-publish` / `-switch` / `-retire` with `service=admin-bridge`.
- **Access policy:** put revision N+1 (signed offline), set `ADMIN_BRIDGE_POLICY_REVISION=N+1`, restart. Revisions are append-only.
- **Audit:** `verify-audit-chain` after any incident. A break is reported with the first bad sequence; the audit table cannot be updated, deleted or truncated.
