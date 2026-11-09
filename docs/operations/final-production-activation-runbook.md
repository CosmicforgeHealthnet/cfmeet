# CF Meet — production activation runbook (final closure, Document G)

Status: **prepared, not executed.** Nothing in this runbook has been run against production. Every step marked *owner* needs explicit authorization; every feature introduced by the closure programme ships **off**. Production is promoted only from a `staging` commit whose full CI matrix is green (see the final closure register).

**Automation:** [coordinated-production-release.md](coordinated-production-release.md) — the dispatch-only workflow that performs §0 P3, §1 and the §4 steps (each a separate operation) and is the single production path for every backend service.

Related, authoritative for their own scope (this runbook orders them, it does not replace them):

- [organizations-cutover-runbook.md](organizations-cutover-runbook.md) — the Organizations authority migration (legacy → shadow → authoritative) and its no-split-brain rollback rule.
- [developer-platform-gateway-activation-checklist.md](developer-platform-gateway-activation-checklist.md) — Developer Platform and API Gateway live activation.
- [backup-and-restore.md](backup-and-restore.md) — backups, the pre-migration snapshot (SEC-011) and the restore drill.
- [org-policy-calling-scope-section-12.md](org-policy-calling-scope-section-12.md) — per-organization Enterprise Calling.

## 0. Preconditions (all must hold; stop otherwise)

