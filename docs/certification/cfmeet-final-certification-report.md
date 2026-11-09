# CF Meet — final certification report (Document D)

Consolidation closure, 2026-11-09. Every journey J-01 … J-22 was re-executed against the consolidated
implementation (the recovered `blueprint/final-org-events` work plus the closure fixes). "Real
processes" means the certification harness (`certification/`) started each service as its own
binary, with its own PostgreSQL database, real Redis, real signed service authentication and the
production runtime database roles; media journeys use a real LiveKit server with real WebRTC clients.

Verdict vocabulary: **PASS** (minimum proof exercised, no fake of an owner service on the critical
path); **PASS (engineering) + gate** (everything implementable proven; a named owner / external
input remains for production); **EXTERNAL_BLOCKED** (needs a provider; the backend's truthful
reporting is proven). No journey is NOT_COVERED, PARTIAL or FAILED.

## CI evidence

| Run | Workflow | Commit | Result |
|---|---|---|---|
| 42579359634 | Part VI cross-service certification (journeys, SDK parity, unit) | `84b981b1` | success |
| 42579362625 | Meeting service | `84b981b1` | success |
| 42579365503 | Contracts and service auth | `84b981b1` | success |
| 42579368612 | Calling RTC certification (real LiveKit) | `84b981b1` | success |
| 42579371582 | Backup and restore drill (+ real-schema drill) | `84b981b1` | success |
| 42579374204 | Communications Hub foundation (+ real-services Section 14 journey) | `84b981b1` | success |
| 42579921418 | RTC voicemail and recording Egress capture | `a9e6660d` | success |
| 42575208522 / 42575184393 / 42575253098 / 42575202827 / 42575187277 / 42575162292 / 42575200085 / 42575178037 / 42575226387 / 42575165228 | Messaging / Calling / Unified Messaging / Enterprise calling / Communications E2E / Organizations / Developer Platform / API Gateway / Platform Access / Frontend | `a48b0413` | success |
| 42575247380 (attempt 2) | Telephony SIP certification | `a48b0413` | success (attempt 1 failed once on an inbound-DTMF timing wait; 20+ consecutive greens before; no Telephony code changed) |

The full matrix was dispatched again on the final pre-merge commit and runs on `staging` after the
merge; those run ids are recorded in the branch consolidation report §5 and the closure register.

## Journeys

