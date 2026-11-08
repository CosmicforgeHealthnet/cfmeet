# Section 18: journey certification matrix (J-01 to J-22)

Baseline: `staging` = `8d7b99a3`. Its code is the same as `6105347d`: `git diff 6105347d 8d7b99a3` changes only `docs/implementation/*`. Runs on `6105347d` therefore count as evidence for this tree. Every test name below was checked with grep in the source. Each one was also found as `test <name> ... ok` in the log of the run cited (job name given). No cited test printed `SKIPPED` in that run, except where this document says so.

Verdict rules used:
- **PASS**: the blueprint's minimum backend proof runs in CI on real owner processes or a real owner DB, with no fake standing in for another service.
- **PARTIAL**: some of the minimum proof is missing, or part of it rests only on a fake or simulator of another service.
- **FAIL**: the behaviour is proven wrong.
- **EXTERNAL_BLOCKED**: a provider gate, where a test proves the backend reports the gate truthfully.
- **NOT_COVERED**: no test, or the feature is not built.

Browser, mobile and physical-device parts belong to Section 17 and are listed as remainder, never counted as PASS. "Planned" marks rows the lead is adding to `certification/tests/organization_lifecycle.rs`. Those verdicts reflect current evidence only.

## Runs referenced

| Workflow (file) | Job(s) used | Run | SHA | Result |
|---|---|---|---|---|
| Part VI cross-service certification (`certification.yml`) | `journeys (real processes, real service auth, Postgres, Redis)`; `SDK parity (TypeScript + Dart against the real stack)` | 42467223759 | 8d7b99a3 | success (27 journeys ok, 0 SKIPPED; SDK parity TS 18/18 and Dart 18/18, 0 skipped) |
| Communications Hub foundation (`communications-hub.yml`) | `hub`; `real-services` (runs `personal_communications` with `--features personal-communications-e2e`) | 42448888276 | 6105347d | success (path-filtered; not triggered on 8d7b99a3) |
| Communications end-to-end journeys (`communications-e2e.yml`) | `SMS + voice journeys (real processes, SIP edge, LiveKit)` | 42467223929 | 8d7b99a3 | success |
| Unified Messaging integration journeys (`unified-messaging-e2e.yml`) | `Meeting ↔ Messaging ↔ Platform Access (real processes)`; `Web Messaging client in Chromium (real services)`; `Privacy Relay / SMS Gateway / Calling ↔ Messaging (real processes)` | 42467223754 | 8d7b99a3 | success |
| Enterprise calling certification (`enterprise-calling-certification.yml`) | `Enterprise SIP (Calling + Telephony + Asterisk + SIPp)`; `Enterprise races and burst invariants (5 repetitions)` | 42467223863 | 8d7b99a3 | **failure**: SIP job only (see J-12). Same workflow passed on c3056355 (42448983409) and 6105347d (42448888316) |
| Calling RTC certification (`calling-rtc-certification.yml`) | `real LiveKit certification` | 42467223733 | 8d7b99a3 | success; **RTC Egress capture SKIPPED** (`CALLING_RTC_CERT_EGRESS_*` not configured) |
| Meeting service (`backend.yml`) | `test / v2 operation coverage`; `build / migration smoke / OpenAPI` | 42467223708 | 8d7b99a3 | success |
| Organizations service (`organizations.yml`) | `test (PostgreSQL)` | 42467223643 | 8d7b99a3 | success |
| Calling service (`calling.yml`) | `test (PostgreSQL)` | 42467223773 | 8d7b99a3 | success |
| Messaging (`messaging.yml`) | `service test (PostgreSQL, Redis, MinIO, ClamAV, real Platform Access)` | 42467223790 | 8d7b99a3 | success |
| Contracts and service auth (`contracts.yml`) | `contracts validate`; `Swagger UI renders every bundle (Chromium, schema host blocked)` | 42467223730 | 8d7b99a3 | success |
| Service boundaries (`service-boundaries.yml`) | `service boundary guard` | 42467223771 | 8d7b99a3 | success |
| API Gateway service (`api-gateway.yml`) | `integration (real Platform Access + Developer Platform)` | 42467223725 | 8d7b99a3 | success |
| Telephony (`telephony.yml`) | `test (PostgreSQL)` | 42467223746 | 8d7b99a3 | success |
| SMS Gateway (`sms-gateway.yml`) | tests | 42467223709 | 8d7b99a3 | success |
| Backup and restore drill (`backup-restore-drill.yml`) | `backup → encrypt → off-host copy → decrypt → restore → verify` | 42448888278 | 6105347d | success (path-filtered) |
| Phase 2 backend certification (`phase2-backend-certification.yml`) | `Real LiveKit + WebRTC + Egress + S3 certification`; full suite | 42448888408 | 6105347d | success (path-filtered) |

## Summary

