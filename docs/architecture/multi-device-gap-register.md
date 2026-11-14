# Multi-device policy — audit and implementation gap register

Specification: *CF Meet — Final backend security, linked devices and multi-device E2EE* (2026-11-14). Baseline: `main` at `e2c90840`, which contains the verified production release `8a64b480`. Decision record: [ADR 0060](../decisions/0060-account-linked-devices-and-production-personal-e2ee.md).

## 1. Existing authorities (audited)

| Domain | Authority found | Relevant behaviour before this change |
|---|---|---|
| Account | Meeting Service `account_sessions` ([ADR 0057](../decisions/0057-account-sessions-typed-tokens-and-revocation-epoch.md)) | <ul><li>One row per sign-in, rotating refresh with reuse detection, revocation checked in PostgreSQL on every request.</li><li>No device kind, installation id or device key.</li><li>`client_label` is the User-Agent (display only).</li></ul> |
| Communications Hub | `communication_devices` | <ul><li>Session-bound index of one Messaging device and/or Calling endpoint per physical device (`account_session_ref`).</li><li>`ACCOUNT_SESSION_ENDED` revokes the bound devices at their owners.</li><li>Cap `MAX_DEVICES = 20` (hard-coded).</li></ul> |
| Messaging | `messaging_devices`, KeyPackages, MLS Delivery Service | <ul><li>Several MLS devices per principal, one KeyPackage claimed per device, per-device Welcomes and ack cursors.</li><li>Revocation creates MLS pending removals and refuses sends until a member commits them.</li><li>`MESSAGING_MAX_ACTIVE_DEVICES = 10`.</li><li>Production E2EE refused unconditionally.</li></ul> |
| Calling | `call_endpoints` | <ul><li>Fan-out to every eligible endpoint; the first answer wins (`uq_call_answer_claims_participant`); losers get `AnsweredElsewhere`.</li><li>Missed calls derived per logical participant.</li><li>Endpoint revocation cancels live invitations; the signaling WebSocket closes within 5 s.</li></ul> |
| Meetings | `conference_participants` / `participant_sessions` | <ul><li>One logical participant, several device sessions (`max_sessions_per_participant`).</li><li>Media sessions are not tied to sign-ins.</li></ul> |
| Platform Access / Developer Platform | principal mirror, session bridge | The bridge calls `authorize_user_token`, so it honours session revocation. |

## 2. Gap register