| Journey | Scope | Verdict | Test file :: test case | CI run (commit) | Remaining gate |
|---|---|---|---|---|---|
| J-01 | Accounts, recovery, revocation | PASS | `services/meeting-service/tests/account_security.rs` :: `password_reset_with_a_valid_code_ends_every_earlier_session`, `refresh_rotates_and_reuse_ends_the_whole_session`, `credential_changes_revoke_every_token_on_every_ingress`, `google_identity_resolution_closes_the_pending_account_takeover`, `sign_out_everywhere_and_per_session_revocation`; `certification/tests/journeys.rs` :: `external_developer_sandbox_journey` (a session revoked at Meeting is refused by the Developer Platform console in another process) | 42579362625, 42579359634 (`84b981b1`) | browser Google flow is frontend |
| J-02 | Contacts, QR, privacy | PASS | `certification/tests/personal_communications.rs` :: `personal_identity_contacts_and_authoritative_policy_across_real_services`; `services/communications/tests/foundation.rs` :: `qr_route_mints_a_one_time_connect_payload_disclosed_once` | 42579374204 (`84b981b1`) | — |
| J-03 | Verified-phone provider gate | EXTERNAL_BLOCKED | `personal_communications.rs` (phone status `PROVIDER_BLOCKED`, one canonical label); `profile_notifications.rs` :: `s9_profile_reads_updates_clears_and_validates` | 42579374204, 42579362625 | SMS / verification provider |
| J-04 | Organization creation, provisioning | PASS | `certification/tests/organization_lifecycle.rs` :: `organization_lifecycle_across_real_services` (step J-04: one UUID, scope READY through real Platform Access); `organizations_provisioning.rs` :: `organizations_provision_first_party_scopes_through_the_real_platform_access` | 42579359634 (`84b981b1`) | production cutover (owner) |
| J-05 | Invitations, membership | PASS | `organization_lifecycle.rs` (step J-05: email-bound invitation accepted, workspace listed) | 42579359634 | — |
| J-06 | Multi-organization isolation | PASS | `organization_lifecycle.rs` (step J-06: two organizations, overlapping user, independent roles, no conversation crosses); `organizations/tests/authorization.rs`, `teams.rs`, `policies.rs` | 42579359634, 42575162292 | — |
| J-07 | Organization meetings, calendar | PASS | `organization_lifecycle.rs` (step J-07: member agenda + ICS, **outsider refused**); `meeting-service/tests/organization_domain_access.rs` :: `org04_organization_only_space_is_not_disclosed_to_outsiders`; `organizations_authority.rs` :: `org011_organization_managers_oversee_organization_meeting_spaces`; `meetings_calendar.rs` :: `cal01`…`cal16` | 42579359634, 42579362625 | — |
| J-08 | Guest access, consent | PASS | `meeting-service/tests/collab_api.rs` :: `j08_invited_guest_prejoin_consent_wait_admit_journey` (resolve → pre-join → refused without invitation / consent → consent recorded → waiting without media credential → admitted → credential); `profiles_h.rs` healthcare cases; `livekit_certification.rs` waiting guest gets no media | 42579362625 | healthcare clinical adapter is external (double in tests) |
| J-09 | Personal messaging | PASS | `personal_communications.rs` :: `native_contacts_exchange_opaque_mls_messages_and_block_alternate_devices`; Unified Messaging journeys | 42579374204, 42575253098 | PRIVATE_E2EE production approval (J-21) |
| J-10 | Organization messaging, offboarding | PASS | `organization_lifecycle.rs` (step J-10a: organization conversation in the organization scope, realtime subscribed; J-10b: removal → relay refused, `PRINCIPAL_INACTIVE`, realtime closed, the member's other scope still sends) | 42579359634 | per-organization Messaging binding is an owner release step |
| J-11 | Personal calling | PASS | `services/calling/tests/rtc_certification.rs` :: `real_livekit_certification` / `voice_call` (ring → answer → real media → hold → resume → end → both histories: marker `J-11 … PASS`); Hub launch in `personal_communications.rs` | 42579368612, 42579374204 | background ringing needs push (J-22) |
| J-12 | Enterprise calling | PASS | `organization_lifecycle.rs` (step J-12: Enterprise Calling per organization scope through real Calling, cross-organization admin refused, one principal per scope); `rtc_certification.rs` enterprise extension / ring group / queue / transfer with media; `enterprise_sip_certification.rs`; Egress capture (recording with consent, voicemail) | 42579359634, 42579368612, 42575202827, 42579921418 | per-organization production credentials (owner); PSTN external |
| J-13 | Device / session security | PASS | `personal_communications.rs` :: `hub_facades_device_sessions_and_revocation_fan_out_across_real_owners`; `account_security.rs`; `journeys.rs` (DP console) | 42579374204, 42579362625, 42579359634 | physical devices (owner) |
| J-14 | Organization suspension / recovery | PASS | `organization_lifecycle.rs` (step J-14: suspended → relay refused `AUTHORITY_SUSPENDED` → reactivated) | 42579359634 | — |
| J-15 | Developer API journey | PASS | `journeys.rs` :: `external_developer_sandbox_journey`, `production_approval_journey`; `chapters_10_13.rs` :: `external_onboarding_to_production_journey` | 42579359634 | live gateway deployment (owner) |
| J-16 | Gateway isolation | PASS | `journeys.rs` :: `tenant_isolation_journey`, `first_party_product_journey`, `credential_revocation_journey`, `quota_enforcement_journey`, `websocket_gateway_journey` | 42579359634 | — |
| J-17 | Organization migration rehearsal | PASS | `organization_lifecycle.rs` :: `organizations_cutover_shadow_and_rollback_rehearsal` (export / import / reconciliation checksum, shadow 0 mismatches, authoritative with ids preserved, rollback) | 42579359634 | production cutover (owner) |
| J-18 | Backup and restore | PASS (engineering) + gate | `infra/deployment/backup/tests/real-schema-drill.sh` (all 11 catalog databases migrated with their **real** migration binaries → read-only role → age-encrypted backup → off-host copy → isolated restore → `RESTORE_DRILL=PASS`, schema fingerprint identical: `REAL_SCHEMA_DRILL=PASS services=11`); `ci-drill.sh` refusals and targets | 42579371582 | off-host target and age key custody (owner) |
| J-19 | Durable events, outage recovery | PASS | `organization_lifecycle.rs` (J-19a consumer outage: pending → drained after restart, applied once; J-19b Platform Access outage: recorded, READY after recovery; delivered=18 dead=0); `webhooks.rs` replay / dead letter; `messaging/tests/cross_service_e2e.rs`; Hub outbox replay | 42579359634, 42575253098, 42579374204 | — |
| J-20 | API / SDK / Swagger parity | PASS | `openapi_contract.rs`; contracts validate; Swagger labels (796 operations); bundles; route catalogue; SDK freshness; TypeScript + Dart real-stack parity; frontend endpoint reference check | 42579365503, 42579359634 | — |
| J-21 | E2EE safety gate | PASS | `messaging/src/config/mod.rs` :: `e2ee_device_operations_need_an_explicit_decision_in_staging_and_never_run_in_production`; `messaging/tests/devices.rs` :: `e2ee_device_operations_can_be_disabled_by_deployment` | 42575208522 | independent cryptographic approval |
| J-22 | Telecom and notification gates | EXTERNAL_BLOCKED | `messaging/tests/communications_owner.rs` :: `push_status_reports_not_configured_and_never_echoes_the_handle`; `sms-gateway/tests/outbound.rs`; `telephony/tests/real_interconnect_certification.rs` (skips truthfully without a carrier) | 42575208522, 42575247380 | APNs / FCM / Web Push, SMS provider, lawful PSTN carrier |

Counts: **PASS 19** (J-01, J-02, J-04 … J-17, J-19, J-20, J-21), **PASS (engineering) + gate 1**
(J-18), **EXTERNAL_BLOCKED 2** (J-03, J-22). NOT_COVERED 0, PARTIAL 0, FAILED 0.

## Defects found and fixed by this certification run

| Id | Defect | Root cause | Fix |
|---|---|---|---|
| CLO-001 | An outsider read an `ORGANIZATION_ONLY` space summary (HTTP 200) | identifier lookups returned a SUMMARY to any caller | `a48b0413` (`lookup_view_of`) + regression `org04` |
| CLO-002 | J-12 step: Platform Access 409 on the Calling service account | harness reused the `backend` name in the same environment | `84b981b1` |
| CLO-003 | Organization managers could not perform versioned writes on organization spaces | no FULL view for them | `89efdc91` |

## Not certified here (explicit)

Physical-device acceptance, browser / mobile UI journeys (Section 17 frontend), live provider
integrations (push, SMS, PSTN, real numbers), production deployments and the production
Organizations cutover. None is reported as live.