| J | Title | Verdict | Primary tests | Workflow / run | Planned |
|---|---|---|---|---|---|
| J-01 | Personal account: register, verify, Google, refresh, recovery, revocation | PARTIAL | `meeting-service/tests/account_security.rs::*`, `certification/tests/personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services` | Meeting 42467223708; Hub 42448888276 | — |
| J-02 | Contacts via CF Meet ID / QR, consent, block, no phone leak | PARTIAL | `personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services` | Hub 42448888276 | — |
| J-03 | Verified phone | EXTERNAL_BLOCKED | `meeting-service/tests/profile_notifications.rs::s9_profile_reads_updates_clears_and_validates` | Meeting 42467223708; Hub 42448888276 | — |
| J-04 | Create organization + owner, one UUID, createdAt/By, truthful provisioning | PARTIAL | `organizations/tests/journey.rs::organization_lifecycle_journey`, `certification/tests/organizations_provisioning.rs::organizations_provision_first_party_scopes_through_the_real_platform_access` | Orgs 42467223643; Part VI 42467223759 | yes |
| J-05 | Invite + accept, email-bound, workspace list | PARTIAL | `journey.rs::organization_lifecycle_journey`, `organizations_authority.rs::s8_authoritative_workspaces_role_status_and_isolation_matrix` (fake Orgs) | Orgs 42467223643; Meeting 42467223708 | yes |
| J-06 | Two organizations, overlapping users, isolation, roles, teams | PARTIAL | `organizations/tests/authorization.rs::a_member_of_one_organization_cannot_see_or_change_another`, `policies.rs::the_role_resource_matrix_holds_for_every_status_and_organization`, `teams.rs::*` | Orgs 42467223643 | yes |
| J-07 | Organization meeting space + calendar | PARTIAL | `meeting-service/tests/meetings_calendar.rs::cal01..cal16`, `recurrence_exceptions.rs::*`, `profile_notifications.rs::s11_*` | Meeting 42467223708 | — |
| J-08 | Guest healthcare consent / pre-join | PARTIAL | `collab_api.rs::prejoin_consent_and_display_name`, `profiles_h.rs::h_e2e_15_healthcare_waiting_room_default` | Meeting 42467223708 | — |
| J-09 | Personal direct/group messaging, replay after offline | PASS (backend) | `personal_communications.rs::native_contacts_exchange_opaque_mls_messages_and_block_alternate_devices` | Hub 42448888276 | — |
| J-10 | Organization messaging + offboarding realtime revocation | NOT_COVERED | — | — | — |
| J-11 | Personal voice/video: ring, answer, media, hold, end, history | PARTIAL | `calling/tests/rtc_certification.rs::real_livekit_certification`, `app_control.rs::hold_and_resume_are_enforced_at_the_provider`, `call_history.rs::*` | RTC 42467223733; Calling 42467223773 | — |
| J-12 | Organization enterprise calling, two scopes, actor audit, recording consent | PARTIAL | `calling/tests/organization_forwarding.rs::*`, `organizations_authority.rs::s12_organization_calling_*` (fake Orgs), `enterprise_recording.rs::consent_is_each_participants_own_and_bound_to_the_call` | Calling 42467223773; Meeting 42467223708; Enterprise 42467223863 (**failure, intermittent**) | — |
| J-13 | Device / session compromise | PASS (backend) | `personal_communications.rs::hub_facades_device_sessions_and_revocation_fan_out_across_real_owners`, `account_security.rs::*`, `session_bridge.rs::*` | Hub 42448888276; Meeting 42467223708 | — |
| J-14 | Organization suspension / recovery across Meetings, Calling, Messaging, Hub | PARTIAL | `organizations_provisioning.rs::organizations_provision_first_party_scopes_through_the_real_platform_access`, `organizations_authority.rs::s12_*` (fake Orgs) | Part VI 42467223759; Meeting 42467223708 | yes |
| J-15 | Developer API customer journey | PASS | `certification/tests/journeys.rs::external_developer_sandbox_journey`, `chapters_10_13.rs::external_onboarding_to_production_journey`, `journeys.rs::production_approval_journey` | Part VI 42467223759 | — |
| J-16 | Gateway negative scope / tenant tests | PASS | `journeys.rs::tenant_isolation_journey`, `journeys.rs::first_party_product_journey`, `journeys.rs::credential_revocation_journey` | Part VI 42467223759 | — |
| J-17 | Migration shadow / cutover / rollback | PARTIAL | `organizations/tests/import.rs::*`, `organizations_authority.rs::s4_shadow_mode_answers_from_legacy_and_counts_mismatches` (fake Orgs), `certification/tests/usage_migration.rs::usage_migration_rehearsal_journey` | Orgs 42467223643; Part VI 42467223759 | yes |
| J-18 | Encrypted backup + isolated restore of every service DB | PARTIAL | `infra/deployment/backup/tests/ci-drill.sh` | Backup drill 42448888278 | — |
| J-19 | Duplicate events, outage recovery, idempotent sagas, outbox replay, dead letter | PARTIAL | `webhooks.rs::webhook_failure_and_replay_journey`, `messaging/tests/cross_service_e2e.rs::*`, `communications/tests/foundation.rs::durable_outbox_leases_deliver_concurrently_and_dead_events_replay_by_scoped_operator` | Part VI; Unified 42467223754; Hub 42448888276 | possibly |
| J-20 | Client contract + Swagger parity | PASS (backend) | `meeting-service/tests/openapi_contract.rs::*`, contracts validate, Swagger render, SDK parity | Contracts 42467223730; Part VI 42467223759 | — |
| J-21 | E2EE gate fails closed | PASS | `messaging/src/config/mod.rs::tests::e2ee_device_operations_need_an_explicit_decision_in_staging_and_never_run_in_production`, `messaging/tests/devices.rs::e2ee_device_operations_can_be_disabled_by_deployment` | Messaging 42467223790 | — |
| J-22 | Push / phone / PSTN reported truthfully | EXTERNAL_BLOCKED | `messaging/tests/communications_owner.rs::push_status_reports_not_configured_and_never_echoes_the_handle`, `sms-gateway/tests/outbound.rs::sending_needs_sms_send_and_a_configured_provider`, `telephony/tests/real_interconnect_certification.rs::real_lawful_interconnect_certification` | Messaging 42467223790; SMS 42467223709; Telephony 42467223746; Hub 42448888276 | — |

Paths in the table are relative to `services/` unless they start with `certification/` or `infra/`.

---

## J-01: Personal email + Google registration, verification, profile, refresh, recovery, revocation

- **Tests**
  - `services/meeting-service/tests/account_security.rs`:
    - `::login_opens_a_session_with_typed_tokens_and_case_insensitive_email`
    - `::refresh_rotates_and_reuse_ends_the_whole_session`
    - `::logout_ends_only_the_current_session`
    - `::credential_changes_revoke_every_token_on_every_ingress`
    - `::sign_out_everywhere_and_per_session_revocation`
    - `::recovery_and_login_answers_do_not_reveal_accounts`
    - `::registration_normalizes_email_and_refuses_case_variants`
    - `::google_identity_resolution_closes_the_pending_account_takeover`
  - `services/meeting-service/tests/legacy_auth_keys_webhooks.rs::google_exchange_code_signs_in_once` and `::verify_reset_token_reports_a_pending_reset_without_consuming_it`
  - `services/meeting-service/tests/session_bridge.rs::the_developer_platform_verifies_sessions_through_the_session_authority` and `::the_bridge_enforces_token_type_and_the_revocation_epoch`
  - `services/meeting-service/tests/profile_notifications.rs::s9_profile_reads_updates_clears_and_validates`
  - `certification/tests/personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services`, which covers real register, login, refresh, logout and durable logout
  - Every Part VI journey also registers, verifies the email (OTP read from Redis) and logs in through the real Meeting Service (`certification/src/provision.rs`).
