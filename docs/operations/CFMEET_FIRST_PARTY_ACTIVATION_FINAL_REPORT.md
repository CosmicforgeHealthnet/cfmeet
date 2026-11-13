# CF Meet — first-party production activation: final handover and closure report

Date: 2026-11-13 · Environment: production (`https://api.cfmeet.com`, Contabo VPS) · Owner directive: "Final production activation: remove backup blockers and activate all first-party features".

## 0. Verdict

**`CFMEET_FIRST_PARTY_BACKEND_ACTIVATED — PRODUCTION_CERTIFIED_WITH_ONE_BLOCKED_JOURNEY`**

The target verdict `CFMEET_FIRST_PARTY_BACKEND_FULLY_ACTIVATED — PRODUCTION_CERTIFIED` is **not** claimed. Every first-party acceptance suite that was executed passed against production. One required journey did not pass, because it cannot run in production:

* **§8.5 "Exchange personal messages".** Personal conversations are PRIVATE_E2EE by design. In production, E2EE device enrollment stays blocked until an independent cryptographic assessment is complete. MANAGED (server-readable) messaging is deliberately never substituted for personal E2EE. `messaging-verify` [43491691538](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43491691538) proves the guard works (15/15) and reports `BLOCKED_EXTERNAL_DEPENDENCY`. This is a security gate, not a defect. It was not bypassed (§11).

Everything else in scope is activated and verified: organization Messaging (send, read, unread counts, realtime), personal and organization Calling with real media, meetings, Organizations, Developer Platform and Gateway.

## 1. Release identity

| Item | Value |
|---|---|
| Deployed release (all 13 service images) | `b9dc15be072107b79bac8c49ca6b4b9ed65d12ca`: coordinated release [43456950173](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43456950173) |
| Commits this session (no image build input changed, so no redeploy was needed; `activation_base` verified) | `6182609b` remove the backup gate and make telecom CI advisory · `73919312` empty-import receipts fix · `58e22ec5` organization acceptance realtime frame fix · `8213eed6` Messaging acceptance gateway-family fix · this report |
| Final `main` head at certification | `58e22ec5b9183cc4054818cb369edf570b18a3b7` (certify reference), then `8213eed6` and this report (tooling and docs only) |

## 2. GitHub Actions runs (this session)

| Run | Operation | Result |
|---|---|---|
| [43471556540](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43471556540) | `activate-organization-bindings` (baseline check, not repeated) | success: `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED=true`, `{automaticBindings: 0, failed: 0}` |
| [43473267251](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43473267251) | `inspect` | success: journal and flags read (§4) |
| [43473874426](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43473874426) | `org-cutover-first-activation` attempt 1 | **refused safely before the switch**: the pre-switch inventory counted the empty import's own receipts as data (§5). Fixed in `73919312`. |
| [43479482331](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43479482331) | `org-cutover-first-activation` (`confirm=empty-dataset`) | **success: Organizations authoritative** |
| [43482739979](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43482739979) | `activate-enterprise-calling` | success |
| [43483450875](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43483450875) | `organization-acceptance` attempt 1 | 9 PASS, then FAIL at realtime: an acceptance-client bug (it read the wrong frame shape). Fixed in `58e22ec5`. |
| [43484531071](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43484531071) | `organization-acceptance` | **18/18 PASS** |
| [43488534526](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43488534526) | `developer-acceptance` | **30/30 PASS** |
| [43490164383](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43490164383) | `certify` | **CERTIFIED_READ_ONLY**, 13/13 services PASS, failures: none |
| [43479509962](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43479509962) | meetings acceptance | **22/22 PASS** (real LiveKit room) |
| [43479566836](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43479566836) | Calling `test-enterprise` | **21/21 PASS** (real media) |
| [43480807961](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43480807961) | Calling `test-recording` | **12/12 PASS** |
| [43481855456](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43481855456) | Calling `test-voicemail` | **7/7 PASS** |
| [43489312016](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43489312016) | `communications-verify` (personal journey) | **20/20 PASS** (real bidirectional voice and video) |
| [43491058144](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43491058144) | `messaging-verify` attempt 1 | FAIL on a stale expectation (`/v1/messaging/policy` is now a gateway family and refuses anonymous callers with 401). Fixed in `8213eed6`. |
| [43491691538](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43491691538) | `messaging-verify` | **15/15 PASS**, verdict `BLOCKED_EXTERNAL_DEPENDENCY` (personal E2EE) |

