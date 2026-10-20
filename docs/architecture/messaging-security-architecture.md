# Messaging security architecture — Part V Version 2.0 (Workstream A record)

Source: *CF Meet Part V — Secure Unified Messaging & End-to-End Encryption Platform*,
Version 2.0, 18 October 2026 (60 pages), Appendix G (normative), Appendix H, Appendix L,
Appendix N. Where this repository and the specification differ, the specification
controls; every difference is listed in
`docs/phases/phase-05/workstreams-a-d-evidence.md` (compliance matrix).

The specification's document control says it is "approved for implementation only after
listed architecture decisions are accepted". None of AD-01…AD-10 is recorded as accepted.
The implementation therefore keeps every production E2EE capability disabled outside
`local` / `test` (`MESSAGING_E2EE_DEVICES_ENABLED`, ADR 0045) and makes no production claim.

## 1. Architecture decision register (Appendix G.1)

"Proposed position" is quoted from the specification. **Status** is the owner's decision,
which this repository cannot supply.

| ID | Decision | Proposed position (specification) | Implemented candidate | Status |
|---|---|---|---|---|
| AD-01 | Primary E2EE protocol | Use IETF MLS 1.0 (RFC 9420) for both direct and group conversations; direct chat is a two-principal MLS group with one leaf per authorized device. | MLS 1.0 only (`profile::PROTOCOL_ID = "MLS_1_0"`); every PRIVATE_E2EE conversation carries a server-assigned 32-byte MLS group id. | PENDING owner approval |
| AD-02 | Reference implementation | Use OpenMLS as the initial Rust protocol implementation, wrapped behind a CF Meet crypto interface and independently tested on required targets. | `openmls =0.9.1`, `openmls_rust_crypto =0.6.0`, `openmls_basic_credential =0.6.0` behind `packages/messaging-crypto`; Linux tested; Android / iOS / wasm32 compile-only. | PENDING owner approval; target certification open |
| AD-03 | Signal / libsignal | Do not make libsignal a production dependency in v2 because upstream states external use is unsupported and the repository is AGPLv3; continue to use Signal specifications as comparative security references. | No libsignal anywhere; no Signal-protocol or interoperability claim. The licence check (`scripts/messaging/check_licenses.py`) rejects AGPL. | PENDING owner approval |
| AD-04 | Post-quantum | Do not combine experimental PQ mechanisms with MLS 1.0 in v2. Provide protocol agility and a future hybrid MLS profile once an appropriate standard / audited library is available. | Classical suite only; no PQ claim. `libcrux-kem 0.0.9` is in the lockfile but in no build graph (CI-proven, scoped RustSec ignore). Protocol agility: protocol id, suite and binding formats are versioned. | PENDING owner approval |
| AD-05 | Key transparency | Implement draft-aligned append-only key transparency with client proofs / monitoring; require external witness / auditor or equivalent anti-equivocation control before strong production claims. | RFC 9162 Merkle log, inclusion / consistency proofs, Ed25519-signed tree heads (dedicated server key), reported `UNWITNESSED`. Not aligned to a pinned KEYTRANS draft yet; no witness / auditor. | PENDING owner approval; witness open |
| AD-06 | Web threat model | Web E2EE is supported but cannot protect against a malicious server that ships hostile JavaScript to the client. High-assurance claims require signed / reproducible native / desktop clients or stronger code-delivery controls. | `web` feature compile-checked for wasm32 only; no Web client exists in this repository. | PENDING owner approval |
| AD-07 | History on new device | New devices receive future messages only by default; historical plaintext requires encrypted backup or authenticated device-to-device transfer. | Message store exists (opaque MLS ciphertext); a new device joins through a Welcome and can decrypt only from its join epoch; members' history windows bound replay. No backup / device-to-device transfer implemented. Nothing grants history server-side. | PENDING owner approval |
| AD-08 | E2EE attachments | Client encrypts before upload; server stores ciphertext and cannot truthfully perform plaintext malware scanning. | Implemented candidate (Part F, ADR 0050): `cf_messaging_crypto::attachment` (XChaCha20-Poly1305 STREAM, per-file key carried only in the MLS-protected descriptor); the server stores ciphertext only (CHECK: no name, no metadata, `application/octet-stream`), never sends it to a scanner and reports `NOT_APPLICABLE_ENCRYPTED`. | PENDING owner approval |
| AD-09 | SMS | SMS is never E2EE. E2EE → SMS requires explicit per-message / channel downgrade and distinct transport copy. | No SMS path exists; `smsBridgeEligible` is always `false` in every policy snapshot. | PENDING owner approval |
| AD-10 | Production claim | Independent cryptographic / security assessment is mandatory before marketing / declaring E2EE production parity with leading secure messengers. | No parity claim; no assessment performed. | PENDING (external assessment not started) |