- **Workflow & run**: Meeting 42467223708 (`test / v2 operation coverage`); Hub 42448888276 (`real-services`); Part VI 42467223759.
- **Proves**
  - Typed tokens, refresh rotation, and refresh reuse ending the whole session.
  - A password change revokes every token on both the v1 and v2 ingresses.
  - Sign-out everywhere works.
  - Recovery and login answers do not reveal whether an account exists.
  - Google sign-in takes over an unverified pending account and ends the attacker's sessions (pre-hijack closed). No real Google is involved.
  - The Developer Platform bridge rejects revoked tokens and tokens from an old revocation epoch, and fails closed when the store is unreachable.
  - Real processes do register → verify → login → refresh → logout.
- **Blueprint minimum**: the whole chain, including a completed recovery that ends earlier sessions, and revocation seen on v1, v2 and the DP bridge.
- **Gap**
  - No test completes `POST /api/v1/auth/reset-password` with a valid token. Only invalid-token and enumeration-safety cases are tested.
  - DP bridge revocation is proven only on the Meeting side. The real Developer Platform calls the bridge in every Part VI journey, but no real-process journey revokes a session and then sees the Developer Platform refuse it.
  - Google is exercised only through the resolver and exchange paths (the provider is external).
  - Remainder for Section 17: web adoption of the OAuth exchange.
- **Verdict: PARTIAL**: the positive password-recovery path and a cross-process DP revocation check are missing.

## J-02: Contacts via CF Meet ID / QR, consent, block, no phone leak

- **Tests**
  - `certification/tests/personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services`. Its markers: `invitations, accepted_connections, separate_aliases, common_groups, blocking, rotation, invitation_invalidation, identity_without_phone`.
  - `services/communications/tests/foundation.rs`:
    - `::invitation_digest_purpose_expiry_revocation_usage_and_owner_are_enforced`
    - `::blocking_and_domain_blocks_are_immediate_and_unblocking_never_reconnects`
    - `::public_id_rotation_invalidates_old_identity_and_invitations_without_remapping`
    - `::concurrent_lazy_activation_keeps_one_stable_account_and_principal_without_phone`
- **Workflow & run**: Hub 42448888276 (`real-services`, `hub`).
- **Proves** (real Account, PA, Hub, Calling and Messaging processes)
  - Invite resolution returns the CF Meet ID with `relationship: NONE`, and no `principalId` or `email`.
  - Contacts, aliases and common groups work.
  - A block makes Messaging and Calling refuse delivery (`DELIVERY_REFUSED`), and unblocking never reconnects.
  - CF Meet ID rotation invalidates the old ID and old invites.
  - No phone is ever required (`phoneVerification.required=false`).
- **Blueprint minimum**: consent via CF Meet ID and QR, block, no phone leak.
- **Gap**: the QR payload route `GET /api/v2/communications/me/qr` (`services/communications/src/api.rs`, `qr.create`) is not called by any test in the repository. Rendering and scanning the QR in the client is Section 17.
- **Verdict: PARTIAL**: everything except the QR route is proven in real processes.

## J-03: Verified phone

- **Tests**
  - `services/meeting-service/tests/profile_notifications.rs::s9_profile_reads_updates_clears_and_validates`: asserts `phoneVerification.status == "PROVIDER_BLOCKED"` and `required == false`.
  - `certification/tests/personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services`: the Hub asserts `phoneVerification.status == "DEFERRED"`.
- **Workflow & run**: Meeting 42467223708; Hub 42448888276.
- **Proves**: phone verification is never required and is reported as unavailable. No SMS provider is configured.
- **Blueprint minimum**: phone verification correctly labelled `PROVIDER_BLOCKED` while no provider exists.
- **Gap**
  - The labels are inconsistent. The Account profile says `PROVIDER_BLOCKED`. The Hub (`services/communications/src/policy.rs`, `profiles.rs`, `internal.rs`) says `DEFERRED`, and `sync.rs` says `phoneRecovery: DEFERRED`. Both are truthful (not available, not required), but only the Account matches the expected label.
  - Owner: backend Section 18, or a decision on which label is canonical.
- **Verdict: EXTERNAL_BLOCKED**: SMS/verification provider. The Hub label should be aligned.

## J-04: Create organization + owner, one UUID, createdAt/createdBy, truthful provisioning (Planned)

- **Tests**
  - `services/organizations/tests/journey.rs::organization_lifecycle_journey`: asserts `createdBy`, `provisioningState: PENDING`, and the member's `createdBy`.
  - `journey.rs::self_service_creation_is_limited_per_actor_and_idempotent`
  - `services/organizations/tests/provisioning.rs::provisioning_is_off_by_default_and_reported_truthfully`
  - `services/organizations/tests/provisioning.rs::two_organizations_get_distinct_triples_ready_only_after_server_side_verification`
  - `certification/tests/organizations_provisioning.rs::organizations_provision_first_party_scopes_through_the_real_platform_access`
  - `services/meeting-service/tests/organizations_authority.rs::s4_every_first_party_route_relays_in_authoritative_mode` (fake Organizations)
- **Workflow & run**: Orgs 42467223643 (`test (PostgreSQL)`); Part VI 42467223759 (`journeys`); Meeting 42467223708.
- **Proves**
  - The Organizations router, with real service tokens and its own DB, creates the organization and its owner with `createdBy`.
  - Provisioning stays PENDING while disabled and is reported truthfully.
  - With real Organizations and real Platform Access processes, provisioning reaches READY only after server-side verification. PA holds a FIRST_PARTY tenant for the organization reference.
- **Blueprint minimum**: a user creates the organization through the Meeting first-party relay, and one organization UUID is consistent across Meeting, Organizations and PA.
- **Gap**: the Meeting-relay leg is tested only against a fake Organizations. No real-process journey goes Meeting → Organizations → PA with the same UUID.
- **Verdict: PARTIAL**. New journey pending in `organization_lifecycle.rs`.

## J-05: Invite + accept, email-bound, workspace list (Planned)

