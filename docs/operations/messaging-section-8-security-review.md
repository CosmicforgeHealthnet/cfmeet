# Section 8 — internal Messaging security assessment

This is a code-level engineering review, **not independent cryptographic certification**.
Review scope is the existing backend and shared crypto core, not the application UI.
Evidence must be read with the delivery report's exact tested/deployed SHAs and run IDs.

## Production decision authority

The older security architecture labels AD-01–AD-10 pending. The more precise
[ADR 0052](../decisions/0052-messaging-v2-architecture-decision-baseline.md) selects them
as an **engineering development direction** and explicitly withholds production
cryptographic approval. [Issue 13](https://github.com/CosmicforgeHealthnet/cfmeet/issues/13)
is OPEN with no assessment or approval attached at this review. Repository/issue searches
found no completed independent assessment. Development selection is not release approval.

Required outside evidence remains: qualified independent assessment of MLS integration,
device PoP/credential binding, state transitions, AAD, attachments, history/recovery,
transparency/equivocation, clients and token authorization; reviewed/pinned KEYTRANS
profile and independent witness/auditor or equivalent anti-equivocation control;
dependency/legal review including hpke-rs MPL-2.0; target runtime/client security evidence;
critical/high findings closed and residual risk accepted; owner authorization of the exact
production E2EE configuration. No new paid infrastructure is provisioned for this.

The production configuration validator still rejects
`MESSAGING_E2EE_DEVICES_ENABLED=true`. This work does not add an override or reinterpret
MANAGED as PRIVATE_E2EE. Calling, Meetings and unrelated permitted contexts retain their
independent authorization. Off-server disaster recovery is owner-deferred.

## Findings repaired within this milestone

| Finding | Repair | Reproducible evidence |
|---|---|---|
| Delivery Service framing accepted an MLS PublicMessage application frame if other metadata matched; the parser accepts these unencrypted frames | `messages/e2ee.rs` requires PrivateMessage for application content, while preserving public handshake commits. This follows [RFC 9420 section 6](https://www.rfc-editor.org/rfc/rfc9420.html#section-6). No private keys or server decryption added | `messaging::public_mls_application_content_is_never_stored_as_private_ciphertext` submits a well-formed public application fixture through the real router and proves rejection/no stored row; real private sends remain covered |
| Client convenience functions merged membership/update commits before canonical server acceptance, leaving network clients vulnerable to rejected/competing-commit state divergence | Add staged `prepare_*`, `accept_pending_commit`, `discard_pending_commit` around OpenMLS. Block application encryption while a commit is pending. Preserve immediate local-simulation compatibility helpers and wire format; document required network lifecycle | `pending_commits` proves refusal does not advance epoch, restart preserves pending state, competing canonical commit recovery and authenticated delivery; native real-service journey uses staged acceptance |
| The client wrapper returned authenticated application AAD without binding its claimed CF device ID to the actual MLS-authenticated leaf credential | Reserved CF Meet AAD must parse canonically and name the authenticated credential's device before plaintext is returned. Generic protocol-vector AAD remains available for simulation; network clients still verify complete transport scope/AAD | `authenticated_member_cannot_claim_another_devices_identity_in_application_aad` uses a real malicious group member and proves forged attribution is refused while subsequent honest delivery works |
| Queued push jobs checked device status but not current membership/history admission, direct blocks or personal recipient preferences | Dispatch rechecks membership/event visibility and owner blocks. Signed personal Hub decisions use the original authenticated scope references, active accounts and recipient ALL/MUTED/MENTIONS settings. Unavailable authority retries without calling provider | `notification_authorization`; Hub `notification_policy_uses_recipient_preferences_and_live_blocks`; existing realtime/push/Redis suites |
| Logout/session revocation could leave push registrations enabled; a stale InvalidHandle response could disable a newly rotated handle | Disable affected registrations in existing revocation transactions, preserve canonical devices; invalid-handle cleanup matches the attempted handle | notification revocation regression plus existing push-adapter rotation/revocation cases; live mobile delivery remains untested |
| Privacy read errors became default presence-sharing settings | Return an error on failed reads, suppress announcements; only a genuinely absent preference gets the documented default | `privacy_database_failure_does_not_return_default_sharing_preferences` |
| A corrupt/unreadable/deleted KT checkpoint could warn or silently bootstrap instead of stopping protected work; empty-tree roots were not compared | Durable initialization audit, atomic/fsynced checkpoint writes, existing audited onboarding/proof controls on faults, independent checks at enrollment/publication/directory/proof boundaries. A restored file does not silently release incident controls. Validate the empty root too | `corrupted_or_deleted_established_checkpoint_stops_onboarding_and_proofs`, `empty_checkpoint_still_requires_the_correct_merkle_root`, existing fork/rollback/witness/restore suites |
| Concurrent lazy Hub profile creation could conflict on the untargeted principal index | Handle both unique constraints, then validate the immutable winning binding. No constraint relaxed and no identity remapping | Expanded 16-request concurrency test; original two-request race reproduced in run 41930087752, repaired Hub run 41930566685 |
| Native relay omitted implemented consumer operations and Swagger did not identify native transport | Allowlist existing operations, retain domain membership/role authority, bound reaction path decoding against traversal/query injection. Derive native Swagger paths from actual routing; generate a contract/route guide checked in CI | relay/Swagger tests, native five-service journey, SDK/contract generation checks |
| Candidate cloning after runtime preparation could reuse the writable production KT checkpoint or duplicate managed-keyring environment values | Separate staging checkpoint directory and keyring, deduplicate environment values, preserve production originals | `test_staging_never_writes_production_checkpoint_or_loads_its_managed_keyring` |

## Review coverage and limits

| Area | Existing protection/reuse and remaining boundary |
|---|---|
| Protocol/dependencies | OpenMLS =0.9.1; mandatory classical suite 0x0001; cf-messaging-crypto wraps RustCrypto/HPKE implementations, not newly invented primitives. Locked CI, compatibility/RFC/provider vectors, property tests and license/security graph checks. No PQ/security-parity claim. Independent composition/upgrade review still required |
| Device identity and PoP | Server-generated bounded nonce/challenge, challenge-bound registration KeyPackage, credential/device identity and signature binding, ownership scope, limits and replay/idempotency. Registered public material only on server. `devices`, `authorization_matrix`, `key_package`, provider vectors |
| Principal/session authority | Existing Account session/revocation check and stable principal map. Signed narrow delegated scope, live Platform Access validation, device-bound short-lived client sessions, one-use WS subprotocol tickets; no credentials returned to frontend. `client_tokens_rest`, `gateway_forwarding`, native/backend integration tests |
| Membership/epochs | Canonical Messaging membership and MLS group/epoch state, serialized transactional sequence allocation and rekey-required barrier after removal. Clients authenticate decrypted sender/AAD/commits. `messaging`, `mls_security_properties`, multi-device tests; staged commit transport added above |
| Ciphertext authenticity | Server validates framing/group/epoch and authorized device, stores opaque bytes and digest. It cannot verify the encrypted AEAD tag without endpoint keys. A well-framed tampered ciphertext can be relayed but must fail receiving-client authentication; do not describe this as server decryption or a server AEAD verdict |
| Replay/transplant | Stable clientMessageId plus byte binding/idempotency, canonical sequence/digest; group framing and client AAD bind tenant/application/environment/conversation/device/client message ID. Receiving clients reject replay/tampering. `messaging`, `rfc9420_vectors`, parser/security suites |
| Transparency | Signed heads, RFC 9162 inclusion/consistency, checkpoint integrity, witness quorum/cosignature verification/fork rejection. Actual production has no independent witness: UNWITNESSED. A same-host checkpoint protects logical DB restore only; it is not independent anti-equivocation or VPS-loss recovery |
| History/device loss | New device gets future epochs by default. Authenticated device-to-device HPKE transfer requires source/target signatures, bounded expiry and owner/device checks; server relays sealed bytes. Optional backup format is client-encrypted. Recovery without an authorized old device/recovery material cannot be invented by server |
| Private key custody | Backend links public crypto half only, scans schemas and image for secret material. Client exports are Zeroizing buffers and must be encrypted under device-protected storage. Physical-device secure storage/recovery UX and native/browser binding adoption are client responsibilities, not certified here |
| Attachments | Private files encrypted before upload, digest/size/object grants and membership checks, NOT_APPLICABLE_ENCRYPTED scanning. MANAGED content validation/ClamAV/derivatives are separate and policy-gated. Existing self-hosted MinIO SSE-S3/KMS reused; no AWS service dependency. Live signed storage probes are recorded separately from encrypted message attachment acceptance |
| Profile/downgrade isolation | Persisted immutable profiles and DB guards. PRIVATE_E2EE default, explicit MANAGED eligibility/disclosure, no fallback on missing devices/keys; private reactions are encrypted messages, not readable managed rows. No production policy modified by dependency preparation |
| Tenant/environment isolation | Platform Access owner scope, owner-local DBs/roles and explicit native personal scope; no cross-service DB reads in domain services. Backend application contexts keep separately scoped permitted behavior. Policy unavailable means governed operations fail closed |
| Delivery/reliability | PostgreSQL messages+outbox/cursors are canonical; Redis carries pointers/ephemeral state, not persistence. Leases, retries, idempotency, replay and recovery tests retained. Broker outage does not prove loss of committed message. Production outage injection must not disrupt customer databases; isolated tests supply those cases |
| Notifications | Existing verifying-fake APNs/FCM/Web Push tests, structural/host/token validation, metadata-only payloads, retry/expiry and invalid-handle cleanup. No configured live provider or physical-device receipt is inferred from them |
| Browser/code delivery | A malicious origin can ship hostile JavaScript. Standalone crypto harness tests exercise shared cryptography, not the deployed web/mobile product. No frontend, mobile application, Vercel settings or browser crypto artifact is changed by this backend milestone |
| Supply-chain/target limits | Fresh CI results, not old audit statements, govern dependency evidence. Local Windows `cargo check` encountered upstream libcrux-platform 0.0.4 unsafe-cpuid compilation errors; production Linux CI and existing Android/iOS/wasm target gates provide their actual evidence. No vendor-source patch or unsupported Windows-runtime certification is claimed |

## Safe recovery and retained limitations

Configured KT checkpoint files must be restored/verified against the correct log, signed
heads and available independent evidence. Only after the cause is understood may an
authorized operator release the existing E2EE_ONBOARDING / KT_PROOFS controls or resolve
KT_ROLLBACK_DETECTED. Do not delete the initialization audit, reseed a missing established
checkpoint, roll back log history or invent witness signatures to pass a test.

Production managed/storage keys and same-host recovery copies remain restricted files;
old wrapping keys must be retained until relevant data is safely rewrapped. Existing
managed-key rotation tests are not proof a production compromise rewrap was performed.
Total VPS loss can lose data and keys because off-server backups are deferred. No DR/RTO/
RPO certification is asserted. Provider credentials, physical devices, independent
assessment and approved client delivery remain explicitly outside completed evidence.
