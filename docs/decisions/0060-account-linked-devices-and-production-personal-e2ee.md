# ADR 0060 — Account-wide linked devices and the production personal E2EE release

**Status:** Accepted (owner decision, 2026-11-15). Decision 3 (trust) for browsers and the web-limit refusal superseded by [ADR 0067](0067-signing-in-is-enough-for-a-browser.md) (2026-11-19).
**Date:** 2026-11-15
**Extends:** [ADR 0057](0057-account-sessions-typed-tokens-and-revocation-epoch.md) (account sessions, amendment A1 device ↔ session binding), the [device and block authority decision](../architecture/communications-device-and-block-authority.md), [messaging security architecture](../architecture/messaging-security-architecture.md) (AD-05, AD-06, AD-10).

## Context

CF Meet needs one account identity across mobile and web:

* a web-only account (no phone, no SMS, no QR scanning);
* at most one primary mobile, replaced by signing in on a new phone without disconnecting trusted browsers;
* several browsers, each an independent device with its own personal E2EE identity.

The Account authority (Meeting Service `account_sessions`) knew sign-ins but not devices. Every downstream owner already ends a sign-in's credentials on `ACCOUNT_SESSION_ENDED`:

* Hub `communication_devices` (index);
* Messaging E2EE devices, with an MLS pending removal;
* Calling endpoints.

Production E2EE was refused unconditionally pending an independent cryptographic assessment that has not started.

## Decisions

1. **The Account authority owns devices.** It holds a new table `account_devices`: one row per browser profile or mobile installation.
   * Each row is bound to a device-generated Ed25519 key and a client-generated installation id. Neither is ever derived from the User-Agent.
   * A sign-in belongs to at most one device (`account_sessions.device_id`). No second session authority or device registry is created; the Hub, Messaging and Calling stay owners of their credentials.
2. **Database-enforced policy:**
   * at most one ACTIVE mobile per account (the primary): partial unique index `uq_account_devices_primary_mobile`;
   * one live device per installation and per key;
   * revocation is terminal, and device identity is immutable (trigger).

   The web limit `ACCOUNT_MAX_WEB_DEVICES` (default 4) is enforced under a per-account advisory lock. Messaging (`MESSAGING_MAX_ACTIVE_DEVICES` = 10) and the Hub (20) keep their caps as safety ceilings above 1 + 4. They are never reduced to one.
3. **Trust.**
   * A device is trusted by one of:
     * bootstrap: the first device of an account that never had one;
     * approval by a trusted device: the approver types the 6-digit comparison code shown on the new device and signs with its own key;
     * an email step-up code: account recovery.
   * A password alone yields an *untrusted* browser. It can use Meetings and Calling, but it cannot approve devices or enroll personal E2EE (`ACCOUNT_E2EE_REQUIRES_TRUSTED_DEVICE`).
4. **Mobile replacement.**
   * A new mobile is `REPLACEMENT_PENDING`. Its sign-in is refused by every verifier (`DEVICE_REPLACEMENT_PENDING`) except the device endpoints.
   * When its link completes, the following happens in one transaction under the account lock:
     * the previous primary is revoked (`DEVICE_REPLACED`);
     * the old primary's sign-ins end;
     * the new device becomes ACTIVE.
   * The existing durable fan-out then ends the old phone's Messaging device (MLS pending removal and forced rekey), push, client sessions, Calling endpoint, invitations, realtime connections, and only its meeting media (`account_session_media`).
   * Trusted web devices are untouched. Concurrent replacements serialize, and the last committed one remains primary.
5. **Proof of possession.** Device operations sign `cfmeet-account-device-proof:v1\n{purpose}\n{keyThumbprint}\n{context}\n{issuedAt}\n{nonce}`.
   * The nonce is single-use, recorded in PostgreSQL.
   * Refresh of a device-bound sign-in requires a `REFRESH` proof, so a stolen refresh token is useless without the device key.
6. **Compatibility.**
   * Sign-ins that never declare a device (all existing ones, and older clients) stay *unclassified*. They work as before and do not count against limits, so nobody is signed out by the migration.
   * Logout-everywhere revokes every device.
7. **Production personal E2EE (owner decision 2026-11-15).** Production E2EE is allowed only with a release record (`MESSAGING_E2EE_PRODUCTION_RELEASE_FILE`), written by the coordinated `activate-personal-encryption`. The record states the **actual** status:
   * `independentAssessment: NOT_PERFORMED`;
   * accepted residual risks: `NO_INDEPENDENT_CRYPTOGRAPHIC_ASSESSMENT`, `KEY_TRANSPARENCY_UNWITNESSED` (no witnesses are configured) and `WEB_CODE_DELIVERY_TRUST` (AD-06).

   Messaging refuses to start with E2EE on and no valid record. The release tooling accepts `MESSAGING_E2EE_DEVICES_ENABLED=true` only after that activation, with the record mounted. **No certification is claimed**: AD-10 remains open.

## Consequences

* New first-party endpoints (`/api/v2/me/device-policy`, `/me/devices…`, `/me/device-links…`, `/me/security-events`) are classified `EXCLUDED: FIRST_PARTY_APP_ACCOUNT`, like `/me/sessions`. They are not part of the public gateway or the public platform SDKs.
* Clients must declare their device right after sign-in, link a Hub device for every Messaging device or Calling endpoint they register (handoff §11), send `deviceProof` on refresh, and commit pending MLS removals.
* Residual risks:
  * platform attestation (App Attest, Play Integrity) is not configured, so a client's declared device class is not attested. Mitigations: approval or step-up for every new mobile, audit, and an email notice;
  * passkeys are not implemented; the email step-up is the web-only recovery path;
  * the key-transparency log is unwitnessed;
  * web code delivery trust (R1 of the web threat model).


## Phase 3 history reliability clarification (2026-11-24)

The real shipped-worker investigation disproved current-epoch and device-registration-time tests as conclusive evidence of historical non-membership. The remediation branch records authenticated local group join provenance for new devices; legacy provenance stays unknown. Delivery position is distinct from successful processing and failed applications remain retryable. See [the Phase 3 design](../audits/messaging/PHASE3_SECURITY_AND_RECOVERY_DESIGN.md) for implementation boundaries and pending protocol work.

The owner selected local WebAuthn/OS unlock and permits an authenticator prompt for retained local history. Ordinary session termination must eventually lock protected history separately from terminal device removal; existing destructive logout is still operational until cryptographic envelope migration, cross-tab locking and persistent installation/device lifecycle contracts are implemented and accepted. Terminal identities must never be revived. This note does not activate a retention or backup protocol or claim lifecycle decoupling complete.