- **Tests**
  - `services/organizations/tests/journey.rs::organization_lifecycle_journey`: covers wrong or missing email refused, expired, and single use.
  - `journey.rs::invitation_replay_reissues_the_token_and_reinvite_replaces_the_pending_one`
  - `services/organizations/tests/foundation.rs::partial_uniques_hold_one_current_membership_and_one_pending_invitation`
  - `services/meeting-service/tests/organizations_authority.rs::s4_member_facades_relay_existing_members_and_require_invitations`
  - `organizations_authority.rs::s8_authoritative_workspaces_role_status_and_isolation_matrix`
  - `organizations_authority.rs::s11_organization_invitation_email_from_the_authoritative_answer` (all three Meeting tests use a fake Organizations)
- **Workflow & run**: Orgs 42467223643; Meeting 42467223708.
- **Proves**: email-bound, single-use, hashed invitations in the Organizations owner. The Meeting workspace list and invitation email are built from Organizations-shaped answers.
- **Blueprint minimum**: invite → accept by the invited account → workspace list shows the organization, across real Meeting and Organizations.
- **Gap**: no real-process composition. Invite and accept UI is Section 17.
- **Verdict: PARTIAL**. New journey pending in `organization_lifecycle.rs`.

## J-06: Two organizations with overlapping users, cross-org denial, independent roles, team boundaries (Planned)

- **Tests**
  - `services/organizations/tests/authorization.rs::a_member_of_one_organization_cannot_see_or_change_another`
  - `authorization.rs::resolve_and_evaluate_truth_tables`
  - `services/organizations/tests/policies.rs::the_role_resource_matrix_holds_for_every_status_and_organization`
  - `policies.rs::policies_restrict_capabilities_everywhere_they_are_evaluated`
  - `services/organizations/tests/teams.rs::owners_and_admins_manage_teams_and_managers_only_their_own`
  - `teams.rs::team_roles_never_self_escalate_and_follow_the_organization_role`
  - `services/meeting-service/tests/organizations_authority.rs::s8_authoritative_workspaces_role_status_and_isolation_matrix` (fake Organizations)
  - `services/meeting-service/tests/meetings_calendar.rs::cal03_organization_filter_member_vs_outsider`
- **Workflow & run**: Orgs 42467223643; Meeting 42467223708.
- **Proves**: the role × status × two-organization matrix, cross-org read and write denial, and team isolation inside the Organizations owner.
- **Blueprint minimum**: the same user holds different roles in two organizations, and is denied cross-org on every consumer (Meetings, Calling, workspaces) in real processes.
- **Gap**: consumer enforcement is proven only against a fake Organizations.
- **Verdict: PARTIAL**. New journey pending in `organization_lifecycle.rs`.

## J-07: Organization meeting space + calendar (time, time zone, recurrence, invite, reminder, reschedule, history)

- **Tests**
  - `services/meeting-service/tests/meetings_calendar.rs`:
    - `::cal01_agenda_across_spaces_and_relations`
    - `::cal05_recurrence_beyond_the_horizon_has_stable_identity`
    - `::cal06_series_edit_keeps_exceptions_and_end_retires_rescheduled`
    - `::cal07_temporal_matrix_across_zones`
    - `::cal08_cancellation_after_invite_reaches_agenda_and_ics`
    - `::cal09_concurrent_reschedule_and_cancel`
    - `::cal10_reschedule_keeps_end_and_rejects_past_and_stale_versions`
    - `::cal13_ics_exports_are_valid_private_and_authorized`
    - `::cal15_idempotent_space_and_conference_creation`
  - `services/meeting-service/tests/recurrence_exceptions.rs::rx01_far_future_exceptions_applied_by_scheduler_across_dst`
  - `services/meeting-service/tests/profile_notifications.rs::s11_reminders_once_per_start_respect_preferences_and_retry_on_unknown_preferences` and `::s11_reschedule_and_cancel_notices_are_durable_once_and_exclude_the_actor`
  - `services/meeting-service/tests/organizations_authority.rs::s12_organization_meeting_creation_follows_the_capabilities_read` (fake Organizations)
- **Workflow & run**: Meeting 42467223708.
- **Proves**: calendar, time-zone, DST, recurrence, reschedule, ICS and reminder rules in the owner service against real Postgres. Organization-policy gating of space creation is tested against a fake Organizations.
- **Blueprint minimum**: an organization space created under real organization authority, then the full calendar chain.
- **Gap**
  - The organization authority leg runs only against a fake.
  - Email delivery is off by default (`NOTIFICATIONS_EMAIL_ENABLED=false`).
  - Web calendar display fixes (MTG-004, FE-001) are Section 17.
- **Verdict: PARTIAL**: calendar rules are owner-proven; the organization leg rests on a fake.

## J-08: Guest healthcare consent / pre-join

- **Tests**
  - `services/meeting-service/tests/collab_api.rs::prejoin_contract` and `::prejoin_consent_and_display_name`
  - `services/meeting-service/tests/profiles_h.rs`:
    - `::h_s01_public_uncontrolled_join_rejected_for_healthcare`
    - `::h_s12_healthcare_guest_token_scoped_to_context`
    - `::h_e2e_15_healthcare_waiting_room_default`
    - `::h_f05_healthcare_adapter_unavailable_fails_closed`
  - `services/meeting-service/tests/recording_g.rs::g_e2e_06_missing_consent_blocks_provider_start`
  - `services/meeting-service/tests/livekit_certification.rs::real_livekit_m_waiting_subscription_ban` (real LiveKit)
- **Workflow & run**: Meeting 42467223708; Phase 2 42448888408 (`Real LiveKit + WebRTC + Egress + S3 certification`).
- **Proves**
  - Pre-join refuses with `CONSENT_REQUIRED` until consent is given.
  - Healthcare forces a waiting room, scopes the guest token, and fails closed when the clinical adapter is down.
  - A waiting guest gets no media subscription on real LiveKit.
- **Blueprint minimum**: a guest is invited, sees pre-join, consents, waits, and is admitted.
- **Gap**
  - The clinical/healthcare adapter is a test double, because the real integration is external.
  - The guest pre-join UI in browser and mobile is Section 17.
  - There is no single real-process guest journey.
- **Verdict: PARTIAL**.

## J-09: Personal direct and group messaging, including replay after offline

- **Tests**
  - `certification/tests/personal_communications.rs::native_contacts_exchange_opaque_mls_messages_and_block_alternate_devices`. Evidence line: `encrypted-storage=PASS client-decryption=PASS replay=PASS block=PASS revocation=PASS`.
  - `services/meeting-service/tests/unified_messaging_e2e.rs::meeting_chat_migrates_to_unified_messaging_across_real_services`
  - `services/messaging/tests/outbox.rs::concurrent_publishers_deliver_each_event_once`
