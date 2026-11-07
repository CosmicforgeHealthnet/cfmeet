# Device registries and block stores — authority decision (DATA-005, DATA-006)

Blueprint v1.0 Section 14. Decision record for the audit's "four device registries" and "three block stores". It extends [ADR 0057 amendment A1](../decisions/0057-account-sessions-typed-tokens-and-revocation-epoch.md) and the [target authority map](cfmeet-target-authority-map.md). No data is removed by this decision; every step below is additive or a documented later contract step.

## 1. Devices (DATA-005)

| Store | Decision | Why |
|---|---|---|
| Account `account_sessions` | **Authority for sign-ins** (root credential of a device's CF Meet access) | ADR 0057 Decision 2 |
| Messaging `messaging_devices` (+ key packages) | **Authority** for E2EE devices | Only Messaging can verify MLS state |
| Messaging `messaging_session_devices` | **Authority** for MANAGED session devices; bound to an account session | ADR 0049 A1 |
| Messaging `messaging_push_registrations` | **Authority**, subordinate to the Messaging device (not a separate registry) | push tokens never leave Messaging |
| Calling `call_endpoints` | **Authority** for call endpoints | signaling and invitations |
| Hub `communication_devices` | **Projection / index**: a reference that groups one Messaging device and/or one Calling endpoint per physical device, now bound to the account session that linked it | gives the user one list; never stores keys or tokens |
| Meeting `audio_video_devices` | **DEPRECATED** (dead table: no handler reads or writes it) | legacy |

Rules that make the projection safe (implemented):

* The Hub never revokes on its own belief: `GET /devices` and `POST /devices/link` re-verify every ACTIVE reference with its owner and mark it `REVOKED (OWNER_SERVICE)` only on an explicit owner answer (`reconcile_devices`). An unavailable owner changes nothing.
* Revocation goes to the owners first, then the reference (`revoke_device`, `end_session_devices`); owner calls are idempotent, so a retry completes a partial failure.
* Section 14: a reference is bound to its sign-in; ending either ends the other (ADR 0057 A1). This is the single read path for "what can this device still do": the account session (`GET /api/v2/me/sessions`) plus the references it linked (`GET /api/v2/communications/devices`).

Migration path (later, owner-approved): (1) clients link devices at registration (handoff §11) so the index is complete; (2) an operator report of ACTIVE owner devices without a Hub reference (read-only, per principal) once clients link; (3) drop `audio_video_devices` only after legacy retirement (Section 16, D-9: measured zero consumers, backup first).

## 2. Blocks (DATA-006)

| Store | Decision | Scope |
|---|---|---|
| Hub `communication_block_bridge` rows with `source = 'COMMUNICATIONS'` | **Authority for personal (user-to-user) blocks** in the FIRST_PARTY personal scope (`PUT/DELETE /api/v2/communications/profiles/{cfMeetId}/block`, `GET /blocks`) | CF Meet personal communications |
| Messaging `messaging_blocks` | **Authority** for blocks made through the Messaging API (customer tenants; legacy first-party rows) | Messaging scope |
| Calling `calling_block_rules` | **Authority** for Calling block rules (customer tenants, enterprise) | Calling scope |
| Hub bridge rows with `source = 'CALLING' / 'MESSAGING'` | **DEPRECATED projection**: the `block-events` ingress that would write them has no producer and is marked deprecated in `cfmeet.communications.communications-internal` 1.3.0; no decision reads them | — |

The single read path already exists and stays: every Hub policy decision (`policy/check`, call launch, conversation creation, message send/notification) evaluates the Hub's own block **and** asks Calling and Messaging live (`State::domain_blocks`); any block denies. Owners also re-check with the Hub for personal actions. So the three stores never disagree in effect: they are unioned at decision time, fail closed when an owner is unavailable.

Migration path: (1) the `block-events` route is removed with the next major version of the Hub internal contract (no caller exists; rows written by it are inert); (2) first-party clients block only through the Hub (handoff §11), so new first-party blocks land in one store; (3) legacy first-party `messaging_blocks` rows keep being honoured by the live union and are never deleted by this programme.

## 3. Evidence

* Hub: `services/communications/src/devices.rs` (`end_session_devices`, `bound_session`, `reconcile_devices`), `policy.rs` (`domain_blocks`), migration `m20261108_000001_device_session_binding`.
* Tests: Hub `devices_are_bound_to_the_account_session_that_linked_them`, `owner_side_revocations_reconcile_and_outages_change_nothing`; certification `hub_facades_device_sessions_and_revocation_fan_out_across_real_owners`.