## 3. Owner decision: activation without verified backup protection

On 2026-11-13 the owner removed the encrypted off-site backup as a blocking prerequisite for the first-time Organizations activation, and accepted the increased recovery risk ([ADR 0059 Amendment 1](../decisions/0059-organizations-empty-dataset-first-activation.md)).

* On the `org-cutover-first-activation` path:
  * no `require_offsite(...)`;
  * no fresh local backup or restore-drill check;
  * no `offsite-backup-verified` confirmation;
  * no `CFMEET_BACKUP_AGE_RECIPIENTS` or `CFMEET_BACKUP_AGE_IDENTITY`;
  * the off-site workflow is not run.
* The empty-dataset import runs with `org_import(..., backup_gate=False)`.
* No replacement backup gate was introduced. No backup record was fabricated.
* The run records `backupGate: {required: false, offsiteBackupVerified: false}` and `firstActivation.backupVerified: false`.
* **No verified off-site backup exists.** If the production database is lost, organization data created after activation cannot be restored from an off-host copy.
* The standard (non-empty) cutover keeps gate 1. The off-site tooling (`cfmeet-offsite-backup.yml`) remains available for optional use.
* Retained and mechanically enforced:
  * empty-everywhere inventories (counts only);
  * refusal of any non-empty dataset, with nothing deleted to make it empty;
  * export, import, reconciliation and idempotence;
  * trust grants and single-writer authority;
  * the irreversible journal flag;
  * the post-switch quiescence check.

## 4. Backend services and runtime state

All 13 services run release `b9dc15be` and pass certify: running, no restarts, image revision equal to the registry tag, readiness, database connectivity, every migration applied, anonymous requests refused, service-auth identity and grants, and TLS reachability.

* Supporting containers running: `lk-server` (LiveKit), `cfmeet-calling-egress`, `cfmeet-calling-artifacts`, `cfmeet-calling-redis`, `cfmeet-core-db`, `cfmeet-core-tls`, `cfmeet-gateway-ingress`, `cfmeet-storage-ingress` and `cfmeet-messaging-ingress`.
* Resources at inspect: 5517 MiB available.

| Service | State | Key runtime flags (production) |
|---|---|---|
| meeting-service | running | `ORGANIZATIONS_AUTHORITY_MODE=authoritative`, `ORGANIZATION_EVENTS_ENABLED=true`, `ORGANIZATION_CALLING_SCOPE=organization`, conference realtime / webinar / quality telemetry on, `NOTIFICATIONS_EMAIL_ENABLED=true` (SMTP), `DEVELOPER_SESSION_BRIDGE_ENABLED=true`, usage export on |
| organizations | running | provisioning on, event destination `cfmeet.meeting-service`, `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED=true` |
| platform-access | running | grants: provision-first-party, bind-organization-services, principal.ensure, credential.authenticate, identity read/delegate |
| messaging | running | `MESSAGING_SESSION_DEVICES_ENABLED=true` (MANAGED realtime), `MESSAGING_E2EE_DEVICES_ENABLED=false`, push `none` |
| calling | running | app calling on, LiveKit media, Enterprise Calling, recording and voicemail on, `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`; public inbound/outbound PSTN off |
| communications | running | Hub; launch credential valid for 357 days |
| developer-platform / -worker | running | gateway API on, webhooks `active`, production-access workflow off (needs named reviewers) |
| api-gateway | running | public routing on, central webhooks on, disabled domains `numbering,sms,privacy-relay`, WebSockets off |
| telephony / numbering / sms-gateway / privacy-relay | running, **features disabled** | see §12 |

## 5. Organizations authority mode: evidence ([43479482331](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43479482331))