- **Workflow & run**: Hub 42448888276 (`real-services`); Unified 42467223754.
- **Proves**
  - Real MLS clients exchange opaque messages through five real processes (Account, PA, Hub, Calling, Messaging).
  - An offline member catches up on history in order (line 378). Group resolution works (line 459).
  - Blocks and revoked alternate devices are refused.
- **Blueprint minimum**: direct and group messaging, with replay after offline.
- **Gap**
  - The journey runs in the TEST runtime only. Production activation of PRIVATE_E2EE devices is refused by design until external cryptographic certification (J-21).
  - The workflow is path-filtered and was last run on 6105347d (code-identical).
  - Web and mobile clients are Section 17.
- **Verdict: PASS (backend)**. Production activation depends on J-21.

## J-10: Organization messaging + offboarding realtime revocation

- **Tests**: none.
- **Proves**: nothing for organizations.
  - Messaging has no organization scope; the register lists "Messaging per-organization scope" and "organization conversations" as open.
  - The Organizations runtime wires `outbox::DisabledDispatcher` (`services/organizations/src/main.rs:56`), so a membership removal is never sent to Messaging, the Hub or Calling.
- **Blueprint minimum**: an organization conversation exists, a member is removed or offboarded, and their realtime connection and access end at once.
- **Gap**: the feature is not built. It needs an Organizations event delivery target (or a pull) plus a Messaging organization scope.
- **Verdict: NOT_COVERED**. Owner: backend (Section 13/18), with an owner product decision on organization conversations.

## J-11: Personal voice and video: ring, answer, media, hold, end, history

- **Tests**
  - `services/calling/tests/rtc_certification.rs::real_livekit_certification`: real LiveKit with real WebRTC clients. It runs `voice_call` (hold at line 679), `video_call_and_removal`, `group_call` and token refusal cases.
  - `services/calling/tests/app_calls.rs::voice_call_end_to_end_with_verified_media_evidence` and `::cancel_during_ringing_stops_everything_and_late_accept_fails`
  - `services/calling/tests/app_control.rs::hold_and_resume_are_enforced_at_the_provider`
  - `services/calling/tests/app_ringing.rs::revoked_endpoints_stop_ringing_and_lose_signaling`
  - `services/calling/tests/call_history.rs::history_is_paginated_scoped_and_strict` and `::one_answered_device_answers_for_the_logical_participant`
  - `certification/tests/personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services` (markers `voice_call_policy, video_call_policy`) and `::hub_facades_device_sessions_and_revocation_fan_out_across_real_owners` (marker `hub_call_launch`)
- **Workflow & run**: RTC 42467223733; Calling 42467223773; Hub 42448888276.
- **Proves**: real media for voice and video, hold enforced at the provider, and end. History works in the owner DB. A personal call launched from the Hub reaches real Calling, but is then cancelled with no media.
- **Blueprint minimum**: one personal call goes ring → answer → media → hold → end → history.
- **Gap**
  - The steps are proven in separate suites. No personal, Hub-launched call goes through answer, media and history in one real-process journey.
  - Background ringing needs APNs/FCM (external, J-22).
  - Client ring UX and physical devices are Section 17.
- **Verdict: PARTIAL**.

## J-12: Organization enterprise calling: two isolated scopes, actor audit, recording consent

- **Tests**
  - `services/calling/tests/organization_forwarding.rs`:
    - `::an_organization_request_acts_in_the_organization_scope_and_audit_names_the_human`
    - `::organization_forwarding_is_refused_while_the_flag_is_off`
    - `::an_organization_request_needs_the_credential_and_a_native_member`
    - `::only_first_party_organization_tenants_are_reachable`
  - `services/meeting-service/tests/organizations_authority.rs`, all with a fake Organizations:
    - `::s12_organization_calling_uses_the_organization_scope_and_the_acting_member`
    - `::s12_organization_calling_admin_rights_come_from_organizations`
    - `::s12_organization_calling_fails_closed`
    - `::s12_organization_calling_is_disabled_by_default`
  - `services/calling/tests/enterprise_recording.rs::consent_is_each_participants_own_and_bound_to_the_call`
  - `services/calling/tests/rtc_certification.rs::real_livekit_certification`: enterprise extension, ring group, queue and transfer with real media.
  - `services/calling/tests/enterprise_sip_certification.rs::enterprise_sip_ivr_certification`
- **Workflow & run**: Calling 42467223773; Meeting 42467223708; RTC 42467223733; Enterprise 42467223863 = **failure**.
  - On 42467223863, `enterprise_sip_ivr_certification` panicked at `tests/enterprise_sip_certification.rs:1522`: "the Call is CONNECTED (bridged) did not happen".
  - The same workflow passed on c3056355 (42448983409) and 6105347d (42448888316), and 8d7b99a3 is docs-only, so the failure is intermittent.
  - It is **recurring**: the same assertion at line 1522 also failed on 22da1aac (42425119285), and a rerun on that SHA passed (42425123331).
- **Proves**: Calling acts in the organization scope only behind its flag and only for FIRST_PARTY `org-` tenants. The audit records `actorPrincipalId`. Recording consent belongs to each participant.
- **Blueprint minimum**: two organizations with isolated enterprise scopes in real processes, actor audit, and recording consent with captured media.
- **Gap**
  - Two-scope isolation is never run across real Meeting, Organizations, PA and Calling.
  - Both flags are off by default, and the per-organization credential and grant are owner release steps.
  - RTC Egress capture was SKIPPED on 42467223733, so recording media is not captured on the staging CI. `cfmeet-rtc-capture-certification.yml` runs only on `main` (last run 41400582781, 2026-11-02, success).
  - The SIP IVR certification is flaky.
- **Verdict: PARTIAL**. The SIP flake needs a root-cause ticket (backend Section 18).

## J-13: Device / session compromise

