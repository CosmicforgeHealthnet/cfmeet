# CF Meet backend — final closure

Date: 2026-11-11 · Environment: production (Contabo VPS, `api.cfmeet.com`)

## Verdict

**`BACKEND_CORE_CLOSED_ORG_CUTOVER_PENDING`**

- Every eligible first-party capability is deployed, activated, and certified in production.
- The Organizations authority cutover, and the organization-scoped Enterprise Calling that depends on it, are blocked by documented owner gates. They are not blocked by missing backend work.
- Provider-dependent capabilities stay intentionally disabled (`EXTERNAL_DEPENDENCY_PENDING`).

This ends the backend development phase. What remains are owner-controlled operational steps on code and runbooks that already exist (see [Remaining owner-controlled steps](#remaining-owner-controlled-steps)).

## Production image and release evidence

| Item | Value |
|---|---|
| Production image revision | `4904ee33c2defdb37dac680119962d62c7ed9ca5`. All 13 containers run it, and each image ID matches the coordinated release manifest. |
| Coordinated releases | [42707740486](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42707740486) (`bde67a8b`, first release) · [42723081476](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42723081476) (`4904ee33`, current) |
| Certification at closure | [43070726146](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43070726146) (2026-11-11): `CERTIFIED_READ_ONLY`, 0 failures. Previous run: [42909534672](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42909534672) (2026-11-10), same result. |
| Source vs. `main` | No service build input changed between `4904ee33` and the `main` head `d2deb862`. The later commits are release tooling and docs only, so no redeploy is needed. |
| Container stability | Restart count 0 on every container since the release (2026-11-10 00:24–00:47 UTC) |
| Report | [cfmeet-final-production-certification.md](cfmeet-final-production-certification.md) |

## The 13 services

| Service | Container | Production status | Readiness · DB · migrations | Anonymous request rejected | Service-to-service |
|---|---|---|---|---|---|
| Platform Access | `cfmeet-platform-access` | LIVE | `/ready` 200 · own role · 6/6 | 401 | PASS |
| Organizations | `cfmeet-organizations` | LIVE (provisioning + event delivery active; not yet the authority) | `/ready` 200 · own role · 4/4 | 401 | PASS |
| Messaging | `cfmeet-messaging` | LIVE | `/ready` 200 · own role · 31/31 | 401 | PASS |
| Calling | `cfmeet-calling` | LIVE | `/health` 200 (DB) · own role · 47/47 | 401 | PASS |
| Communications | `cfmeet-communications` | LIVE | `/ready` 200 · own role · 4/4 | 401 | PASS |
| Numbering | `cfmeet-numbering` | DEPLOYED, providers off | `/ready` 200 · own role · 10/10 | 401 | PASS |
| Privacy Relay | `cfmeet-privacy-relay` | DEPLOYED, disabled | `/health` 200 (DB) · own role · 17/17 | 401 | PASS |
| SMS Gateway | `cfmeet-sms-gateway` | DEPLOYED, no provider | `/ready` 200 · own role · 4/4 | 401 | PASS |
| Telephony | `cfmeet-telephony` | DEPLOYED, disabled | `/health` 200 (DB) · own role · 17/17 | 401 | PASS |
| Meeting Service | `cfmeet-production-backend` | LIVE (account authority, meetings) | `/health` 200 · own role · 33/33 | 401 | PASS |
| Developer Platform | `cfmeet-developer-platform` | LIVE | `/ready` 200 · own role · 10/10 | 401 | PASS |
| Developer Platform worker | `cfmeet-developer-platform-worker` | LIVE | stays running · own role · (shares DB) | n/a | PASS |
| API Gateway | `cfmeet-api-gateway` | LIVE (public routing on) | `/ready` 200 · no DB | 401 | PASS |

Public surface:
- 236 public GET routes probed: 141 reject anonymous requests and 95 are not yet routed (gateway families not activated).
- None returns 2xx without credentials, and none returns 5xx.
- None of the 25 internal, operator or callback routes is reachable from the internet.
- All 10 OpenAPI documents are served.
- CORS for `https://cfmeet.com` is correct.
- The TLS certificate has 40 days left.

## Completed activation operations (production evidence)

Each operation below is recorded as `done` in the release journal and was followed by a passing regression. The 2026-11-11 certification confirms each flag is still set.

| # | Operation | Run | What it set | Still in place |
|---|---|---|---|---|
| 1 | `release` `bde67a8b` | [42707740486](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42707740486) | All 13 services, with new services dark | superseded by #7 |
| 2 | `activate-provisioning` | [42709421203](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42709421203) | Platform Access ← Organizations grant; `ORGANIZATIONS_PROVISIONING_ENABLED=true` | yes |
| 3 | `activate-org-events` | [42710192324](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42710192324) | Meeting ← Organizations grant; `ORGANIZATION_EVENTS_ENABLED=true`; `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service` | yes (grant has current key; outbox has no `DEAD` events) |
| 4 | `org-cutover-export` | [42711430322](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42711430322) | Read-only inventory and export (see below) | n/a |
| 5 | `activate-developer-platform` | [42712263986](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42712263986) | Grants to API Gateway and Developer Platform; `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=true`; `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true` | yes |
| 6 | `activate-gateway-routing` | [42713082843](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42713082843) | Public routing for the 8 Developer Platform families through the gateway | yes (34/34 routes answer 401) |
| 7 | `release` `4904ee33` | [42723081476](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42723081476) | Current images on all 13 services | yes |
| 8 | `activate-first-party-features` | [42730700547](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42730700547) | Meeting: conference realtime, quality telemetry, webinar mode, email notifications. Messaging: session devices. | yes |

None of these was repeated, and no credential was rotated or overwritten during closure.

## Feature activation status

| Feature | Status | Evidence |
|---|---|---|
| Accounts, sign-in, sessions, profile, notifications (incl. email) | ACTIVE | Meeting flags; SMTP configured; first-party features run #8 |
| Google sign-in | ACTIVE | client configured |
| Meetings, calendar, conference realtime, webinar mode, quality telemetry | ACTIVE | first-party features run #8 |
| Messaging (session devices, realtime) | ACTIVE | `MESSAGING_SESSION_DEVICES_ENABLED=true`; Messaging 15/15 live acceptance |
| Communications Hub | ACTIVE | Communications 20/20 live acceptance |
| Personal calling (app calling, new calls; self-hosted LiveKit media) | ACTIVE | `CALLING_APP_CALLING_ENABLED`, `CALLING_NEW_CALLS_ENABLED`, `CALLING_MEDIA_PROVIDER=livekit` |
| Enterprise Calling core, call recording, voicemail | ACTIVE | `ENTERPRISE_CALLING_ENABLED`, `CALL_RECORDING_ENABLED`, `VOICEMAIL_ENABLED=true` (artifacts AES256); live acceptance PASS |
| Organization provisioning | ACTIVE | run #2 |
| Organization event delivery to the Meeting Service | ACTIVE | run #3 |
| Developer Platform, developer gateway API | ACTIVE | run #5 |
| API Gateway public routing (Developer Platform families) | ACTIVE | run #6 |
| Organizations as the authority (cutover) | **PENDING (owner gates)** | see next section |
| Organization-scoped Enterprise Calling (`activate-enterprise-calling`) | **PENDING (needs authoritative Organizations)** | code gate: requires `ORGANIZATIONS_AUTHORITY_MODE=authoritative` |
| Gateway families for Messaging, Numbering, Privacy Relay, SMS | Not activated | owner decision (public API exposure); routes return 404 by design |
| Healthcare profile | OFF | `FEATURE_HEALTHCARE_PROFILE_ENABLED=false`; owner decision |

Live acceptance evidence for Communications, Messaging, Enterprise Calling, recording and voicemail comes from the backend closure report: runs 42740615988, 42716127525, 42717080104, 42718224193 and 42719418460. The closure phase made no new activation, so no new authenticated acceptance run was required. Authenticated journeys were not re-run, because they would write production data.

## Organization cutover status

Following [organizations-cutover-runbook.md](organizations-cutover-runbook.md) and the coordinated release procedure:

| Step | Status | Evidence / blocker |
|---|---|---|
| 1. Inventory | DONE | Run 42711430322: **0 organizations, 0 memberships**, 0 duplicates, 0 ownerless, 0 slugs outside the contract, 0 orphan spaces |
| 2. Export | DONE | Run 42711430322 (2026-11-09): schema 1, counts 0/0, checksum `e3b0c442…b855` (empty set). Must be repeated right before the import. |
| Gate 1: encrypted off-host backup + restore drill | **NOT MET** | Owner confirmed on 2026-11-11 that no verified off-host backup exists. [backup-and-restore.md](backup-and-restore.md) records the age key pair and the off-host target as OWNER items, and every backup as `LOCAL_ONLY — not disaster recovery`. |
| 3. Import + reconciliation | BLOCKED by gate 1 | `org-cutover-import` refuses without `confirm=offsite-backup-verified`. Not run. |
| 4. Shadow authority | BLOCKED | Requires step 3. Every activation also requires a completed coordinated release of the dispatched `main` head. The owner declined to release `d2deb862` (no service-code change) for this purpose. |
| 5. Seven-day zero-mismatch observation | NOT STARTED | Starts when shadow mode is entered; at least 7 days |
| 6. Quiesce + final reconciliation + authoritative | NOT STARTED, irreversible | Owner gates 1 and 6 (`offsite-backup-verified,quiesced`), after the ingress block of the three legacy write routes |
| Organization-scoped Enterprise Calling | NOT STARTED | `activate-enterprise-calling` requires authoritative mode. It then grants Meeting Service the Platform Access, Calling and Messaging permissions and sets `ORGANIZATION_CALLING_SCOPE=organization`. |

**Safe current state, verified 2026-11-11:**
- The Meeting Service is the only organization writer (`ORGANIZATIONS_AUTHORITY_MODE` unset, so `legacy`).
- No import has run into the Organizations Service.
- In `legacy` mode the first-party organization routes answer `FEATURE_DISABLED` (runbook table).
- Nothing irreversible has been done; rollback per runbook §7 is a no-op.
- The legacy dataset is currently empty, so the cutover carries little data risk once the gates are met. The gates themselves are mandatory and were not bypassed.

## External dependencies (`EXTERNAL_DEPENDENCY_PENDING`)

Each item below is implemented, deployed, and disabled until a provider contract and credentials exist. None of them is unfinished internal backend work.

| Capability | Service | Switch (production value) | Needs |
|---|---|---|---|
| SMS delivery | SMS Gateway | `SMS_GATEWAY_PROVIDER` unset | SMS carrier account |
| Phone number inventory | Numbering | `NUMBERING_PROVIDERS` unset | Number provider |
| Number porting | Numbering | `NUMBERING_PORTING_ENABLED=false` | Carrier porting agreement |
| Public PSTN calling (outbound/inbound), callback relay, enterprise SIP | Telephony, Calling | all `false`; `TELEPHONY_SIP_EDGE=none` | PSTN/SIP carrier |
| Privacy relay, contact vault, relay number pool | Privacy Relay | all `false` | Relay numbers from a carrier |
| Mobile push notifications | Messaging | `MESSAGING_PUSH_PROVIDER=none` | APNs / FCM credentials |
| End-to-end encrypted messaging devices | Messaging | `MESSAGING_E2EE_DEVICES_ENABLED=false` | Owner security sign-off for private E2EE launch |
| Customer webhooks | Developer Platform | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=false`, delivery mode shadow | Owner launch decision and egress policy |
| Production access workflow | Developer Platform | `false` | Owner review process |
| API Gateway WebSockets | API Gateway | `false` | Owner launch decision |

## API documentation and frontend integration

- Production API: `https://api.cfmeet.com`. Swagger UI: `https://api.cfmeet.com/swagger-ui/`. Index: `https://api.cfmeet.com/api-docs/swagger-config.json` (10 documents, all served).
- Contracts (source of truth): `packages/contracts/` ([manifest](../../packages/contracts/manifest.json)). Gateway catalogue: [`infra/api-gateway/route-manifest.json`](../../infra/api-gateway/route-manifest.json).
- [Final API inventory](../implementation/final-api-inventory.md) · [API documentation index](../api/README.md).
- Frontend: [handover](../frontend/cfmeet-frontend-handover.md) · [endpoint reference](../frontend/cfmeet-frontend-endpoint-reference.md). Organization routes stay `FEATURE_DISABLED` until the cutover, so the frontend must keep treating them as unavailable.
- Operations: [coordinated release runbook](coordinated-production-release.md) (incl. `certify`) · [backend closure report](../implementation/cfmeet-backend-closure-report.md).

## Remaining owner-controlled steps

None of these needs new backend code.

1. **Off-host backups (gate 1):**
   - create the age key pair and choose the off-host target ([backup-and-restore.md](backup-and-restore.md) setup steps 1–3);
   - run the first production backup and the restore drill;
   - set `CFMEET_BACKUP_REQUIRE_OFFSITE=1`.

   This also closes the disaster-recovery gap: today, total loss of the VPS is not recoverable.
2. **Release decision:** to run any activation, `main`'s head must be a completed coordinated release. Either release the current head (no service-code change) or keep `main` pinned at a released SHA while activating.
3. **Organization cutover:** run, in this order:
   1. `org-cutover-export` again, to get a fresh export;
   2. `org-cutover-import offsite-backup-verified`;
   3. `org-cutover-shadow`, followed by at least 7 days of zero-mismatch evidence (runbook §5);
   4. a backup;
   5. the ingress block and the `quiesced` confirmation;
   6. `org-cutover-authoritative offsite-backup-verified,quiesced`, which is irreversible.
4. **Organization-scoped Enterprise Calling:** run `activate-enterprise-calling` after step 3.
5. **Provider integrations:** contracts and credentials for each external dependency above. Each one is then a configuration change on an already-deployed service.
6. **Authenticated acceptance:** run the acceptance suites with dedicated acceptance accounts after any future activation (they write data, so they need owner approval).
7. **TLS:** confirm the automated renewal of the `api.cfmeet.com` certificate (40 days left on 2026-11-11).