1. **Inventory before any change** (counts only, every database):
   * legacy organizations, memberships, orphans, ownerless and duplicates: all 0;
   * Meeting organization references: none, across 5 columns;
   * Organizations Service: empty, across 16 tables;
   * other services: no `organization_id` / `organization_ref`.
2. `backupGate` recorded as not required and not verified.
3. **Export** of the empty set: checksum `e3b0c442…b855`, counts 0/0.
4. **Import:** 0 conflicts, 0 imported. **Reconciliation:** 0 mismatches. A **repeated import** changed nothing.
5. **Inventory again**, immediately before the switch: empty. The 2+2 empty-import receipts are reported separately as `emptyImportReceipts`.
6. **Switch:** the Meeting Service received the shadow-step trust grants and moved to `ORGANIZATIONS_AUTHORITY_MODE=authoritative` with `CF_ORGANIZATIONS_BASE_URL`. The step is journalled as irreversible.
7. **Post-switch checks:**
   * the startup log contains `Organizations authority mode: authoritative`;
   * quiescence: legacy memberships 0, local organizations 0, stray legacy organizations 0.
8. **Public APIs no longer return `FEATURE_DISABLED`.** Organization creation without a session returns 401 `AUTHENTICATION_REQUIRED`. Authenticated creation returned 201 in acceptance.
9. **Single writer:** legacy write routes stay unused because the Meeting Service is authoritative and relays to Organizations. Rollback refuses the irreversible step.

**First attempt ([43473874426](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43473874426)).**

* What happened: the pre-switch inventory saw `organization_import_runs: 2` and `organization_audit_events: 2` and refused, so nothing was switched. These rows are the receipts of the procedure's own empty import.
* The fix in `73919312`:
  * subtracts exactly the receipts of an import with zero organizations, memberships, imported, unchanged and conflicts (SQL checked on PostgreSQL 16);
  * still counts every other row;
  * lets an `org-cutover-import` recorded for an empty dataset be retried.

## 6. Automatic organization provisioning and service bindings

`activate-organization-bindings` (ADR 0058 A2) is active.

In organization acceptance, every organization created by a user reached READY with no operator action (`automatic_provisioning_ready_without_operator`: PASS). That covers:

* its FIRST_PARTY tenant, application and environment;
* the Platform Access mapping;
* the Messaging and Calling service accounts, credentials and fixed scopes, created inside the organization's own triple and verified before READY.

Proven by the same run:

* isolation between two organizations;
* suspension and reactivation (relays fail closed, then recover);
* archival, which revokes the bindings.

Service credentials never leave the backend: the Organizations Service stores ids only, and the secret is discarded at issuance.

## 7. Acceptance results

### 7.1 Organization journey: 18/18 ([43484531071](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43484531071))

* Setup: three fresh, email-verified accounts and two organizations; 92 real public-API requests.

| Check | Result |
|---|---|
| accounts_login_and_personal_baseline | PASS |
| organizations_created_by_their_owners | PASS |
| retry_create_is_idempotent | PASS |
| automatic_provisioning_ready_without_operator | PASS |
| owner_membership_workspace_and_capabilities | PASS |
| invitation_accepted_only_by_the_invitee (wrong account → 403) | PASS |
| teams_and_member_workspace (outsider → 404) | PASS |
| organization_messaging_conversation_members_and_messages | PASS |
| organization_messaging_unread_and_read_cursor | PASS |
| organization_realtime_session_and_events (WebSocket; personal session refused) | PASS |
| organization_attachment_authorization | PASS |
| cross_organization_messaging_isolation | PASS |
| personal_messaging_unaffected | PASS |
| organization_calling_enterprise_permissions_and_isolation (MEMBER 403, outsider 403/404, audit names the member principal) | PASS |
| organization_audit_events | PASS |
| membership_revocation_is_immediate | PASS |
| suspension_fails_closed_and_reactivation_restores | PASS |
| archival_of_both_organizations | PASS |

### 7.2 Calling with real media

