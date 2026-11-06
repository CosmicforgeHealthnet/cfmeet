# PRIVATE_E2EE — independent cryptographic review commissioning package

**Status: READY FOR COMMISSIONING. No independent assessment has been performed.**
Production activation of personal PRIVATE_E2EE messaging stays refused by the Messaging
configuration validator (`MESSAGING_E2EE_DEVICES_ENABLED=true` is rejected in production)
until all of the following are recorded against
[issue 13](https://github.com/CosmicforgeHealthnet/cfmeet/issues/13):

1. an assessment report by a qualified independent party covering the scope below;
2. every critical and high finding closed, re-tested, and the residual risk accepted in writing;
3. the owner's release approval of the exact production E2EE configuration and image digests.

This package is engineering preparation. It does not grant approval, and no part of it
may be cited as approval. The engineering self-review that preceded it is
[messaging-section-8-security-review.md](../operations/messaging-section-8-security-review.md).

## 1. What is being reviewed

| Item | Identifier |
|---|---|
| Repository | `CosmicforgeHealthnet/cfmeet` |
| Review source revision | the service source SHA of the current production release, recorded in [cfmeet-sections-8-23-final-closure.md](../operations/cfmeet-sections-8-23-final-closure.md) §2; assessors should pin that exact commit |
| Shared crypto core | `packages/messaging-crypto` (Rust; native, FFI, wasm targets) |
| Delivery service | `services/messaging` (device registry, KeyPackages, MLS framing checks, opaque ciphertext storage, key transparency, history-transfer relay, attachments) |
| Client binding surfaces | `packages/messaging-crypto/ffi`, `packages/messaging-crypto/wasm`, `packages/messaging-crypto/web-harness` (harness only; no shipped client is in scope of this backend package) |
| Contracts | `packages/contracts/messaging/**` (public, native and internal OpenAPI, realtime frames) |
| Decisions | ADR [0045](../decisions/0045-messaging-domain-and-cryptographic-core.md), [0046](../decisions/0046-platform-access-messaging-scopes-and-device-authority.md), [0047](../decisions/0047-messaging-service-devices-and-key-transparency.md), [0049](../decisions/0049-messaging-realtime-gateway.md), [0050](../decisions/0050-messaging-attachments.md), [0051](../decisions/0051-messaging-search-moderation-compliance.md), [0052](../decisions/0052-messaging-v2-architecture-decision-baseline.md) |
| Architecture | [messaging-security-architecture.md](../architecture/messaging-security-architecture.md), [web-messaging-client-threat-model.md](../architecture/web-messaging-client-threat-model.md), [security-deployment-checklist.md](../architecture/security-deployment-checklist.md) |

## 2. Threat model summary (for scoping)

Adversaries the design claims to resist, each to be confirmed or refuted by the assessor:

| Adversary | Claim under review |
|---|---|
| Honest-but-curious or compromised Messaging server / database operator | cannot read PRIVATE_E2EE message or attachment plaintext; holds only public device material, opaque ciphertext and digests |
| Malicious server attempting key substitution | detected by clients through key transparency (signed heads, RFC 9162 inclusion/consistency proofs, checkpoint rollback detection); **production has no independent witness (UNWITNESSED)**, so equivocation resistance is limited to what a single operator log provides |
| Malicious group member | cannot forge another device's attribution (authenticated credential bound to CF Meet AAD), cannot keep access after removal beyond the epoch boundary |
| Network attacker | TLS everywhere; device-bound short-lived client sessions; one-use WebSocket tickets |
| Replay / transplant across scopes | framing and AAD bind tenant, application, environment, conversation, device and client message id; server dedupes by client message id and byte digest |
| Downgrade to MANAGED | profiles are immutable per conversation; no fallback on missing devices or keys; MANAGED requires explicit eligibility and disclosure |
| Stolen device | device revocation, rekey-required barrier after removal; recovery of history needs an authorized old device or client-held backup material |

Explicitly **out of scope of the backend claim** (client responsibilities, to be assessed
with the shipped clients): secure key storage on devices, browser code-delivery integrity
(a malicious origin can ship hostile JavaScript), UI safety-number verification UX.

## 3. Cryptographic dependency inventory

Exact versions from `packages/messaging-crypto/Cargo.lock` at the review revision:

| Crate | Version | Use |
|---|---|---|
| openmls | =0.9.1 | MLS (RFC 9420) group protocol, mandatory ciphersuite 0x0001 (MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519) |
| openmls_traits | =0.6.0 | provider interfaces |
| hpke-rs / hpke-rs-crypto / hpke-rs-rust-crypto | =0.8.0 | HPKE for history transfer and backups (MPL-2.0: legal review required) |
| ed25519-dalek | 2.2.0 | device signatures, transparency head signatures |
| x25519-dalek / curve25519-dalek | 3.0.0 / 4.1.3, 5.0.0 | X25519 |
| aes-gcm | 0.11.1 | AEAD (suite 0x0001) |
| chacha20poly1305 | 0.10.1, 0.11.0 | AEAD (backups, managed keyring in the service) |
| hkdf / sha2 | 0.12.4, 0.13.0 / 0.10.9, 0.11.1 | KDF / hashing |
| p256 | 0.14.0 | service-side ECDSA/ECDH where required by contracts |
| zeroize | 1.9.1 | secret buffer clearing |
| libcrux-platform | 0.0.4 | platform feature detection (known upstream build issue on Windows; Linux CI authoritative) |

Two versions of some crates coexist through transitive dependencies; the assessor should
confirm which version each code path links. `cargo deny`/advisory gates run in the
"Dependency and image security" workflow on every change.

## 4. Areas the assessment must cover

| Area | Where to look | Existing tests and vectors |
|---|---|---|
| Device enrollment and proof of possession | `services/messaging/src/devices/`, `packages/messaging-crypto/src/device.rs`, `key_package.rs` | `device_state.rs`, provider vectors, service `devices`, `authorization_matrix`, `key_package` suites |
| Key custody | `packages/messaging-crypto/src/device.rs`, `binding.rs`, `ffi/`, `wasm/` | secret-scan of schemas and images; Zeroizing exports |
| MLS epochs and group changes | `packages/messaging-crypto/src/group.rs`, `services/messaging/src/messages/e2ee.rs` | `mls_compatibility.rs`, `mls_security_properties.rs`, `pending_commits.rs`, `multi_device_welcome.rs`, `rfc9420_vectors.rs` |
| Framing, AAD and attribution | `packages/messaging-crypto/src/framing.rs`, `verification.rs` | `parser_properties.rs`, `authenticated_member_cannot_claim_another_devices_identity_in_application_aad`, `public_mls_application_content_is_never_stored_as_private_ciphertext` |
| Authentication and authorization | `services/messaging/src/http/middleware.rs`, `services/messaging/src/realtime/sessions.rs`, Platform Access scopes (ADR 0046) | `client_tokens_rest`, `gateway_forwarding`, native five-service journey |
| Replay protection | client message id binding, sequence allocation in `services/messaging` | `messaging`, `rfc9420_vectors` |
| Key transparency | `packages/messaging-crypto/src/transparency.rs`, `services/messaging/src/transparency/` | fork, rollback, witness, restore suites; `corrupted_or_deleted_established_checkpoint_stops_onboarding_and_proofs`, `empty_checkpoint_still_requires_the_correct_merkle_root` |
| Backup and history transfer | `backup.rs`, `history_transfer.rs` | `backup_vectors.rs`, history transfer suites |
| Safety codes | `profile.rs`, `verification.rs` | `safety_code_vectors.rs` |
| Attachments | `attachment.rs`, `services/messaging/src/attachments/` | encrypted-attachment suites; MANAGED scanning is separate |
| Local search | `local_search.rs` | unit suites (device-only) |

Test vectors live in `packages/messaging-crypto/tests/vectors/`.

## 5. Known risks and open items (assessor input, not dispositions)

- **No independent transparency witness in production** (UNWITNESSED). A same-host
  checkpoint protects against logical rollback only.
- **No off-server backups** (owner-deferred): total VPS loss can lose server-side state.
- **hpke-rs is MPL-2.0**: dependency/legal review outstanding.
- **Server cannot verify AEAD tags**: tampered but well-framed ciphertext is relayed and must
  fail at receiving clients; the server never issues an AEAD verdict.
- **Clients not in this package**: shipped web/mobile clients, secure storage, and code
  delivery need their own assessment before personal E2EE is offered to users.
- **No post-quantum claim.**

## 6. Release checklist (all must be true before the production flag is changed)

| # | Gate | Evidence location |
|---|---|---|
| 1 | Independent assessment report attached to issue 13 | issue 13 |
| 2 | Critical/high findings closed with regression tests on the release revision | issue 13 + CI run ids |
| 3 | Residual risk acceptance signed by the owner | issue 13 |
| 4 | Transparency witness decision recorded (independent witness, or explicit owner acceptance of UNWITNESSED) | issue 13 |
| 5 | Licence review of hpke-rs (MPL-2.0) | issue 13 |
| 6 | Client assessments for the clients that will expose personal E2EE | issue 13 |
| 7 | Owner approval of the exact configuration and image digests | issue 13 |
| 8 | A dedicated, reviewed change that lifts the validator refusal for production, deployed by the normal release workflow with stage acceptance | PR + release run id |

Until gate 8, `communications-release` and `messaging-configure` keep refusing production
E2EE activation, and Sections 8–23 closure records PRIVATE_E2EE as `BLOCKED_EXTERNAL`.
