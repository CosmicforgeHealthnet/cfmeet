# History transfer v2 implementation security review

Date: 2026-11-24. Scope: candidate implementation in PR #175, not a production attestation.

The reviewer did not author the Rust paged-transfer framing or relay implementation. The reviewer did author parts of the worker/application integration and therefore does not claim independent review of those components. This is an engineering security review, not external cryptographic certification. Existing SAS/source approval is mandatory; this review does not approve replacing it with trust in a server-listed same-account device.

## Decision

**Activation is conditional and currently blocked on the items below.** No new cipher implementation was found: v2 uses the existing OpenMLS crypto provider's X25519 / HKDF-SHA256 / ChaCha20-Poly1305 HPKE and existing device signatures. That reuse does not itself establish protocol or application correctness.

1. Rebuild the browser artifact with the checked-arithmetic framing fix and pass the hostile-length browser regression against that actual artifact. The earlier candidate artifact does not contain this fix.
2. Execute the final real-service application journeys with the new engine, metadata reconciliation, current device lifecycle, revocation and SharedWorker changes. Earlier five-device evidence is not final-implementation acceptance.
3. Verify cancellation/expiry secret cleanup and restart behavior against the final worker. Cleanup fixes described below are source changes until the rebuilt worker tests pass.
4. Preserve explicit source approval and comparison-code verification. Fully unattended authorization of a new source/target trust relationship remains unreviewed and must not be activated by interpreting ordinary directory membership as approval.
5. Treat crash recovery between accepted MLS group commits and local confirmation as a separate unresolved reliability/security gate. A background transfer must not persist an unaccepted MLS candidate, and successful RPC serialization alone does not prove whole-transaction crash recovery.

## Findings and corrections

### H2-01: unchecked frame lengths on wasm32 — blocking, source corrected

`history_transfer.rs::parse_version` previously added an untrusted u32 body/KEM length to a usize offset without checked arithmetic. On wasm32 a length such as `0xffffffff` can wrap before slice validation; native 64-bit refusal does not prove browser safety. Parsing occurs before signature verification. A malicious relay can therefore supply a malformed frame even without a valid source signature.

The shared parser now uses `checked_add` for header, body and signature bounds before slicing. The native `framing_regressions::untrusted_lengths_cannot_wrap_or_escape_the_frame` test passes. A browser regression supplies the hostile v2 manifest length and subsequently requires the same worker and target key to remain usable. Activation requires the rebuilt WASM and that browser result; the native test alone does not exercise 32-bit arithmetic.

### H2-02: local expired transfer secrets and snapshot pages — blocking, source corrected

The earlier worker filtered expired transfer checkpoints out of its resume list without deleting them. Source snapshot generation could also leave encrypted pages without an owning expiry checkpoint if interrupted before manifest publication.

The worker now persists a finite-expiry source `preparing` checkpoint before producing pages. A restart before publication may discard those unpublished pages and regenerate; an already published descriptor remains immutable. Runtime expiry removes only that transfer's reserved pending-store prefix, including the private-key checkpoint and sealed snapshot objects. Authenticated server CANCELLED/EXPIRED/COMPLETED outcomes also trigger local cleanup. Imported message records are preserved. Offline storage cannot be remotely erased while the browser is not running; explicit revocation still denies future relay access.

### H2-03: unaccepted MLS candidate checkpointing — blocking regression gate

Unrelated metadata RPCs could persist a tentative MLS group state while an HTTP commit was still awaiting acceptance. The worker now checkpoints the pre-change state and membership view while a snapshot is pending. KeyPackage generation, registration proofs, encryption and message processing reject `MLS_CHANGE_PENDING` rather than returning mutations absent from the durable checkpoint. The engine also locks whole MLS operations across browser tabs, including HTTP acceptance.

The added browser regression makes an unconfirmed epoch change, refuses KeyPackage publication, writes metadata, restarts the worker and requires the old epoch to remain durable. This protects against checkpointing an unaccepted candidate. It does not solve or certify the separate server-accepted/local-confirmation crash window.

## Boundary audit

