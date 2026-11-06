# CF Meet Sections 8–23 — final closure report

Status words: `ENGINEERING_CLOSED` means implemented, tested in CI, with no open defect. `PRODUCTION_VERIFIED` means running in production and verified there by a completed run. `BLOCKED_EXTERNAL` means it needs something outside engineering control. `DEFERRED_BY_OWNER` means an explicit owner decision. `FAILED` means what it says. A passed build or CI run is never reported as a deployment; production statuses below are taken only from completed workflow runs.

## 0. Verdict

<!-- VERDICT -->
_Pending the coordinated release and independent verification (sections 6–7)._

## 1. Branches

| Item | Value |
|---|---|
| `main` = `staging` | `<!-- HEAD -->` (fast-forward promotions only; no force push) |
| Previous `main` at the start of this closure | `71eedfebcdac788c80a2f3743527cc953b07e257` |

## 2. Final service image source

`08729298656daf69e32870390888554675d44dde`. Every later commit changes only `infra/`, `docs/` and `.github/workflows/`. The release workflow refuses any difference under `services/`, `packages/service-auth`, `packages/communications-policy` and the bundled contracts between the image source and the release commit. The defective `c2a8035` images were never deployed.

## 3. Images (built by run [42177533187](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177533187), accepted by stage)

| Service | Image id | Registry digest |
|---|---|---|
| platform-access | `sha256:4bdb89750530d5a2247013fb34c993f91a2c229d936317c938e9ebac2abd0e38` | `ghcr.io/cosmicforgehealthnet/cfmeet-platform-access@sha256:afe1b4236e952c8dd23c2ee63e0f4c78bb7e00e84fe97bd1adf6b87c38f217ed` |
| meeting-service | `sha256:08eee845fcc38ae5e2bdaf8397f90bdf1929c32e53069954b70dd92b8d0813ed` | `…cfmeet-meeting-service@sha256:e8eb556e5152f5f10bc57188bfd4e8a724c94d545cf6f1f71f2921c72839c0b3` |
| calling | `sha256:daa635074927f466bf17510201a7d1a92ed9c44debee7ebb29aabf88e68fadc8` | `…cfmeet-calling@sha256:37921c677426dfbfae0299dc14a194d0d2578f04fd7dc8bc13f2feded1262d98` |
| messaging | `sha256:48bced4b1c50ffce39ed5a27505e361878884beb4dc10eb387c35a5788f14421` | `…cfmeet-messaging@sha256:fc8fb22177a817061a1fe5d9a77b4c07583a01576151470be53f08b79dd2e087` |
| communications | `sha256:dde1a55819d85ce0a0c47da08948bf3b8b8e61336d67ec45756bd6ce3a43c5f1` | `…cfmeet-communications@sha256:623c5e887854f0d8746c5f12a6af4ba8bd75740400a62662bb522d00d988ab1c` |

## 4. CI on the image source

