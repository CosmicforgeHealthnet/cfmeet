# 09 — Security review (Part M §24–§26, Part N §72, §81, §96)

## Result

| Severity | Found in M/N | Resolved | Unresolved |
|---|---|---|---|
| Critical | 0 | — | **0** |
| High | 1 (SEC-M-01) | 1 | **0** |
| Medium | 0 | — | 0 |
| Low | 0 | — | 0 |

Security closure condition (0 critical, 0 high unresolved): **MET**. No known exploitable
identity bypass, authorization bypass, cross-conference IDOR, stale-credential privilege
restoration, provider-control bypass, recording-consent bypass or artefact-access bypass
remains.

### SEC-M-01 (HIGH, fixed) — expired session could refresh / replay its media credential

- **Found by**: `tests/credential_replay.rs::replay_expired_session` (Part M §26).
- **Defect**: a `participant_sessions` row past its `expires_at` but still `PENDING` /
  `CONNECTED` could call `POST /conferences/{id}/media-credential` and receive a fresh,
  full-publish credential (200); the provider `participant_joined` callback also accepted
  the identity; nothing ever swept such sessions.
- **Fix** (staging, this commit):
  - `ParticipantSession::is_live_at(now)` = active state **and** not past `expires_at`
    (`src/models/participant_sessions.rs`, unit-tested).
  - Credential issuance refuses an expired session with `PARTICIPANT_STATE_CONFLICT`
    ("Your session has expired; join the meeting again") and caps every credential TTL at
    the session's remaining lifetime (`src/services/collaboration/media.rs`).
  - Webhook authority uses `is_live_at` → stale identity evicted (`STALE_SESSION_EVICTED`)
    (`src/services/rtc_webhooks.rs`).
  - Reconciler marks expired active sessions `EXPIRED` (drift kind `SESSION_EXPIRED`,
    repaired) and evicts the identity (`src/workers/conference_reconciler.rs`).
- **Proof**: with the fix stashed the test fails (refresh → 200 with a publish credential);
  with the fix it passes. Permanent regressions: `replay_expired_session`,
  `refreshed_credential_is_capped_by_session_expiry`, unit
  `participant_sessions::tests::*`.

## Mandatory security regression matrix (M-S1 … M-S15)

