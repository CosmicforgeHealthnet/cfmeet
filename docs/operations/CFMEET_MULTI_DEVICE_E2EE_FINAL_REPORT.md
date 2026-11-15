# CF Meet — linked devices and multi-device personal E2EE: final report

**Date:** 2026-11-15
**Environment:** production (`https://api.cfmeet.com`)
**Baseline:** `main` `e2c90840`, containing the verified release `8a64b480`
**Decision record:** [ADR 0060](../decisions/0060-account-linked-devices-and-production-personal-e2ee.md)
**Gap register:** [multi-device-gap-register.md](../architecture/multi-device-gap-register.md)
**Client handover:** [multi-device-linked-devices-handover.md](../architecture/multi-device-linked-devices-handover.md)

## 0. Verdict

**`MULTI_DEVICE_E2EE_ACTIVATED — PRODUCTION_VERIFIED; INDEPENDENT_CRYPTOGRAPHIC_CERTIFICATION_NOT_CLAIMED`**

* The account-wide device policy is live in production: one replaceable primary mobile, `ACCOUNT_MAX_WEB_DEVICES` = 4 web devices, and web-only accounts.
* Personal E2EE is active under the owner's release decision. It records:
  * `independentAssessment: NOT_PERFORMED`;
  * accepted residual risks: `NO_INDEPENDENT_CRYPTOGRAPHIC_ASSESSMENT`, `KEY_TRANSPARENCY_UNWITNESSED`, `WEB_CODE_DELIVERY_TRUST`.
* A real multi-device encrypted exchange passed in production (16/16).
* **No independent cryptographic assessment has been performed, and no certification is claimed.**

## 1. Commits (main)

| Commit | Content |
|---|---|
| `70cd3ee2` | Account linked devices (migration, policy, linking, replacement, revocation, meeting media), Messaging release record, contracts and SDK types, ADR, gap register, handover, production tooling |
| `fb2d0576` | Production checks: the `cfmeet-e2ee-acceptance` client (real OpenMLS), the real-services certification test, Calling API checks |
| `d83d59a0` | Web client: account device key, browser linking UI in the existing panels, refresh proofs; Messaging common contract 1.24.0 |
| `c6de964c` | Chromium journey reads the visible linking code (**released**) |
| `45801c93` | Digit-free production operation names (activation and verification ran on this head; images equal to `c6de964c`, 56 build inputs proven) |

## 2. GitHub Actions runs

| Run | Operation | Result |
|---|---|---|
| CI on `fb2d0576` | Service CI | Messaging contract and web journey failed; fixed in `d83d59a0` |
| CI on `d83d59a0` / `c6de964c` | Service CI | All required workflows green; the real-services Chromium journey passed. Telephony SIP certification failed (advisory, external telecom) |
| [43660073717](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43660073717) | Coordinated `release` of `c6de964c` | **success** (details in §3) |
| [43662628261](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43662628261) | `activate-personal-e2ee` | refused by the workflow's input pattern before contacting the VPS; nothing changed. Renamed in `45801c93` |
| [43663108367](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43663108367) | `activate-personal-encryption` | **success** (details below) |
| [43666265601](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43666265601) | `messaging-encrypted-verify` | **PASS 16/16** (§4) |

Release run 43660073717:

* replaced messaging, calling, meeting-service and api-gateway; kept the 9 unchanged images;
* backups taken and restore-tested;
* `account-security-migration` applied `m20261115_000001_account_devices` (the history now holds 2);
* every verification check passed.

Activation run 43663108367:

* release record written; `MESSAGING_E2EE_DEVICES_ENABLED=true`;
* Messaging validated the decision at startup;
* regression passed.

## 3. Deployed images and flags

**Images (release `c6de964c`):**

* replaced: `cfmeet-messaging`, `cfmeet-calling`, `cfmeet-meeting-service`, `cfmeet-api-gateway`;
* kept, with their previous release images: communications, developer-platform (and worker), numbering, organizations, platform-access, privacy-relay, sms-gateway, telephony.