| Suite | Result | Covers |
|---|---|---|
| Calling enterprise [43479566836](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43479566836) | 21/21 | voice and video real media, reconnection, hold/resume, decline, cancel, missed-call history, group media, IVR keypad to extension, internal transfer (transferred party removed), extensions, ring groups, queues, tenant isolation, scope denial |
| Recording [43480807961](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43480807961) | 12/12 | consent required, non-participant denied, encrypted playback, cross-tenant denial, audit, deletion, retention worker |
| Voicemail [43481855456](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43481855456) | 7/7 | real audio lifecycle, isolation, access revocation, physical deletion, retention |
| Personal communications [43489312016](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43489312016) | 20/20 | native voice/video ring → accept → bidirectional frames (both sessions CONNECTED); Hub-launched voice and video; contact connection; block/unblock; ID rotation; call history without principal ids; meetings still available |

Organization calls use the governed personal calling scope. The organization relay is the Enterprise administration surface only (ADR 0056 §4, org-policy-calling-scope-section-12.md). The organization Enterprise permissions were verified in §7.1.

### 7.3 Meetings: 22/22 ([43479509962](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43479509962))

* A live conference in a real LiveKit room.
* Distinct canonical participant ids for same-name guests.
* Hands, spotlight and reconnect (same participant).
* Q&A restore, including denial for attendees.
* Realtime `qna.question_restored`.
* Cleanup.

### 7.4 Developer Platform and API Gateway: 30/30 ([43488534526](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43488534526))

* Developer Console operations.
* Scoped platform credentials calling Calling, Messaging and webhook families through the gateway, with quota headers.
* Missing scope → 403. Forged credential → 401. Revoked credential → 401.
* Tenant isolation between developer A and B.
* A signed webhook delivered (attempt `SUCCEEDED`, HTTP 204).
* Cleanup with 0 problems.

CORS:

* `https://cfmeet.com` → 200 and `https://www.cfmeet.com` → 204 (certify, and a probe from this session);
* a foreign origin → 403.

### 7.5 Personal Messaging: 15/15 guard checks, journey blocked ([43491691538](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43491691538))

Verified:

* anonymous access refused;
* contact connection is idempotent;
* E2EE device enrollment returns 503 (guard active);
* PRIVATE_E2EE and default conversation creation return 409 `E2EE_NOT_AVAILABLE` (no plaintext fallback);
* MANAGED is refused as a personal substitute (422);
* no partial conversations;
* block enforced by the Messaging owner.

Not run, and reported as such:

* `encryptedProductionDelivery: NOT_RUN_ENROLLMENT_BLOCKED`;
* `physicalDevicePush: NOT_RUN_NO_PROVIDER_CREDENTIALS_OR_DEVICE_EVIDENCE`.

The personal MANAGED realtime transport is active (`MESSAGING_SESSION_DEVICES_ENABLED=true`). The same realtime gateway carried the organization event in §7.1.

## 8. Section 8 journey matrix

| # | Journey | Result | Evidence |
|---|---|---|---|
| 1 | Register / login | PASS | org acceptance (3 fresh accounts), communications (2), messaging (2) |
| 2 | Create a personal meeting | PASS | meetings acceptance |
| 3 | Join meetings, voice/video | PASS | meetings acceptance (LiveKit), real-media calling suites |
| 4 | Personal app-to-app calls | PASS | communications 20/20 |
| 5 | Personal messages | **BLOCKED_EXTERNAL_DEPENDENCY** | messaging-verify (§0, §7.5) |
| 6 | Create an organization | PASS | org acceptance |
| 7 | Automatic platform scope and bindings | PASS | org acceptance |
| 8 | Invite and accept members | PASS | org acceptance |
| 9 | Teams and workspaces | PASS | org acceptance |
| 10 | Organization conversations | PASS | org acceptance |
| 11 | Realtime organization messaging | PASS | org acceptance |
| 12 | Organization voice/video calls | PASS for calls (governed scope, real media) and organization Enterprise surface | §7.2 |
| 13 | Enterprise Calling permissions | PASS | org acceptance, enterprise 21/21 |
| 14 | Suspension and revocation | PASS | org acceptance; revoked developer credential |
| 15 | Organization isolation | PASS | org acceptance |
| 16 | Developer Console backend | PASS | developer acceptance 30/30 |
| 17 | Gateway routing and CORS | PASS | certify, developer acceptance |
| 18 | Personal functionality still works | PASS | org acceptance `personal_messaging_unaffected`, communications, meetings |