| ID | Requirement | Permanent regression tests | Status |
|---|---|---|---|
| M-S1 | Unauthenticated caller cannot submit host `user_id` and obtain host role | `security_v2::s01_no_user_id_impersonation`, `s02_no_client_supplied_role`, `platform_security::l_s01_forged_registered_identity_rejected_on_join`, `legacy_v1_join::v1_unauthenticated_cannot_impersonate_registered_user`, `legacy_v1_web_compat::web_arbitrary_user_id_cannot_impersonate` | PASS |
| M-S2 | Guest using another guest's display name cannot inherit identity / session | `security_v2::s09_display_name_is_not_identity`, `platform_security::l_s03_display_name_cannot_become_identity`, `legacy_v1_web_compat::web_guest_display_name_copy_does_not_impersonate` | PASS |
| M-S3 | Guest cannot send chat as another guest | `collab_security::s06_chat_sender_not_forgeable` (guests; sender fields rejected, sender = credential's participant), `security_v2::s10_guest_credentials_are_scoped` | PASS |
| M-S4 | Host of A cannot admit / deny / remove / ban / role-change / mute / lock / breakout-move a participant of B | `invariants_certification::m_s04_host_a_cannot_moderate_conference_b` (all 10 actions × both path forms; B fingerprint unchanged; zero provider calls on B's identity), `idor_matrix`, `security_v2::s11_to_14_cross_conference_idor`, `rtc_security::s12_cross_conference_provider_control`, `collab_security::s05_cross_conference_media_controls` | PASS |
| M-S5 | Participant invite cannot self-promote to HOST / CO_HOST | `security_v2::s05_invitation_cannot_escalate`, `s15_member_cannot_self_promote`, `platform_security::l_s05_invitation_cannot_exceed_granted_role`, `l_s07_unauthorized_role_escalation_rejected` | PASS |
| M-S6 | VIEWER modified client cannot publish mic / camera / screen | `rtc_security::s05_s06_viewer_cannot_publish`, `collab_security::s04_viewer_cannot_publish`, `s03_modified_client_cannot_share`; **real SFU**: `livekit_certification::real_livekit_full_lifecycle` (VIEWER publish denied), `real_livekit_part_h_*`, media load (attendee camera denied) | PASS |
| M-S7 | Removed participant is removed at the provider; old credential cannot restore access | `rtc_security::s08_removed_participant_disconnects`, `credential_replay` (removed), `failure_injection_m::m_f_livekit_unavailable_during_remove`, **real SFU** removal in `real_livekit_full_lifecycle` | PASS |
| M-S8 | Banned participant cannot rejoin with old credential | `rtc_security::s09_banned_cannot_get_credential`, `security_v2::s16_banned_cannot_hold_session`, `credential_replay` (banned), **real SFU** `real_livekit_m_waiting_subscription_ban` (old credential reconnect → evicted; no new credential; no active session) | PASS |
| M-S9 | ENDED conference refuses late credential issuance / reconnect | `security_v2::s19_ended_cannot_join`, `rtc_security::s10_s11_ending_and_ended_issue_no_credentials`, `credential_replay` (pre-end) | PASS |
| M-S10 | Revoked invitation fails | `security_v2::s06_07_08_invitation_lifecycle`, `credential_replay` (revoked / exhausted), `system_e2e::r_e2e_invitation_revoke_expiry_and_ban_rejoin` | PASS |
| M-S11 | Expired invitation fails | same as M-S10 | PASS |
| M-S12 | Passcode never grants member / host identity | `security_v2::s04_passcode_grants_no_privilege`, `s03_code_is_not_authority`, `platform_security::l_s04_meeting_code_does_not_imply_host` | PASS |
| M-S13 | PRIVATE meeting rejects unauthorized actor | `security_v2::s20_private_access_modes`, unit `meeting_access::tests::*` | PASS |
| M-S14 | ORGANIZATION meeting rejects external actor | `organization_domain_access::org01..org03`, `dom01`, `dom02`, `security_v2::s21_body_cannot_claim_organization` | PASS |
| M-S15 | HEALTHCARE meeting rejects unauthorized actor | `security_v2::s20b_healthcare_adapter`, `profiles_h::h_s01_*`, `h_s11_*`, `h_f05/h_f06` (adapter unavailable / timeout fail closed), `appointment_revocation::*`, `platform_security::l_s16_healthcare_profile_is_not_permissive` | PASS |

All A–L security suites are retained unchanged: `security_v2` (23), `rtc_security` (12),
`collab_security` (18), `platform_security` (15), `profiles_h` (H-S1..15),
`operations_i` (I-S1..10).

## IDOR / cross-conference fuzz matrix (§25)

`tests/idor_matrix.rs`: two complete worlds A and B (spaces, conferences, members,
guests, invitations, registrations, breakouts, polls, questions, messages, recordings,
artefacts). Every documented v2 path is re-issued by A's actors with B's identifiers
substituted (derived from the OpenAPI contract), plus 13 body-identifier cases (`participantIds`,
`participantId`, `toBreakoutId`, `participantSessionId`, assignment ids …).
**61 attempts → all 403 / 404 / 409 / 410**; B's 16-table fingerprint (md5), provider call
count and object-store keys unchanged. Positive control: 6 GET routes with A's own ids
succeed.

## Token / credential replay (§26)

`tests/credential_replay.rs` (8 tests): expired session (SEC-M-01), TTL capped by session
expiry, removed / banned, pre-demotion and pre-lock (grants re-asserted at the provider),
pre-breakout-move (`STALE_SESSION_EVICTED`), pre-end, revoked / exhausted invitation and
consumed registration token, expired guest session re-authorization. All PASS.

## Original high-risk themes (§81)

| Theme | Previous risk (legacy v1) | Current control | Permanent regression | Status |
|---|---|---|---|---|
| Identity impersonation | Client-supplied `user_id` trusted on join | `ActorContext` resolved only from verified credentials; DTOs `deny_unknown_fields` reject `userId`, `role`, … | M-S1 tests; `dto::v2::tests` (17 DTOs × 8 injected fields); `platform_security::l_s18_dto_cannot_inject_authority` | PASS |
| Guest identity | Display name used as identity | Server-issued opaque participant token (hashed); display name is label only | M-S2 | PASS |
| Cross-meeting IDOR | Ids accepted without scoping | Every lookup scoped `(id, conference_id)` (composite FKs `uq_participant_scope`, …) before authorization | §25 matrix, M-S4 | PASS |
| Stale credentials | Long-lived client tokens | Short TTL ≤ session lifetime; webhook + reconciler eviction; grants re-asserted | §26, SEC-M-01 | PASS |
| Role escalation | Client chose role | `MeetingPermissionService` decides; role changes authority-checked and capped | M-S5 | PASS |
| Media permission bypass | Client UI enforced mute / publish | Provider grants pushed by LiveKit (`UpdateParticipant`), locks re-asserted | M-S6, `collab_security::s01/s02`, real SFU Part F | PASS |
| Ban / rejoin | No durable ban | `conference_bans` (conference / space scope); bans checked on every join / refresh / webhook | M-S8 | PASS |
| Provider callback forgery | — | Signed LiveKit webhooks (JWT + body hash); replay dedupe; unknown rooms cannot mutate | `rtc_security::s01..s04`, `platform_security::l_s12_s13` | PASS |
| Recording consent bypass | — | Consent / policy gates before provider start; withdrawal handling; healthcare off by default | `recording_g` G-S*, `profiles_h::h_s13`, `system_e2e` (`CONSENT_REQUIRED` until accepted) | PASS |
| Artefact access | — | Authorised, scoped, expiring signed reads; storage keys never exposed | `platform_security::l_s14_s15`, `recording_g::g_s13`, real S3 `CERT S3 signed read` | PASS |
| Healthcare permissive defaults | — | Waiting room default on, recording off, external intelligence blocked, fail-closed adapter | M-S15, `profiles_h::h_e2e_15`, `h_s14` | PASS |

## Artifact hygiene

Certification logs and CI artifacts never include secrets, signed artefact URLs,
recording content or real clinical data. Real-provider runs print only `CERT … PASS`
lines (no URLs); CI uploads filtered lines and JSON reports only
(`.github/workflows/phase2-backend-certification.yml`). All data is synthetic.