Also pending: legal confirmation of the MPL-2.0 `hpke-rs` 0.7 crates (file-level copyleft,
used unmodified; ADR 0045); cryptographic review of the device-registration
proof-of-possession composition (§3); production key custody for the key-transparency
signing key (§142).

## 2. Threat model and security objectives (Appendix G.2)

The network, load balancers, application servers, databases, object storage, cloud
administrators and ordinary operators may be observed or compromised. Message content of
PRIVATE_E2EE conversations stays confidential and authenticated while authorized endpoint
devices and their private keys stay secure. No anonymity; no protection from a malicious
authorized recipient; metadata (who, when, sizes, membership) is visible to the server.

| Threat | What this repository does today | Residual risk |
|---|---|---|
| Passive network attacker | TLS at the edge (deployment); MLS ciphertext is opaque to the backend (`OpaqueCiphertext`). | Traffic analysis. |
| Compromised Messaging server / database | No private key, epoch secret or plaintext in the backend: the backend links only the public half of the crypto core (CI graph guard), schemas are scanned for secret-bearing columns (`E2EE_SECRET_IN_SCHEMA`), every certification run scans both databases. | Denial of service, delay, metadata, key substitution attempts. |
| Key-directory substitution | Append-only RFC 9162 log of every device key event; signed tree heads; inclusion / consistency proofs a client verifies with `packages/messaging-crypto`. | Split-view equivocation is detectable only once witnesses / gossip exist (AD-05). |
| Replay / tamper / transplant | MLS AEAD; the application-message AAD binds profile, protocol, suite, tenant, application, environment, conversation, client message id and device (H.1); the acceptance record binds the server sequence and id to the ciphertext digest. Proven in a real two-device OpenMLS group. | An injected forgery consumes the generation key, so the genuine copy of that one message fails: denial of service only. |
| Stolen device | Device revocation is immediate and terminal for API use and closes its KeyPackages. | MLS removal / epoch advance is Workstream E; local plaintext on the device. |
| Malicious recipient | Membership authorization. | Copy / screenshot / forward. |
| Browser XSS / hostile JS | Out of scope of this backend (AD-06). | Full compromise of a Web client. |
| SMS downgrade | No SMS path; policy snapshots never allow it. | — |
| Supply chain | Exact pins of the MLS crates, `Cargo.lock` with `--locked` in CI, RustSec audit, licence allowlist, secret scan. | Zero-day in a dependency; no SBOM signing yet. |

## 3. Protocol and version registry