| Boundary | Observed implementation and limitation |
|---|---|
| Protocol separation | v2 uses its own label, frame version and request signature domain; v1 does not interpret paged frames. Capability negotiation exposes versions and `sourceApprovalRequired: true`. |
| Context ambiguity | `TransferContext::encode` length-prefixes the label, purpose and seven fixed UUID fields. Object context further binds purpose, snapshot, signed expiry, manifest digest, chunk count, total bytes and object index. |
| Source authentication | Relay upload and target open verify the registered source signature over the full context and sealed frame. The page checks active device status and transparency before approval/receive. The server cannot forge an approved source object without that device key. |
| Target authentication | A v2 request binds the target's transfer public key and expiry under its registered signature. The relay requires source/target ACTIVE under the same authenticated scoped principal. The worker checks its own account/scope/device against context. |
| Confidentiality | HPKE encrypts each object to the target's ephemeral transfer key with the complete object context as AAD. Manifest content, attachment descriptors and private content keys remain inside encrypted envelopes. No MLS group state is transferred. |
| Expiry | The core authenticates expiry but is deliberately clock-agnostic. Relay operations and worker operations enforce current expiry. Direct native/core callers must perform equivalent authorization/time checks; raw `open_object` alone is insufficient. |
| Manifest integrity | The worker checks the exact manifest plaintext SHA-256, authenticated descriptor, ordered chunk digests, lengths/counts and schema before importing. Server SHA-256 of sealed objects is not substituted for client authentication. |
| Ordering and replay | Object index is signed/AAD-bound. Uploads are immutable and byte-idempotent; target imports contiguous indexes and persists records/checkpoint atomically. Duplicate indexes do not import twice. Ordered record identities reject duplicate/out-of-order records across chunks. |
| Completion | Server progress requires the target's descriptor-bound signature and all stored lower-index chunks. `next_index` only increases. Completion requires chunk count reached. Client completion requires verified durable imports; source coverage can remain PARTIAL. |
| Scope and ownership | Relay lookups include tenant, app, environment and principal. Both persistent device rows are checked on data operations, with consistent device-lock-before-transfer-lock order matching revocation. Same-account token possession exposes ciphertext, not the target's private transfer key. |
| Revocation | Terminal device revocation cancels affected transfers and removes relay objects transactionally. Reads and writes recheck both devices. Local wiping remains best effort; previously downloaded plaintext cannot be cryptographically recalled. |
| Visibility | The engine reads all authoritative message pages, requires an explicit visibility boundary, and reconciles tombstones before snapshot/import completion. The worker rejects records outside current supplied windows, preserves removed records, and never grants historical MLS secrets. Full application tests must prove edits, reactions, receipts and deletion reconciliation, rather than infer it from chunk tests. |
| Quotas | 256 KiB plaintext/object, 4096 chunks, 1 GiB transfer, finite TTL and two open transfers/account. The relay serializes account quota checks, checks immutable indexes and stored-byte bounds; DB constraints provide additional limits. These are recovery-abuse limits, not a simultaneous-device product cap. |
| Secret persistence | Transfer seed export is explicitly secret and versioned. The worker persists it only under the account's authenticated local encryption envelope and zeros byte buffers after use. WebAuthn-protected installations require the selected PRF unlock policy. A compromised active same-origin runtime remains inside the stated client threat model. |
| Rollback | Signed contexts prevent relabeling objects between transfers/manifests. Local authenticated encryption does not provide hardware-backed monotonic storage against whole-profile rollback. Fresh authoritative visibility and active-device checks remain required; no stronger rollback resistance is certified. |
| Readiness | v2 uses an additive standalone migration history and is advertised only when the known migration set matches the applied set. Do not apply that migration as an implicit security approval. Old clients stay on the complete bounded v1 path; no partial archive may masquerade as v1 completion. |

## Evidence interpretation

Existing core and browser tests establish useful framing, tamper, large-history and restart properties. They do not independently establish all Account/Hub/Messaging authorization paths, physical authenticators, native SDK parity or actual production incident recovery. The current production `BEFORE_JOIN` and mobile `MESSAGE_REFUSED` incidents therefore remain separately unconfirmed until their specific device/message chronology and available recovery sources are inspected.

This review approves neither encrypted-backup activation nor server-controlled recovery keys. Backup requires its separate design/implementation review. Do not issue multi-device, deployment or production-feature certification from this document alone.


## Authoritative edit-target removal follow-up

Review found that an ACTIVE encrypted EDIT control could outlive its removed base APPLICATION row. Filtering only the EDIT row lifecycle would copy its hidden old plaintext into a new snapshot despite a correct UI tombstone. The worker now records authoritative removals as encrypted per-message references in the existing protected pending store. Read, export, source snapshot and target import paths consult both the record ID and EDIT target ID, including when the base is absent locally, on a different page or known before an interrupted transfer resumes. Removed payloads and attachment material are excluded; no historical MLS state is imported. References survive restarts without an unbounded in-memory metadata map. Regression results are recorded with the final worker evidence; production activation still requires final service tests.

## Private reaction and timeline access review

The real application test exposed a call to the server's plaintext reaction API from PRIVATE_E2EE. That API correctly refuses private reactions. Private reaction state must instead come from validated encrypted APPLICATION controls binding the target UUID, bounded reaction and add/remove intent. Only authenticated active records targeting earlier eligible APPLICATION messages contribute. Canonical message sequence determines the latest operation per principal/target/reaction; the client enforces the existing 20-active-reaction limit. Control messages remain hidden in the timeline, including after removal, and deleted targets suppress both badges and transferred dependent contents. Deleting a control removes that operation from the active-history calculation; it is not a separate user-facing undo protocol.

Independent worker/reducer follow-up found and corrected loss of the hidden-control marker during fast-path tombstoning and a spread-based sequence maximum that could overflow the JavaScript argument limit. Session end clears the new plaintext sequence/cache state. Existing workers do not recognize the new payload type and must upgrade; their durable failed outcomes may retry after upgrade. This is not transparent old-client or native-client compatibility certification.

Private timelines previously used only the latest 200 records, with no older navigation. The new worker historyPage RPC reads at most 201 raw records with a reverse IndexedDB cursor, returns an exclusive raw-record continuation and enforces account/conversation/current visibility bounds. Hidden controls advance that cursor. Real shipped-worker/WASM paging tests passed 29 checks over 600 records, including a control-only page, restart, tombstones and visibility changes; instrumentation refuses getAll/getAllKeys during the paging RPC. Engine/UI integration must preserve those bounds and pass final application acceptance before release qualification.