- **Tests**
  - `certification/tests/personal_communications.rs::hub_facades_device_sessions_and_revocation_fan_out_across_real_owners`. Markers: `session_ends_devices, device_ends_session, reference_never_relayed, revocation_without_profile_messaging, revocation_without_profile_calling, principal_status_mirror`.
  - `personal_communications.rs::native_contacts_exchange_opaque_mls_messages_and_block_alternate_devices` (device revoke)
  - `services/meeting-service/tests/account_security.rs::refresh_rotates_and_reuse_ends_the_whole_session` and `::sign_out_everywhere_and_per_session_revocation`
  - `services/meeting-service/tests/session_bridge.rs::the_bridge_enforces_token_type_and_the_revocation_epoch`
  - `services/communications/tests/unified.rs::devices_are_bound_to_the_account_session_that_linked_them`
  - `services/messaging/tests/notification_authorization.rs::revoking_all_sessions_disables_push_without_deleting_device_identity`
- **Workflow & run**: Hub 42448888276; Meeting 42467223708; Messaging 42467223790.
- **Proves** (real processes)
  - Ending a sign-in revokes its device at Calling (`ACCOUNT_SESSION_ENDED`).
  - Revoking a device ends its sign-in.
  - A password change reaches Messaging and Calling by principal, even for an account that never opened the Hub.
  - Suspension is mirrored to the PA principal.
  - Refresh-token reuse kills the session.
  - Push is disabled on revoke-all.
- **Blueprint minimum**: a compromised session or device is revoked everywhere it is honoured.
- **Gap**: DP console revocation is proven only on the Meeting side (see J-01). Sessions and devices UI is Section 17.
- **Verdict: PASS (backend)**.

## J-14: Organization suspension / recovery across Meetings, Calling, Messaging and Hub (Planned)

- **Tests**
  - `certification/tests/organizations_provisioning.rs::organizations_provision_first_party_scopes_through_the_real_platform_access`: real Organizations and PA. Suspend → PA tenant suspended → scope resolve refused → other organization untouched → reactivate.
  - `services/organizations/tests/provisioning.rs::suspension_during_provisioning_and_lifecycle_follow_the_tenant_fail_closed`
  - `services/organizations/tests/journey.rs::organization_lifecycle_journey` (suspend, reactivate, archive)
  - `services/meeting-service/tests/organizations_authority.rs::s12_admission_and_admit_follow_the_organization_meetings_feature` (fake Organizations)
  - `services/meeting-service/tests/organization_domain_access.rs::org02_inactive_membership_or_organization_is_denied_on_reconnect`
- **Workflow & run**: Part VI 42467223759; Orgs 42467223643; Meeting 42467223708.
- **Proves**: suspension reaches PA and stops scope resolution in real processes. Meeting refuses with `ORGANIZATION_SUSPENDED` against a fake Organizations, including on reconnect.
- **Blueprint minimum**: suspend the organization; Meetings, Calling, Messaging and the Hub all refuse; recovery restores access.
- **Gap**
  - Calling refusal for a suspended organization tenant is not tested.
  - Messaging and the Hub have no organization path (see J-10), and Organizations events are not delivered (`DisabledDispatcher`).
  - The Meeting leg rests on a fake.
- **Verdict: PARTIAL**. New journey pending in `organization_lifecycle.rs`.

## J-15: Developer API customer journey

- **Tests**
  - `certification/tests/journeys.rs::external_developer_sandbox_journey` and `::production_approval_journey`
  - `certification/tests/chapters_10_13.rs::external_onboarding_to_production_journey` and `::credential_lifecycle_and_caller_quota_journey`
  - `certification/tests/webhooks.rs::external_developer_signed_webhook_journey`
  - `certification/tests/meetings_platform.rs::sandbox_credential_runs_environment_meetings_through_the_gateway`
  - `certification/tests/service_accounts.rs::service_account_delegation_journey`
- **Workflow & run**: Part VI 42467223759 (`journeys`, all ok, not skipped).
- **Proves**: signup → sandbox → credential → API calls through the real gateway → production approval → webhooks. Ten real service processes take part, with Ed25519 service auth.
- **Blueprint minimum**: the same chain.
- **Gap**
  - The developer console UI (ORG-017) is Section 17.
  - Gateway/DP deployment (PLT-005, http vs https) is an owner/external step.
- **Verdict: PASS**.

## J-16: Gateway negative scope / tenant tests

- **Tests**
  - `certification/tests/journeys.rs`:
    - `::tenant_isolation_journey`
    - `::credential_revocation_journey`
    - `::quota_enforcement_journey`
    - `::first_party_product_journey`: route scope refused with 403; every internal or admin route answers 404 through the gateway.
    - `::websocket_gateway_journey`
  - `certification/tests/webhooks.rs::webhook_ssrf_policy_journey`
  - API Gateway `integration (real Platform Access + Developer Platform)`
- **Workflow & run**: Part VI 42467223759; API Gateway 42467223725.
- **Proves**: cross-tenant access is denied, missing scopes are refused, revoked credentials are refused, and internal routes are unreachable.
- **Blueprint minimum**: the same, negative tests only.
- **Gap**: none in the backend.
- **Verdict: PASS**.

## J-17: Migration shadow / cutover / rollback: row checksum, no split brain, IDs preserved (Planned)

- **Tests**
  - `services/organizations/tests/import.rs`:
    - `::the_shared_vector_imports_with_ids_and_timestamps_preserved_and_reimport_is_a_no_op`
    - `::a_differing_row_is_reported_and_never_overwritten`
    - `::invalid_exports_and_orphan_memberships_are_refused_atomically`
    - `::the_cli_imports_the_same_export_idempotently`
  - `services/organizations/tests/provisioning.rs::split_brain_mappings_and_drift_are_refused_and_reported`
  - `services/meeting-service/tests/organizations_authority.rs::s4_shadow_mode_answers_from_legacy_and_counts_mismatches` and `::s4_legacy_mode_is_unchanged_and_first_party_routes_are_disabled` (fake Organizations)
  - Other migrations rehearsed in real processes:
    - `certification/tests/usage_migration.rs::usage_migration_rehearsal_journey`
    - `certification/tests/webhook_migration.rs::webhook_migration_journey`
    - `certification/tests/webhooks.rs::shadow_mode_no_double_delivery_journey`
    - `certification/tests/rollout_rehearsal.rs::rollout_and_independent_rollback_rehearsal`
    - `certification/tests/schema_upgrade.rs::part_vi_migrations_upgrade_a_pre_part_vi_schema_without_data_loss`
    - `services/numbering/tests/relay_cutover_e2e.rs::relay_numbers_move_to_numbering_leases_with_identical_routing_and_a_safe_rollback`