| Item | Value | Where |
|---|---|---|
| Security profiles | `PRIVATE_E2EE`, `MANAGED` (immutable per conversation; DB CHECK + trigger) | ADR 0048, `messaging_conversations` |
| E2EE protocol | `MLS_1_0` (RFC 9420, `ProtocolVersion::Mls10`) | `packages/messaging-crypto/src/profile.rs` |
| Cipher suite | `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (0x0001) only; 0x0003 not enabled | same |
| Managed protocol id | `CFMEET_MANAGED_V1` (no MLS binding) | migration `m20261102_000002` |
| Device credential identity | `cfmeet-messaging-device:v1:<device id>` | `binding.rs` |
| Registration binding | `cfmeet-messaging-registration:v1:<challenge id>:<nonce hex>` (RFC 9420 `application_id` leaf extension) | `binding.rs` |
| Application-message AAD | `cfmeet-messaging-aad:v2:PRIVATE_E2EE:MLS_1_0:<suite>:<tenant>:<application>:<environment>:<conversation>:<client message id>:<sender device>` (v1 retired before any use) | `binding.rs` |
| Acceptance record | SHA-256, label `cfmeet-messaging-acceptance:v2`, length-prefixed fields | `services/messaging/src/conversations/sequence.rs` |
| Transparency leaf | JSON leaf `version: 1` | `services/messaging/src/transparency/mod.rs` |
| Signed tree head | Ed25519 over length-prefixed `cfmeet-messaging-kt-tree-head:v1`, log id, size, root, timestamp µs | `packages/messaging-crypto/src/transparency.rs` |
| Platform policy | version 2 (SUPPORT defaults to PRIVATE_E2EE; `messages` features) | `services/messaging/src/conversations/policy.rs` |

Every change to an entry is a new version, never an in-place reinterpretation.

## 4. Downgrade rules

1. A conversation's profile is chosen at creation from the effective policy and never
   changes (API refuses with `SECURITY_PROFILE_IMMUTABLE`; DB trigger). The specification
   allows the choice to stay open until the first protected message; this implementation
   is stricter and fixes it at creation.
2. PRIVATE_E2EE is the default for every context class (Appendix G). MANAGED is available
   only where the application's policy enables it and is never a default by itself; a
   context that allows only MANAGED has no default, so the creator must ask for it.
3. If E2EE cannot be established (device operations disabled, no active device for the
   creator or the DIRECT counterpart) creation fails with `E2EE_NOT_AVAILABLE` and
   nothing is written — never a MANAGED fallback.
4. Disabling a context or the E2EE device switch stops new work; it never relabels an
   existing conversation.
5. Moving between profiles requires a new successor conversation with disclosure (later
   workstream). SMS is never E2EE (AD-09) and no SMS path exists.

## 5. Capability matrix (A–G)

| Capability | PRIVATE_E2EE | MANAGED |
|---|---|---|
| Conversation create / read / lifecycle | Yes | Yes (only where application policy enables it; regulated contexts require `managedDisclosureAcknowledged`) |
| Message storage | Opaque MLS ciphertext + acceptance digest (Delivery Service checks public framing only) | Text sealed under the managed key ring (XChaCha20-Poly1305) + full-text index |
| Realtime delivery (Part E) | Ciphertext from committed state; ephemeral typing / presence | Same |
| Attachments (Part F) | Client-encrypted; never scanned, never claimed scanned | Content-type checks + fail-closed self-hosted ClamAV |
| Server search | Never (`LOCAL_SEARCH_ONLY`; device-local index) | Membership-aware PostgreSQL full text |
| Reports / moderation | Metadata + reporter's voluntary, unverifiable disclosure; tombstone / mute / remove / restrict without reading content | Message references; moderators see text only where policy allows |
| Exports | Never (`E2EE_EXPORT_NOT_AVAILABLE`) | MANAGED-only, policy-gated, audited |
| Retention / purge / legal hold | Yes — holds preserve ciphertext only, never create a decryption capability | Yes |
| SMS Bridge | Never | Not implemented (`smsBridgeEligible` always `false`) |
| Device keys | Public material only; private keys device-side | n/a |

## 6. Parts E–G security audit (§96)

Each item is enforced in code and covered by a test; "structural" means a database
constraint or trigger enforces it independently of application code.

| Restriction (§48) | Enforcement | Evidence |
|---|---|---|
| No plaintext substitute for E2EE | PRIVATE_E2EE rows can hold no sealed text / index (CHECK); MLS framing inspected, plaintext refused | `tests/messaging.rs` |
| No E2EE private keys / backdoor on the server | Only public MLS material stored; no decrypt path exists; holds / exports / moderation report `decryptionCapability: NONE` | architecture tests, `tests/compliance.rs` |
| LiveKit not a messaging store; Redis not the durable store | No LiveKit dependency; broker carries pointers; outbox in PostgreSQL is the replay log | `tests/realtime.rs::committed_messages_survive_a_broker_outage` |
| WebSocket delivery is not proof of commit | Delivery only after commit from the outbox | `tests/realtime.rs` |
| Typing never persisted; presence privacy | Ephemeral store with TTL; HIDDEN = OFFLINE | `tests/realtime.rs` |
| No server scanning of E2EE plaintext / no false scan claim | Scanner never invoked for E2EE; CHECK forces `NOT_APPLICABLE_ENCRYPTED` (structural) | `tests/attachments.rs` |
| No E2EE search terms to the server | Refused before any processing; queries never logged | `tests/compliance.rs` |
| No globally accessible attachment URLs | Private bucket, per-object signed reads ≤ grant TTL, re-authorized each time | `tests/attachments.rs::real_object_storage_end_to_end` |
| Removed members get no fresh grants | Membership checked per grant / proxied read | `tests/attachments.rs` |
| Legal holds respected by purge | Every purge / object deletion checks holds; held → preserved | `tests/attachments.rs`, `tests/compliance.rs` |
| No plaintext export of E2EE | `E2EE_EXPORT_NOT_AVAILABLE`, audited | `tests/compliance.rs` |
| No disabled security gates | CI gates extended (attachments, compliance, MinIO, ClamAV), none removed | `.github/workflows/messaging.yml` |
| No WhatsApp / Signal equivalence claim | None made; AD-10 assessment not started | this document |

Residual risks (open): AD-01…AD-10 pending; no independent cryptographic assessment;
key-transparency log UNWITNESSED; Web clients cannot be protected from a malicious
server (AD-06); a grant issued before a revocation remains usable for its TTL (stated
in the response, default 60 s); MANAGED content is readable by the operator by design;
reporter disclosures are unverifiable; ClamAV coverage depends on the deployment's
signature databases; no APNs / FCM adapters; no media processing (durations unverified).