| # | Check | Evidence |
|---|---|---|
| P1 | The release commit is on `staging` and its full matrix is green (incl. Part VI certification, Unified Messaging, Enterprise calling, RTC Egress capture, backup drill). | CI run ids in the closure register |
| P2 | Off-host backup target and age key custody chosen by the owner; a restore drill against that target passed (the CI real-schema drill `real-schema-drill.sh` proves the tooling over every service's real migrated schema; the owner target is the remaining input). | backup-and-restore.md §Owner setup, §Restore drill |
| P3 | A fresh pre-migration snapshot of **every** service database exists and was verified (checksum + isolated restore of at least the Meeting Service and Organizations databases). | snapshot manifest |
| P4 | Production secrets present and never logged: service signing keys (one per identity), trust bundles, `JWT_SECRET` length confirmed, credential pepper, Messaging token / KT keys. | secret inventory (names only) |
| P5 | Owner approval recorded for each *owner* step below that will be executed in this release. | approval log |

## 1. Deployment order

Owners deploy before their callers; additive migrations before the images that need them; nothing is enabled until its consumers are live.

1. **Platform Access** (identity authority): migrate (`platform-access-migration up`), deploy.
2. **Organizations Service**: own database; `organizations-migration up` (adds `m20261110_000004_messaging_service_binding`); deploy with `ORGANIZATIONS_PROVISIONING_ENABLED=false` and **no** `ORGANIZATIONS_EVENT_DESTINATIONS` (events accumulate durably, nothing is sent).
3. **Messaging**, **Calling**, **Communications Hub**, **Telephony / Numbering / SMS Gateway / Privacy Relay**: migrate, deploy (unchanged behaviour).
4. **Meeting Service**: run every history in order — `migration`, `account-security-migration`, `meetings-calendar-migration`, `profile-notifications-migration`, `account-hardening-migration`, `account-communications-migration`, **`organization-integration-migration`** (new: event inbox, fences, member access, scope environments). Deploy with `ORGANIZATIONS_AUTHORITY_MODE=legacy`, `ORGANIZATION_EVENTS_ENABLED=false`, `ORGANIZATION_CALLING_SCOPE=shared`. The new history is additive; the previous image still starts after it is applied (rollback safety).
5. **Developer Platform + API Gateway**: per the activation checklist (separately authorized; does not re-point `api.cfmeet.com` without its own approval).
6. **Web and mobile clients**: the web build consumes the generated contract types already in the repo; organization features stay hidden until the workspace capabilities say otherwise (server-decided).

## 2. Trust-bundle grants (receiver-authoritative)

Add only what the step being activated needs; every grant is in the permission registry (platform common 1.16.0).

| Receiver | Issuer | Permission | Needed for |
|---|---|---|---|
| Meeting Service | `cfmeet.organizations` | `meeting-organization-events.apply` | organization event delivery (§4.2) |
| Organizations | `cfmeet.meeting-service` | `organization.read`, `organization.write`, `organization.act-for-user` | authoritative mode |
| Organizations | `cfmeet.operator-console` | `organization.administer`, `organization.migrate` | import, bindings, delivery replay |
| Platform Access | `cfmeet.organizations` | `platform-identity.provision-first-party` | provisioning saga |
| Platform Access | `cfmeet.meeting-service` | `platform-principal.ensure` | organization-scope principals and offboarding |
| Calling | `cfmeet.meeting-service` | `calling-request.act-for-organization` | per-organization Enterprise Calling |
| Messaging | `cfmeet.meeting-service` | `messaging-application.delegate` | organization-scope Messaging (already granted for personal Messaging) |

## 3. Feature flags (defaults keep today's behaviour)

| Service | Variable | Default | Turn on when |
|---|---|---|---|
| Organizations | `ORGANIZATIONS_PROVISIONING_ENABLED` | `false` | §4.1 |
| Organizations | `ORGANIZATIONS_EVENT_DESTINATIONS` (+ `CF_MEETING_SERVICE_BASE_URL`) | empty | §4.2 |
| Meeting | `ORGANIZATIONS_AUTHORITY_MODE` | `legacy` | cutover runbook (`shadow`, then `authoritative`) |
| Meeting | `ORGANIZATION_EVENTS_ENABLED` | `false` | §4.2 |
| Meeting | `ORGANIZATION_CALLING_SCOPE` (+ `CF_CALLING_BASE_URL`, optional `CF_MESSAGING_BASE_URL`) | `shared` | §4.3 / §4.4 (requires authoritative) |
| Calling | `CALLING_ORGANIZATION_FORWARDING_ENABLED` | `false` | §4.3 |
| Messaging | PRIVATE_E2EE device operations in production | refused | never without independent cryptographic approval (J-21) |
| Notifications | `NOTIFICATIONS_EMAIL_ENABLED` | `false` | owner (mail provider) |

## 4. Activation steps (each *owner*, in this order)

### 4.1 Provisioning
Grant `platform-identity.provision-first-party`; set `ORGANIZATIONS_PROVISIONING_ENABLED=true`; watch `GET /internal/v1/organizations/{id}/provisioning` reach READY for a test organization; a failure backs off and an operator retries (`POST /internal/v1/admin/organizations/{id}/provisioning/retry`).

### 4.2 Organization event delivery
Grant `meeting-organization-events.apply` in the Meeting Service bundle; set `ORGANIZATION_EVENTS_ENABLED=true` on Meeting (its startup refuses without `organization-integration-migration`); then set `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service` on Organizations. The accumulated backlog is delivered in aggregate order. Watch `GET /internal/v1/admin/event-delivery` (pending → 0, `dead = 0`); replay dead letters only after the cause is fixed (`POST /internal/v1/admin/event-delivery/replay`).

### 4.3 Organizations authority cutover
Follow organizations-cutover-runbook.md exactly (export → import → reconciliation → shadow with the zero-mismatch criterion → quiesce → final import → authoritative). Rehearsed across real processes in CI (`organizations_cutover_shadow_and_rollback_rehearsal`).

### 4.4 Per-organization Enterprise Calling and Messaging
For each organization that opts in: an operator creates a service account and credential **inside that organization's own FIRST_PARTY triple** in Platform Access, then records them: `PUT /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling` and `/messaging`. Bootstrap the organization environment's Messaging policy to allow MANAGED (delegated `PUT /v1/messaging/policy`). Then set `ORGANIZATION_CALLING_SCOPE=organization` (+ `CF_MESSAGING_BASE_URL`) on Meeting and `CALLING_ORGANIZATION_FORWARDING_ENABLED=true` on Calling.

## 5. Health verification

- Every service `/ready` (or `/health`) is 200; the Meeting startup log shows `Organizations authority mode: …` as intended.
- Organizations: `GET /internal/v1/admin/event-delivery` pending bounded, `dead = 0`; provisioning READY for test organizations.
- Meeting: `organization_member_access` backlog bounded (the worker logs `organization access applied`; failures retry with backoff, never dropped).
- Messaging realtime revalidation interval as configured (default 30 s; this is the documented upper bound for closing a removed member's organization connection).

## 6. Live smoke tests (consented test identities, non-destructive)

1. Sign in, read `/api/v2/me/profile`; phone verification reports `PROVIDER_BLOCKED`, never verified.
2. Create a test organization; provisioning READY; invite and accept a second test account; workspaces list it.
3. Organization Messaging: create a group, send, read; remove the test member; the relay refuses them and, within the revalidation bound, Messaging refuses their organization principal; their personal messaging still works.
4. Suspend and reactivate the test organization; organization operations fail closed, then return.
5. Developer Platform (if activated): the journey in the activation checklist §8.

## 7. Rollback

- **Images**: redeploy the previous image; every closure migration is additive, the previous images start on the migrated schemas.
- **Flags**: turning a flag off restores the previous behaviour (`ORGANIZATION_EVENTS_ENABLED=false` stops ingestion; Organizations keeps the backlog PENDING durably).
- **Organizations authority**: the cutover runbook's §7 rules only (never back to `legacy` after Organizations accepted a write).
- **Data**: restore from the pre-migration snapshot into a fresh database and verify (backup-and-restore.md §Restore drill) — never over a live database.

## 8. DNS and TLS

- `api.cfmeet.com` keeps its current routing; this release does **not** re-point it (the API Gateway takeover is its own approval in the activation checklist).
- Certificate automation covers the `api.cfmeet.com` lineage (`tls-certificate-automation.md`, `cfmeet-certificate-monitor.yml`); any new public host (for example a hosted staging API, `staging-api.cfmeet.com`) needs its own certificate lineage and monitor entry before traffic.
- Internal service-to-service traffic stays on the private network with signed service tokens; the internal-TLS decision for the Gateway / Developer Platform path (PLT-005) is an owner input.
- Web CSP: `connect-src` must include the object-storage origin for artifact / voicemail playback and the Messaging realtime origin (`CSP_STORAGE_URL`, `CSP_EXTRA_CONNECT_SRC`).

## 9. Staging activation (before any production step)

1. Deploy the release commit to the staging hosts in the order of §1 with every flag at its default.
2. Run the staging smoke tests: §6 steps 1–4 with staging test identities, plus: an `ORGANIZATION_ONLY` meeting resolves for a member and is `404` for an unrelated account and `401` signed out; a guest joins an invite-only meeting with consent, waits and is admitted; a personal voice call rings, connects, holds, ends and appears in both histories.
3. Enable §4.1 → §4.4 on staging first, one step at a time, and repeat the smoke tests after each.
4. Record the staging run ids next to the release commit; only then request the production approvals listed in §0 P5.

## 10. Behaviour changes in the final closure (no flag; effective on deploy)

| Change | Who notices | Evidence |
|---|---|---|
| An `ORGANIZATION_ONLY` meeting space is no longer disclosed by identifier lookup to outsiders (`404 MEETING_NOT_FOUND`) or signed-out callers (`401 AUTHENTICATION_REQUIRED`); members, invitees and token holders are unaffected | clients opening organization meeting links while signed out (offer sign-in) | `organization_domain_access::org04_*`, certification J-07 |
| Legacy (v1) guest refresh tokens are single use, last at most 24 h from the join, and stop after removal, leaving or the meeting ending | legacy guest links only | `legacy_v1_web_compat::sec004_*` |
| Organization OWNER / ADMIN (`meetings.organization.manage`) may configure, reassign and archive the organization's meeting spaces and see their FULL view | only in `ORGANIZATIONS_AUTHORITY_MODE=authoritative` | `organizations_authority::org011_*` |
