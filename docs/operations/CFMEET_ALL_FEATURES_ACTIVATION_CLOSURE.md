# CF Meet — all-features production activation closure

Date: 2026-11-12 · Environment: production (`api.cfmeet.com`, Contabo VPS) · Baseline: [CFMEET_BACKEND_FINAL_CLOSURE.md](CFMEET_BACKEND_FINAL_CLOSURE.md) (2026-11-11)

## Verdict

**`NOT 100% — ACTIVATION PARTIAL, REMAINDER BLOCKED BY OWNER ACTIONS, AN EXTERNAL ASSESSMENT AND ELAPSED TIME`**

<!-- VERDICT_DETAIL -->

## Exclusions (owner, unchanged)

| Feature | Owner service | State | Status |
|---|---|---|---|
| SMS delivery and phone OTP | SMS Gateway | `SMS_GATEWAY_PROVIDER` unset; release/verify/certify refuse any provider | EXCLUDED_BY_OWNER |
| External PSTN / SIP calling (inbound, outbound, callback relay, enterprise SIP) | Telephony, Calling | all `false`, `TELEPHONY_SIP_EDGE=none` | EXCLUDED_BY_OWNER |
| Telephone number provisioning, inventory, porting | Numbering | `NUMBERING_PROVIDERS` unset, porting `false`; `/v1/numbers` unrouted and gateway domain `numbering` disabled | EXCLUDED_BY_OWNER |
| Privacy Relay, contact vault, relay numbers | Privacy Relay | all `false`; `/v1/privacy-relay` unrouted and gateway domain `privacy-relay` disabled | EXCLUDED_BY_OWNER |

<!-- ACTIVATED -->

## Messaging

| Feature | Owner service | Previous state | Final production state | Activation | API availability | Evidence | Remaining dependency | Status |
|---|---|---|---|---|---|---|---|---|
| First-party realtime (MANAGED session devices, WebSocket `/v1/realtime`, tickets) | Messaging | ACTIVE (run 42730700547) | unchanged, `MESSAGING_SESSION_DEVICES_ENABLED=true` | `activate-first-party-features` | `/v1/realtime` direct route | Messaging 15/15 live acceptance (backend closure); certify, this report | — | ALREADY_ACTIVE |
| Private E2EE device messaging (device registration, KeyPackages, key transparency, device sync / history transfer, PRIVATE_E2EE conversations) | Messaging | OFF | OFF. The Messaging config validator refuses `MESSAGING_E2EE_DEVICES_ENABLED=true` in production (`services/messaging/src/config/mod.rs:471-490`), with no override, by design | none: not activated | routes answer `E2EE_DEVICES_DISABLED` | [messaging-section-8-security-review.md](messaging-section-8-security-review.md): code-level review only. Required before launch: a qualified **independent cryptographic assessment** (MLS integration, device PoP, AAD, attachments, history/recovery, transparency/equivocation, clients); a pinned KEYTRANS profile with an **independent witness** (production heads are `UNWITNESSED`); hpke-rs MPL-2.0 legal review; critical/high findings closed; **owner authorization** of the exact configuration ([commissioning package](../security/private-e2ee-independent-review-package.md): "No independent assessment has been performed") | independent assessment + witness + owner sign-off | BLOCKED_EXTERNAL_DEPENDENCY |
| Push notifications: web (VAPID), Android (FCM), iOS (APNs); device push registration and delivery | Messaging (+ Hub, Calling) | OFF (`MESSAGING_PUSH_PROVIDER=none`, no adapter credentials) | OFF. Release/verify/certify now also refuse any `MESSAGING_PUSH_APNS_*`, `_FCM_*` or `_WEB_*` variable (the adapters switch on from these, whatever the provider flag says) | guard hardened in PR #55; nothing activated | `PUT /v1/messaging/devices/{id}/push-registration` exists, but registration needs an ACTIVE E2EE device row (`services/messaging/src/realtime/push.rs:148`), so it cannot succeed while E2EE is refused | Web Push is native (RFC 8030/8291/8292, self-generated VAPID key, no third-party account), but: (1) push registration depends on E2EE devices (row above); (2) **no route publishes the VAPID public key** (`WebPush::application_server_key()` is never called), so browsers cannot subscribe; (3) iOS needs an Apple Developer `.p8` key, team id, key id and bundle topic; Android needs a Firebase service-account JSON; (4) Calling has no `PushDispatcher` and clients have no service worker / `expo-notifications` registration ([profile-notifications-sections-9-11.md](profile-notifications-sections-9-11.md)). No APNs/FCM delivery is claimed. | E2EE approval; APNs and FCM credentials; a VAPID-key route and Calling push dispatcher (code); client registration; physical-device certification | BLOCKED_OWNER_ACTION |

## Healthcare profile