| Flag | Production value |
|---|---|
| `MESSAGING_E2EE_DEVICES_ENABLED` | `true`, under `MESSAGING_E2EE_PRODUCTION_RELEASE_FILE=/run/messaging/e2ee-production-release.json` (decision `OWNER-E2EE-PRODUCTION-RUN-43663108367-1`) |
| `ACCOUNT_MAX_WEB_DEVICES` | default `4` |
| `ACCOUNT_E2EE_REQUIRES_TRUSTED_DEVICE` | default `true` |
| `ACCOUNT_DEVICE_EMAIL_STEP_UP` | default `true` |
| Telecom, SMS and PSTN | unchanged: disabled |

## 4. Production multi-device encrypted exchange (run 43666265601)

**Setup:**

* two fresh, email-verified accounts;
* real OpenMLS 0.9.1 client devices;
* real Ed25519 account-device keys;
* public API only.

**Checks (all PASS):**

1. web_only_account_bootstraps_a_trusted_browser_without_phone
2. second_browser_linked_by_trusted_device_approval
3. first_mobile_becomes_primary_after_approval_and_web_stays_trusted
4. untrusted_browser_refused_e2ee_enrollment
5. one_mls_leaf_per_authorized_device_of_both_accounts
6. web_and_mobile_devices_of_one_account_decrypt_the_same_message_locally
7. web_to_mobile_and_cross_web_sync_of_encrypted_messages
8. mobile_to_web_encrypted_messages_and_idempotent_retries
9. new_mobile_replaces_old_and_web_devices_stay_connected
10. replaced_phone_hub_and_messaging_devices_revoked_by_fan_out
11. web_device_commits_the_mls_removal_and_the_epoch_advances
12. removed_phone_cannot_decrypt_after_rekey_web_devices_continue
13. new_phone_fresh_keys_future_messages_only_and_replies
14. sign_out_everywhere_revokes_all_devices
15. server_stores_only_opaque_ciphertext_no_plaintext

**Server storage**, read-only and counts only, from Messaging's database:

* 6 application messages;
* 0 not opaque PRIVATE_E2EE ciphertext;
* 0 containing any exchanged phrase.

## 5. Calling API behaviours in production

`communications-verify` now checks three behaviours directly:

* `callerDisplayName` on a Hub-launched invitation;
* `canViewRecordings` on `GET /v1/calls/{id}` (false for a credential without `enterprise.recording.read`);
* a genuinely expired signaling credential answers `401 SIGNALING_CREDENTIAL_EXPIRED` with `WWW-Authenticate: error="invalid_token"`, while a forged credential keeps `AUTHENTICATION_FAILED`.

Result: see §7.

## 6. Tests run before deployment

**Meeting Service:**

* full workspace suite;
* 10 new device integration tests (PostgreSQL and Redis, real HTTP router, real Ed25519 proofs);
* route coverage 220/220;
* `unified_messaging_e2e` against the real Messaging and Platform Access binaries.

**Messaging:**

* all 42 test targets;
* a new real-OpenMLS multi-device replacement test;
* production release-record config tests.

**Certification (five real services):**

* Section 14 fan-out;
* native encrypted messaging;
* personal identity;
* contact names;
* the new production E2EE client (14/14 against real services).

**Release tooling:** 335 offline tests.

**Web:**

* type check, lint and build;
* unit tests, including the device-proof format verified with Node Ed25519;
* the real-services Chromium journey (CI).

**Contracts:** validator (98 contracts), bundles, SDK pins, gateway classification.

## 7. Final certification

Pending at the time of writing: `communications-verify` and the final `certify`. This section is completed with their runs.

## 8. Remaining requirements and limitations (not claimed)

* **Independent cryptographic assessment (AD-10):** not performed. The owner accepted the risk; no parity or certification claim is made.
* **Key-transparency witnesses:** not configured (`UNWITNESSED`).
* **Platform attestation of the device class** (App Attest / Play Integrity): not configured. Mitigated by approval or step-up for every new mobile, audit and an email notice.
* **Passkey recovery:** not implemented; the email step-up is the web-only recovery path.
* **Clients:**
  * the web client implements the account device, browser linking and refresh proofs, but has no email step-up or device-list UI yet;
  * the mobile app must adopt the device flow (handover §3) before it can enroll personal E2EE. Until then, mobile sign-ins stay unclassified (Meetings, Calling and MANAGED messaging keep working).
* **Mobile push** (APNs, FCM, Web Push): provider credentials are not configured (unchanged).