| Workflow | Run | Result |
|---|---|---|
| Communications Hub foundation, including the real five-service journey | [42177519122](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519122) | success |
| Communications end-to-end journeys | [42177519091](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519091) | success |
| Calling service | [42177519048](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519048) | success |
| Calling RTC certification (real LiveKit) | [42177519076](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519076) | success |
| Enterprise calling certification | [42177519057](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519057) | success |
| Part VI cross-service certification | [42177519011](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519011) | success |
| Service boundaries | [42177519072](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519072) | success |
| Dependency and image security | [42177519042](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519042) | success |
| Telephony / Privacy Relay | [42177519041](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519041) / [42177519034](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519034) | success |
| Unified Messaging journeys | [42177519023](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42177519023) | both backend jobs success. The web client in Chromium fails: frontend-owned, identical on `main` before this work, tracked in [#37](https://github.com/CosmicforgeHealthnet/cfmeet/issues/37). Not counted as a pass |
| Contracts, SDK TypeScript / Dart, Messaging, Meeting service, API Gateway, Platform Access, Developer Platform, Numbering, SMS Gateway | 42170779065, 42170778834 / 42170778939, 42170778719, 42170778776, 42170778957, 42170778700, 42170778810, 42170778901, 42170778794 (`cc12940`; their inputs are unchanged at the image source) | success |

A one-time RTC failure ([42173510716](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42173510716)) was root-caused, not retried away. Real LiveKit's `room_started` callback briefly holds a foreign-key lock on the call row, so the best-effort post-create queue dispatch skips it. In production the one-second worker then offers the call. The test now runs that worker pass. Product behaviour is unchanged.

## 5. Isolated staging

[42180933332](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42180933332) on the exact images above, with fresh isolated owner databases (all five migration histories applied). **22 of 22 checks PASS**:

- authenticated ingress, private policy and public Swagger (73 operations)
- native accounts without phone
- truthful capabilities
- relationship gates against direct APIs
- QR invitations
- idempotent requests
- canonical direct conversation with a MANAGED message
- native voice and video with bidirectional frames
- **Hub-launched voice and video from a contact: ringing, acceptance, CONNECTED, frames both ways, history, idempotent retry**
- **a Hub-launched conversation call writing exactly one card per viewer**
- aliases and common groups
- block/unblock with Hub-launch refusal
- ID rotation
- principal preservation
- Meetings availability
- favorites and lists
- **truthful delivery status: APNS, FCM and WEB_PUSH all `NOT_CONFIGURED`**
- monotonic sync
- canonical Calling reuse
- history and timeline without principal ids
- devices
- conversation timeline and Meetings through their owners
- fixture cleanup

## 6. Production release

[42189631003](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42189631003) (`communications-release`, ref `main` at `ef4f934`, images `0872929`). Before replacement it checked that the staged image digests match, that the trust and private routes are in place, and that public ingress is restricted. It applied only the additive owner feature migrations and the isolated Hub schema. Each service was replaced with its previous container retained for `communications-rollback`. Calling was replaced only once no call was active. **22 of 22 production checks PASS**, including:

- `existing_enterprise_calling_acceptance`
- `existing_backend_compatibility`
- `public_ingress_denies_private_account_endpoints`
- `hub_contact_launched_voice_and_video_connect_with_bidirectional_media_and_history`
- `hub_call_launch_reuses_canonical_calling`
- `preferences_truthful_delivery_status_and_monotonic_sync` (push providers `NOT_CONFIGURED`)

The running images are exactly the staged set in section 3. The production flags are unchanged: Enterprise Calling, recording and voicemail on; public PSTN off.

**Calling launch credential:** `b8d54e20-42ca-4c93-a154-6beacffe00e5`, issued by the release. Scope `calls.write` only, belonging to the governed personal scope's service account, expiring 2027-11-06 (364 days). Only the id is stored; the secret was discarded when issued. Calling re-verifies it with Platform Access on every launch, so a revoked or expired credential fails closed (503); the CI test covers revocation. A daily journal check alerts below 45 days. `communications-verify` fails if Calling runs another id or fewer than 30 days remain. A release reuses it with 30 or more days left and otherwise issues a new one.

## 7. Independent production verification

<!-- VERIFY -->
_Pending._

## 8. Section 8 infrastructure delivered in production

| Item | Run(s) | Evidence |
|---|---|---|
| Private TLS: durable lineage mounts | [42185815717](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42185815717) | The pinned `archive/…/fullchain4.pem` was replaced by read-only `live/` and `archive/` directory mounts. All 8 private routes returned 200 before and after. The previous proxy is retained, stopped. Two earlier attempts were stopped safely: [42182753156](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42182753156) found no startup wait (rolled back, routes 200), and [42184658316](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42184658316) found the reload race in its CI proof (production untouched). Both were fixed with tests |
| TLS automation v2 | [42188709330](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42188709330) | Targets are the private proxy and host nginx, which uses certbot's nginx authenticator and installer on the managed lineage. After the live hook, each presented the managed certificate on 5 of 5 independent verified connections. The daily check needed no reload. `certbot renew --dry-run` exited 0. Status WARN, band 45 (expiry 2026-12-21, 45 days; certbot renews at 30). Runbook: [tls-certificate-automation.md](tls-certificate-automation.md) |
| Operator notification | [42189603354](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42189603354) | Delivery test comment [6680863924](https://github.com/CosmicforgeHealthnet/cfmeet/issues/38#issuecomment-6680863924) on [#38](https://github.com/CosmicforgeHealthnet/cfmeet/issues/38), read back by the run and again independently. The private proxy, host nginx and the public certificate seen from GitHub all match the managed certificate. Daily schedule active. Delivery is verified to the GitHub issue; e-mail or push to a person follows that account's GitHub notification settings, which engineering cannot observe |
| Storage routes without Docker addresses | [42186633760](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42186633760) | Both bucket routes use the loopback relay `127.0.0.1:19000`. Unsigned requests get 403 and signed round trips pass for Calling artifacts and Messaging attachments. No published store port. CI proves the old route shape breaks when the store is recreated, while the relay follows the store |
| Before-state inspections | [42173910632](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42173910632), [42173920297](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42173920297) | pinned certificate and `172.23.0.13:9000` routes recorded before any change |

## 9. Closure matrix

| Section | Engineering | Production | Evidence | Remaining |
|---|---|---|---|---|
| 8 Messaging infrastructure, scanning, ingress | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (scanner, realtime ingress, storage, TLS; see 7–8) | scan gateway and messaging verify runs (7), [42186633760](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42186633760), [42188709330](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42188709330) | PRIVATE_E2EE activation BLOCKED_EXTERNAL (#13) |
| 8 Private TLS renewal | ENGINEERING_CLOSED | PRODUCTION_VERIFIED | 8, runbook | — |
| 8 Storage routes | ENGINEERING_CLOSED | PRODUCTION_VERIFIED | 8 | — |
| 9 Feature inventory | ENGINEERING_CLOSED | PRODUCTION_VERIFIED for the inventory's active features | delivery record §9 | DEVICE_ONLY features are device work by design |
| 10 Calling/Messaging integration | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (Hub-launched voice and video with media; Enterprise regression) | 5, 6 | — |
| 11 Groups and chat controls | ENGINEERING_CLOSED | PRODUCTION_VERIFIED for MANAGED contexts | Hub suites, stage/release | personal PRIVATE_E2EE groups BLOCKED_EXTERNAL (#13) |
| 12 Notifications and devices | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (truthful status, devices, owner reconciliation) | 5, 6 | provider delivery and physical devices BLOCKED_EXTERNAL |
| 13 Meetings integration | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (Meetings independent; conversation meetings in stage) | 5, 6 | — |
| 14 Database migrations | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (additive owner and Hub histories applied; rollback-safe) | 6 | — |
| 15 Public APIs | ENGINEERING_CLOSED (all 73 operations routed: `every_published_communications_operation_is_routed`) | PRODUCTION_VERIFIED (served Swagger 73, no internal paths) | 4–7 | phone operations DEFERRED_BY_OWNER (not exposed) |
| 16 Internal APIs | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (private routes, signed service auth) | 5–7 | — |
| 17 Contracts and events | ENGINEERING_CLOSED (owner reads proved equivalent to producers: call-card sweep, device reconciliation, feed entries) | PRODUCTION_VERIFIED | delivery record §17 | — |
| 18 End-to-end certification | ENGINEERING_CLOSED | PRODUCTION_VERIFIED for every journey in 5–7 | 5–7 | T07–T09 (personal E2EE), T21 (push and devices) BLOCKED_EXTERNAL; T23 load DEFERRED_BY_OWNER |
| 19 VPS capacity and operations | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (inventory, recovery) | 7 | isolated load environment DEFERRED_BY_OWNER |
| 20 Workstreams and Definition of Done | ENGINEERING_CLOSED | PRODUCTION_VERIFIED within the limits above | this report | — |
| 21 Fixed decisions | ENGINEERING_CLOSED | PRODUCTION_VERIFIED (no phone, no new antivirus, no AWS, frontend untouched) | 6 | — |
| 22 Frontend and operator handoff | ENGINEERING_CLOSED | — (documentation) | [handoff](../api/communications-unified-handoff.md), [TLS runbook](tls-certificate-automation.md) | Chromium journey frontend-owned (#37) |
| 23 Production release and acceptance | ENGINEERING_CLOSED | PRODUCTION_VERIFIED | 6, 7 | — |

## 10. External dependencies and owner decisions

| Item | Status | Exact need |
|---|---|---|
| Production PRIVATE_E2EE | BLOCKED_EXTERNAL | Independent cryptographic assessment, residual-risk acceptance and owner approval recorded on [#13](https://github.com/CosmicforgeHealthnet/cfmeet/issues/13). Package: [private-e2ee-independent-review-package.md](../security/private-e2ee-independent-review-package.md). The production validator still refuses `MESSAGING_E2EE_DEVICES_ENABLED=true` |
| Push notifications to devices | BLOCKED_EXTERNAL | APNs key, FCM project and VAPID keys from the owner, plus physical devices. The backend reports `NOT_CONFIGURED` truthfully (verified in stage) |
| Physical-device acceptance | BLOCKED_EXTERNAL | needs the credentials above and real devices; never simulated as passed |
| Phone verification | DEFERRED_BY_OWNER | not exposed; email and Google registration unchanged; the routing test proves no phone route is served |
| Off-server backups / total-VPS-loss recovery | DEFERRED_BY_OWNER | same-host copies only; VPS-loss recovery is not claimed |
| Isolated load environment | DEFERRED_BY_OWNER | no load tests on the shared production VPS (standing rule) |
| Web client Chromium journey | frontend-owned | [#37](https://github.com/CosmicforgeHealthnet/cfmeet/issues/37): the collapsed device panel hides "Recover history" from the test |