Every suite tested unauthorized operations as well as successful ones.

## 9. Database integrity and migrations (certify)

* Every service: all migrations of the revision applied, with none missing.
  * Platform Access 7 (latest `m20261123_000007_organization_service_bindings`).
  * Organizations 5 (`m20261113_000005_automatic_service_bindings`).
  * Messaging 31 (`m20261203_000031`).
  * Calling 47+.
  * Meeting: every history, including `organization_integration_migrations`.
* Messaging outbox: pending 0, dead 0.
* Organizations outbox: DEAD 0, DELIVERED 31, PENDING 52. Every event also writes a never-dispatched `cfmeet.reconciliation` record row (`services/organizations/src/outbox.rs`). The 52 PENDING rows are consistent with those records. Certify counts by state, not destination, so that last point is an inference.
* Organizations data after acceptance: the acceptance organizations are archived. No legacy rows were written after the switch.

## 10. Certification and CI

* `certify` [43490164383](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43490164383): **CERTIFIED_READ_ONLY**. 13/13 services PASS. Provider safe defaults PASS. Core identity PASS. 25 internal routes probed publicly, 0 answered.
* Main CI was green for each dispatched SHA (`6182609b`, `73919312`, `58e22ec5`); the coordinated-release gate enforces this.
* The offline release tests (303 and more) passed in every run.
* Release gate change (§3 of the directive). These workflows still run on every push but are **advisory**, reported as warnings:
  * `telephony.yml`, `telephony-sip-certification.yml`, `sms-gateway.yml`, `numbering.yml`, `privacy-relay.yml`, `communications-e2e.yml`;
  * `backup-restore-drill.yml`, `cfmeet-offsite-backup.yml`.
* Still mandatory:
  * backend, Platform Access, Organizations, Messaging, Calling, Communications Hub;
  * Developer Platform, API Gateway, security scan gateway;
  * contracts, service boundaries, dependency security, migration guard;
  * certification, unified messaging e2e, Enterprise Calling certification.

## 11. Genuine remaining failures and blockers

1. **Personal Messaging message exchange: `BLOCKED_EXTERNAL_DEPENDENCY`.**
   * Required before it can be enabled: an independent cryptographic assessment of the production E2EE (MLS) configuration, owner sign-off on that reviewed configuration, and reviewed key custody and transparency anti-equivocation evidence.
   * Transparency witnesses are not configured.
   * Enabling MANAGED (server-readable) personal messaging instead would be a product and privacy decision that contradicts the recorded design. It was not done.
2. **No verified off-site backup.** This is accepted by the owner (§3) and remains a recovery risk.
3. **Mobile push** (APNs, FCM, Web Push): not configured. This needs provider credentials and is external.
4. **Developer production-access review:** submissions stay disabled until the owner names reviewers (`vars.CFMEET_PRODUCTION_ACCESS_REVIEWERS`, then `activate-production-access-review`). This is an owner decision.
5. **Resend email:** not activated; SMTP is in use. This needs the owner's Resend setup (resend-email-owner-handoff.md).

No first-party defect remains open. The three failures in this session were tooling defects, each fixed and re-run green:

* the inventory receipts;
* the acceptance realtime frame shape;
* the stale Messaging route expectation.

## 12. Disabled external-provider features (Africa's Talking credentials not available)

All of these were verified disabled by certify; none is represented as operational:

* SMS carrier (`SMS_GATEWAY_PROVIDER` unset);
* number inventory providers and porting;
* PSTN telephony and public inbound/outbound PSTN calling;
* callback relay, Enterprise SIP trunking, SIP edge `none`;
* Calling public inbound/outbound;
* Privacy Relay, contact vault, relay number pool;
* phone OTP / SMS verification (`PROVIDER_BLOCKED`);
* mobile push.

The gateway refuses `numbering`, `sms` and `privacy-relay` (`NOT_ROUTED`). The code and internal tests of these features remain in the repository. No simulated telecom result is used as evidence anywhere in this report.