| # | Requirement (spec §) | Gap found | Resolution | Evidence |
|---|---|---|---|---|
| G1 | One primary mobile, transactional (§1.5, §9, §10) | No device model | `account_devices`, partial unique index `uq_account_devices_primary_mobile`, advisory lock | migration `m20261115_000001_account_devices`; test `concurrent_mobile_replacements_leave_exactly_one_primary` |
| G2 | New mobile replaces the old one without touching web (§1.6-7, §2C) | — | Pending mobile → link → atomic replacement, then the durable fan-out | `first_mobile_becomes_primary_and_a_new_mobile_replaces_it_without_touching_web` |
| G3 | Web-only accounts, no phone, SMS or QR (§1.8, §2A) | — | Bootstrap trust for the first device; email step-up; QR optional | `web_only_account_bootstraps_a_trusted_browser_and_stays_fully_usable` |
| G4 | Configurable web limit of 4; tabs are one device (§1) | — | `ACCOUNT_MAX_WEB_DEVICES`; one live device per installation; same installation = `SUPERSEDED` sign-in, not a new device | `four_browser_profiles_one_device_each_and_the_fifth_is_refused` |
| G5 | Classification not by User-Agent; proof of possession (§9) | User-Agent label only | Ed25519 device key, signed proofs, single-use nonces | `proofs_are_single_use_and_bound_refresh_needs_the_device_key` |
| G6 | Bound refresh credentials (§9) | Refresh token was a bearer | A device-bound sign-in needs a `REFRESH` proof | same test |
| G7 | Secure linking: one-time, expiring, replay-safe, bound, rate-limited, audited (§4) | None | `account_device_links`: 5-minute TTL, comparison code typed on the approver, 5 attempts, 10 per hour, audit events | `forged_and_cross_account_links_are_refused`, `a_new_device_reads_and_cancels_its_own_link_request` |
| G8 | Revoke one device; others continue (§3, §11) | Sessions only | `POST /api/v2/me/devices/{id}/revoke` (trusted caller + proof, or self) | four-browser test |
| G9 | Replace the mobile during a meeting: only its media end (§6.9-10) | Media not tied to sign-ins | `account_session_media`; session end terminates and queues provider removal | `replacing_a_mobile_during_a_meeting_ends_only_its_media` |
| G10 | Logout everywhere revokes every device (§11) | Sessions only | `revoke_all` also revokes devices | `sign_out_everywhere_revokes_every_device` |
| G11 | Existing sessions not logged out by the migration (§10) | — | Additive migration; unclassified sign-ins keep working and do not count | `unclassified_sign_ins_keep_working_and_never_count` |
| G12 | E2EE only from authorized devices (§4, §5) | Any signed-in session could enroll | `ACCOUNT_E2EE_REQUIRES_TRUSTED_DEVICE`: enrollment through the relay needs a trusted device | `unified_messaging_api` (DEVICE_NOT_TRUSTED), production acceptance `untrusted_browser_refused_e2ee_enrollment` |
| G13 | Multi-device MLS: sync, removal at the rekey boundary, no old secrets for new devices (§5) | Supported by Messaging; not proven for a multi-device account | Real-OpenMLS test with a mobile, two web devices and a peer, including replacement | `services/messaging/tests/messaging.rs::one_account_with_mobile_and_two_web_devices_replaces_its_mobile` |
| G14 | Controlled production activation; no claim without evidence (§12) | Unconditional refusal | `MESSAGING_E2EE_PRODUCTION_RELEASE_FILE` with the actual status; coordinated `activate-personal-e2ee`; safe defaults widened only after it | `production_e2ee_needs_a_valid_owner_release_record`, offline release tests |
| G15 | Notify surviving devices of a replacement (§2C.13, §9) | Hub change feed only | Security log `GET /api/v2/me/security-events` plus an email notice | first-mobile test (event and email) |
| G16 | Cap reconciliation (§10) | Messaging 10, Hub 20, Account none | Account enforces 1 + `ACCOUNT_MAX_WEB_DEVICES`; downstream caps stay as ceilings (≥ 5, never 1) | ADR 0060 §2 |
| G17 | Calling: one answer wins across mobile and web (§7) | Already satisfied | No change | Calling `ringing.rs` arbitration; enterprise acceptance |
| G18 | Organization authorization unchanged by linking (§8) | Satisfied: membership is checked live per request | No change; devices carry no organization rights | organization acceptance (unchanged code path) |
| G19 | Production checks for `callerDisplayName`, `canViewRecordings`, `SIGNALING_CREDENTIAL_EXPIRED` | Unit and contract tests only | Direct checks in `communications-verify` | `communications_acceptance.py::calling_api_checks`, `expired_signaling_check` |
| G20 | Real production multi-device encrypted exchange | Blocked (E2EE off) | `messaging-e2ee-verify`: real OpenMLS clients and a read-only storage check | `infra/calling/production/e2ee_acceptance.py`, `certification/src/bin/cfmeet-e2ee-acceptance.rs` |

## 3. Not done (documented residual)

| Item | Reason |
|---|---|
| Platform attestation of the device class (App Attest / Play Integrity) | Needs Apple and Google onboarding; mitigated by approval or step-up for every new mobile, audit and an email notice |
| Passkey (WebAuthn) recovery | No WebAuthn stack in the product; the email step-up is the web-only recovery path |
| Key-transparency witnesses | External witness or auditor not available; recorded as an accepted risk (`KEY_TRANSPARENCY_UNWITNESSED`) |
| Independent cryptographic assessment (AD-10) | Not performed; recorded as `NOT_PERFORMED`; no certification is claimed |
| Server-side MLS removal commits | Impossible by design (the server holds no group secrets); surviving devices commit, and sends are refused until they do |
| Encrypted history transfer to a replacing phone | The existing device-to-device transfer applies; without a surviving trusted device, history stays inaccessible (proven) |
| Public platform SDK operations for device management | First-party account endpoints are `EXCLUDED` from the public gateway and SDKs (the same class as `/me/sessions`); the web and mobile generated contract types are updated |