- **Workflow & run**: Orgs 42467223643; Meeting 42467223708; Part VI 42467223759; Comms e2e 42467223929.
- **Proves**
  - The Organizations import keeps IDs and timestamps, uses a shared checksum vector, reports differing rows without overwriting them, and refuses split-brain mappings.
  - Meeting shadow mode counts mismatches (against a fake).
  - Usage, webhook, relay-number and schema migrations are rehearsed with rollback in real processes.
- **Blueprint minimum**: for the Organizations authority move, export → import → shadow with zero mismatches → authoritative → rollback, with no two writers, on real Meeting and Organizations.
- **Gap**
  - No real-process rehearsal of the Organizations cutover.
  - The production cutover needs off-host backups and owner approval (runbook).
- **Verdict: PARTIAL**. New journey pending in `organization_lifecycle.rs`.

## J-18: Encrypted backup + isolated restore of every service database

- **Tests**: `infra/deployment/backup/tests/ci-drill.sh` (with `tests/unit.sh`). Service DR suites:
  - `services/messaging/tests/disaster_recovery.rs::a_restore_keeps_attachments_never_resubmits_sms_and_cannot_recover_e2ee_without_device_state`
  - DR suites for calling, privacy-relay, telephony and sms-gateway (`*/tests/disaster_recovery.rs`)
- **Workflow & run**: Backup drill 42448888278 (6105347d).
- **Proves**
  - All 11 active catalog databases complete age encryption → off-host copy → identical copy check → decrypt → isolated restore with `RESTORE_DRILL=PASS`. The 11 are meeting-service, communications, developer-platform, platform-access, calling, messaging, numbering, privacy-relay, telephony, sms-gateway and organizations.
  - No plaintext dump is left anywhere.
  - The backup role is read-only.
  - `NOT_CONFIGURED` is labelled `LOCAL_ONLY`.
- **Blueprint minimum**: an encrypted off-host backup and an isolated restore of every service DB.
- **Gap**
  - Apart from meeting-service and communications, the drill restores synthetic sample schemas (`CREATE TABLE … (id bigserial)`), not each service's real migrated schema.
  - "Off-host" is a local directory, rsync or rclone target on the runner.
  - The real off-host target and age key custody are an owner decision, so production backups are `LOCAL_ONLY` today.
- **Verdict: PARTIAL**: tooling is proven for all 11; the real target is owner-gated.

## J-19: Duplicate events, outage recovery, idempotent sagas, outbox replay, dead letter (Planned: possibly)

- **Tests**
  - `certification/tests/webhooks.rs::webhook_failure_and_replay_journey`, `::webhook_retry_after_is_honoured` and `::shadow_mode_no_double_delivery_journey`
  - `certification/tests/load_resilience.rs::gateway_load_and_resilience` and `::webhook_usage_load_and_resilience`
  - `services/messaging/tests/cross_service_e2e.rs`:
    - `::postgresql_loss_during_active_messaging_loses_nothing_acknowledged_and_duplicates_nothing`
    - `::sms_gateway_death_mid_send_recovers_without_duplicate_submission`
    - `::scenario_4b_privacy_relay_outage_and_restart_neither_grant_nor_lose_authority`
  - `services/communications/tests/foundation.rs::durable_outbox_leases_deliver_concurrently_and_dead_events_replay_by_scoped_operator` and `::dependency_and_database_outages_never_return_personal_authorization`
  - `services/messaging/tests/outbox.rs::a_crashed_claim_is_reclaimed_after_its_lease_and_failures_dead_letter`
  - `services/organizations/tests/provisioning.rs` (in-memory PA):
    - `::duplicate_requests_and_replays_create_one_workflow_one_scope_and_no_duplicate_events`
    - `::platform_access_unavailable_backs_off_then_fails_and_an_operator_retry_recovers`
    - `::partial_platform_access_success_with_a_local_failure_resumes_with_the_same_triple`
- **Workflow & run**: Part VI 42467223759; Unified 42467223754 (`Privacy Relay / SMS Gateway / Calling ↔ Messaging`); Hub 42448888276; Messaging 42467223790; Orgs 42467223643.
- **Proves**: webhook replay and dead-letter handling, outage recovery without loss or duplication across real Messaging, SMS Gateway and Privacy Relay processes, Hub outbox replay of dead events, and idempotent provisioning (against an in-memory PA).
- **Blueprint minimum**: the same, including organization events.
- **Gap**
  - Organizations events are never delivered (`DisabledDispatcher`), so organization outbox replay and dead-letter handling have no real consumer.
  - Provisioning saga failure modes are proven only against an in-memory PA. The real-PA run covers the happy path and suspension.
- **Verdict: PARTIAL**.

## J-20: Client contract + Swagger parity

- **Tests**
  - `services/meeting-service/tests/openapi_contract.rs`:
    - `::committed_contract_matches_code`
    - `::every_v2_route_is_documented`
    - `::j_e2e_06_route_table_and_openapi_are_identical`
    - `::j_e2e_07_contract_is_resolvable_and_consistent`
  - Contracts `contracts validate` and `Swagger UI renders every bundle (Chromium, schema host blocked)`
  - Service boundaries `service boundary guard` (gateway upstream check, INV-001)
  - Part VI `SDK parity`: TS 18 passed, 0 skipped; Dart "+18 All tests passed"
- **Workflow & run**: Contracts 42467223730; Meeting 42467223708 (`build / migration smoke / OpenAPI`); Boundaries 42467223771; Part VI 42467223759.
- **Proves**: code and contract are identical, bundles render, and both SDKs pass every parity scenario against the real stack.
- **Gap**
  - In the standalone SDK workflows (42467223777, 42467223745) real-stack parity prints `SKIPPED: CFMEET_STACK_ENV is not set`. That is expected because the Part VI job covers it.
  - Web/mobile adoption of the SDKs (FE-015, report-only parity scan) is Section 17.
- **Verdict: PASS (backend)**.

## J-21: E2EE gate fails closed

- **Tests**
  - `services/messaging/src/config/mod.rs::tests::e2ee_device_operations_need_an_explicit_decision_in_staging_and_never_run_in_production`: `true` is refused in production.
  - `services/messaging/tests/devices.rs::e2ee_device_operations_can_be_disabled_by_deployment` (`E2EE_DEVICES_DISABLED`)
  - `services/messaging/tests/realtime.rs::session_devices_are_refused_every_e2ee_privilege`
  - `services/messaging/tests/communications_owner.rs::capabilities_report_managed_messaging_apart_from_e2ee_readiness`
  - `certification/tests/personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services`: Hub `messaging.securityReady=false`, `available=false`.