| Feature | Owner service | Previous state | Final production state | Activation | API availability | Evidence | Remaining dependency | Status |
|---|---|---|---|---|---|---|---|---|
| Healthcare profile (`meetingType` / `securityProfile` HEALTHCARE, `HEALTHCARE_APPOINTMENT` access) | Meeting Service | OFF | OFF (`FEATURE_HEALTHCARE_PROFILE_ENABLED=false`; healthcare space creation answers `FEATURE_DISABLED`) | none: not activated | refused | Controls that exist: restrictive profile defaults (recording/transcription off, no external AI, forced waiting room, 7-day retention ceilings, enhanced audit), metadata redaction, fail-closed revocation, explicit consents. Not met: (1) the **HealthNet appointment-authorization connection** is not configured (`HEALTHNET_APPOINTMENT_AUTHZ_URL` unset → every appointment join is denied; the production connection is DEFERRED, `docs/PHASE2_MEETING_DOMAIN.md:190-194`; journey J-08 PARTIAL with a test double); (2) the retention policy (DATA-010) is an undecided owner decision; (3) no data-protection assessment exists: the profile links identities to clinical role tokens (`PATIENT`, `DOCTOR`), appointment references and validity windows, but no document classifies this as PHI / NDPA / GDPR special-category data, and there is no DPIA or processor agreement; (4) the frontend pre-join consent contract mismatch (FE-001). Enabling the flag without these would expose regulated-data flows without their controls. | HealthNet authority endpoint and token; owner retention decision; compliance assessment (DPIA / data classification) and owner approval | BLOCKED_OWNER_ACTION |

## Organizations

| Feature | Owner service | Previous state | Final production state | Activation | API availability | Evidence | Remaining dependency | Status |
|---|---|---|---|---|---|---|---|---|
| Organization provisioning | Organizations | ACTIVE (run 42709421203) | unchanged | `activate-provisioning` | internal | backend closure; certify, this report | — | ALREADY_ACTIVE |
| Organization event delivery to the Meeting Service | Organizations → Meeting | ACTIVE (run 42710192324) | unchanged, outbox without `DEAD` events | `activate-org-events` | internal | certify, this report | — | ALREADY_ACTIVE |
| Encrypted off-host backup destination | VPS tooling + GitHub Actions artifact store | NOT MET: no off-host copy (owner confirmed 2026-11-11) | tooling deployed to `main` (`cfmeet-offsite-backup.yml`, daily 02:17 UTC + dispatch); **no backup taken yet**: the owner's age public recipient(s) and the drill identity do not exist | PR #55 | n/a | backup → age encryption on the VPS → off-host artifact (90 days) → restore drill on a separate runner → record on the VPS. The drill was proven end to end against PostgreSQL 16 (including tamper refusal); `org-cutover-import` now refuses without the record. No production backup evidence is claimed. | **Owner**: create the age key pair(s) and set `vars.CFMEET_BACKUP_AGE_RECIPIENTS` and `secrets.CFMEET_BACKUP_AGE_IDENTITY` ([backup-and-restore.md](backup-and-restore.md#production-off-host-copy-coordinated-release-tooling)), then dispatch the workflow | BLOCKED_OWNER_ACTION |
| Verified restore drill | same | NOT MET | not run: requires the off-host copy above | — | — | — | owner key step above | BLOCKED_OWNER_ACTION |
| Organization export, import and reconciliation | Meeting → Organizations | export done 2026-11-09 (0 orgs, 0 memberships); import BLOCKED by gate 1 | unchanged: import refuses without the verified off-host record **and** the owner's `offsite-backup-verified` | — | — | run 42711430322 | gate 1 (above); then `org-cutover-export`, `org-cutover-import offsite-backup-verified` | BLOCKED_OWNER_ACTION |
| Shadow authority | Meeting | NOT STARTED | NOT STARTED: requires the import | — | — | — | the import | BLOCKED_OWNER_ACTION |
| Seven-day zero-mismatch evidence | Meeting / Organizations | NOT STARTED | NOT STARTED: the observation begins when shadow mode is entered; it cannot be shortened or claimed early | — | — | — | shadow + **7 elapsed days** (runbook §5) | BLOCKED_EXTERNAL_DEPENDENCY (elapsed observation period) |
| Quiesce and authoritative cutover (irreversible) | Meeting / Organizations | NOT STARTED | NOT STARTED | — | first-party organization, workspace, team and policy routes stay `FEATURE_DISABLED` (frontend must keep treating them as unavailable) | — | 7-day evidence; owner gate 6 (`quiesced`), the ingress block of the three legacy write routes, owner acceptance | BLOCKED_OWNER_ACTION |
| Organization management, workspaces, memberships, teams, policies; organization-scoped Messaging | Meeting (facade) → Organizations | gated on authoritative | unchanged | — | `FEATURE_DISABLED` until authoritative (by design: one writer at a time) | runbook | authoritative cutover | BLOCKED_OWNER_ACTION |
| Organization-scoped Enterprise Calling and per-organization service bindings | Meeting, Calling, Messaging, Platform Access | requires authoritative | unchanged | `activate-enterprise-calling` (code gate: `ORGANIZATIONS_AUTHORITY_MODE=authoritative`) | `409 ORGANIZATION_CALLING_DISABLED` per organization until bound | runbook §4.4 | authoritative cutover; then per organization: service account + credential in its FIRST_PARTY triple (owner decision, ADR 0058 §1), scopes and limits, `PUT …/service-bindings/{calling,messaging}` | BLOCKED_OWNER_ACTION |
| Authorization, tenant isolation and service bindings (organizations) | all | — | verified for the active topology by certify (grants with current issuer keys, own DB roles, anonymous requests refused); organization-scoped checks wait for the cutover | — | — | certify, this report | cutover | BLOCKED_OWNER_ACTION |