- **Workflow & run**: Messaging 42467223790; Hub 42448888276.
- **Proves**: production cannot enable E2EE devices, session devices can never gain E2EE privileges, and readiness is reported truthfully.
- **Gap**: external cryptographic certification (E2EE review #13) is needed before activation. That is a release gate, not a test gap.
- **Verdict: PASS**.

## J-22: Push, phone and PSTN reported truthfully

- **Tests**
  - Push: `services/messaging/tests/communications_owner.rs::push_status_reports_not_configured_and_never_echoes_the_handle`, plus `personal_communications.rs::personal_identity_contacts_and_authoritative_policy_across_real_services` (N07: Hub repeats Messaging `NOT_CONFIGURED`, and reports a dependency failure when Messaging is down; markers `truthful_delivery_status, delivery_status_owner_outage`).
  - Phone: see J-03.
  - SMS: `services/sms-gateway/tests/outbound.rs::sending_needs_sms_send_and_a_configured_provider` (503 `PROVIDER_UNAVAILABLE`, nothing recorded).
  - PSTN:
    - `services/telephony/tests/real_interconnect_certification.rs::real_lawful_interconnect_certification` logs "NOT RUN — REAL LAWFUL TELECOM INTERCONNECT CERTIFICATION BLOCKED". It reports `ok` without asserting anything about a carrier.
    - `telephony` `config::tests::the_synthetic_test_interconnect_is_refused_outside_local_and_test`
    - `services/calling/tests/public_app_to_phone.rs` (`PUBLIC_ROUTE_UNAVAILABLE`)
- **Workflow & run**: Messaging 42467223790; Hub 42448888276; SMS 42467223709; Telephony 42467223746; Calling 42467223773.
- **Proves**: without credentials every provider is reported `NOT_CONFIGURED`, `PROVIDER_UNAVAILABLE` or `BLOCKED`, and deterministic or synthetic providers are refused outside local and test. SMS/voice journeys run only on deterministic adapters (Comms e2e 42467223929).
- **Gap**
  - APNs/FCM/Web Push credentials, an SMS provider, and a lawful interconnect with a test line are all missing.
  - The phone label is inconsistent (J-03).
- **Verdict: EXTERNAL_BLOCKED**.

---

## Verdict counts

| Verdict | Count | Journeys |
|---|---|---|
| PASS | 6 | J-09 (backend), J-13 (backend), J-15, J-16, J-20 (backend), J-21 |
| PARTIAL | 13 | J-01, J-02, J-04, J-05, J-06, J-07, J-08, J-11, J-12, J-14, J-17, J-18, J-19 |
| FAIL | 0 | — |
| EXTERNAL_BLOCKED | 2 | J-03, J-22 |
| NOT_COVERED | 1 | J-10 |

## Remaining gaps and owners

| # | Gap | Journeys | Owner |
|---|---|---|---|
| 1 | Real-process Organizations lifecycle across Meeting (authoritative), Organizations and PA: create with one UUID, invite/accept/workspaces, two-organization isolation, suspension/recovery, cutover rehearsal | J-04, J-05, J-06, J-14, J-17 (and the organization legs of J-07, J-12) | backend Section 18 (pending in `organization_lifecycle.rs`) |
| 2 | Organizations outbox wired to `DisabledDispatcher`: no organization event reaches any consumer | J-10, J-14, J-19 | backend, plus owner decision on delivery targets |
| 3 | Messaging organization scope and offboarding revocation not built | J-10 | backend (Section 13 follow-up) + owner product decision |
| 4 | Positive password reset (valid token → new password → old sessions end) untested | J-01 | backend Section 18 |
| 5 | Real-process DP console session revocation check (revoke at Meeting → DP refuses) | J-01, J-13 | backend Section 18 |
| 6 | `GET /api/v2/communications/me/qr` has no test | J-02 | backend Section 18 |
| 7 | Phone label mismatch: Hub `DEFERRED` vs Account `PROVIDER_BLOCKED` | J-03, J-22 | backend Section 18 / owner decision on the canonical label |
| 8 | One personal Hub-launched call through answer → media → hold → end → history | J-11 | backend Section 18 |
| 9 | `enterprise_sip_ivr_certification` intermittently fails at line 1522 (8d7b99a3 42467223863; 22da1aac 42425119285) | J-12 | backend Section 18 (root-cause the flake) |
| 10 | RTC Egress capture SKIPPED on staging CI; capture certification runs only on `main` | J-12 | backend/infra Section 18 (provide Egress config to staging CI) |
| 11 | Per-organization calling: flags, credentials, grants, binding | J-12 | owner release step |
| 12 | Backup drill uses synthetic schemas for 9 of 11 DBs; off-host target and age key custody not chosen | J-18 | backend Section 18 (real-schema fixtures) + owner (target/keys) |
| 13 | Provisioning saga failure modes only against an in-memory PA | J-19 | backend Section 18 |
| 14 | Clinical/healthcare adapter is a double | J-08 | external integration |
| 15 | Browser/mobile journeys: OAuth exchange, contacts/QR, calendar display, guest pre-join, messaging, ringing, sessions UI, developer console, SDK adoption; physical devices unavailable | J-01, J-02, J-07, J-08, J-09, J-11, J-13, J-15, J-20 | Section 17 frontend |
| 16 | APNs/FCM/Web Push, SMS/verification provider, lawful PSTN interconnect | J-03, J-11, J-22 | external provider |
| 17 | External cryptographic certification before production E2EE | J-09, J-21 | external / owner |
| 18 | Gateway/DP deployment over internal TLS (PLT-005) | J-15 | owner |

Workflow note: `personal_communications.rs` (feature `personal-communications-e2e`) is run by **`communications-hub.yml` job `real-services`**, not by `communications-e2e.yml`. The latter runs `services/numbering` tests `communications_e2e` and `relay_cutover_e2e`. `certification.yml` runs `cargo test` without the feature, so it never compiles `personal_communications`. `communications-hub.yml` is path-filtered and was last run on staging at 6105347d.
