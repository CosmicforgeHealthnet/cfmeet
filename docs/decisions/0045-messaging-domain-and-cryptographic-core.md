# ADR 0045 — Unified Messaging domain registration and the MLS cryptographic core

Status: Accepted for the foundation (Part V Workstream A). The cryptographic choices are
the specification's PROPOSED architecture; production enablement of E2EE device
operations waits for the owner decisions listed under "Open decisions".

## Context

Part V Version 2.0 introduces Unified Messaging as a first-class backend domain with two
security profiles: `PRIVATE_E2EE` (default for eligible private conversations; the server
never holds decryption keys) and `MANAGED` (server-processable enterprise / regulated
messaging, never presented as E2EE). The proposed protocol is MLS 1.0 (RFC 9420) with
OpenMLS and the mandatory cipher suite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`.

The 60-page Part V specification document was not available when this record was first
written; it was derived from the implementation directive that restates its requirements.
Revision (18 October 2026): the record has since been reviewed against the specification
itself (Version 2.0, Appendices G, H, L, N). The exact AD-01 … AD-10 positions, their
pending status, the threat model, the protocol / version registry and the downgrade rules
are in `docs/architecture/messaging-security-architecture.md`; the requirement matrix and
the corrections it led to are in `docs/phases/phase-05/workstreams-a-d-evidence.md`. No
AD is recorded as approved, so every one is treated as **not yet approved**.

## Decisions

1. **Service identity.** `cfmeet.messaging` is registered in `packages/service-auth`
   (`services::MESSAGING`, `ServiceId::messaging()`) and named in the
   `cfmeet.platform.common` `ServiceId` description (1.9.1). Registration grants nothing:
   a test pins that no permission is received by Messaging and lists exactly what it may
   be granted (ADR 0046 adds the Platform Access reads).
2. **Repository boundaries** (`scripts/service-boundaries`, mutation-tested). Registering
   the identity extends every existing rule to Messaging (foreign database URLs, DB
   helpers, table access, Cargo / include coupling, packages never depending on
   services). New rules:
   * `MESSAGING_DOMAIN_ELSEWHERE` — only Messaging creates `messaging_*` tables;
   * `MESSAGING_CRYPTO_ELSEWHERE` — no other service stores KeyPackages, MLS state or
     Messaging devices (Platform Access owns principal authority, Messaging owns device
     PUBLIC keys);
   * `E2EE_SECRET_IN_SCHEMA` — Messaging and Platform Access schemas never name private
     keys, epoch / exporter / init / welcome secrets, HPKE private keys, attachment /
     media / recovery / backup keys or plaintext message bodies (SQL string literals and
     comments are ignored so a guard that FORBIDS such a key is not flagged);
   * `PRODUCT_DOMAIN_IN_MESSAGING` — no product business tables (orders, trips,
     patients, …) and no CosmicForge product names in Messaging production code;
   * `PLAINTEXT_CONTACT_FIELD` now also covers Messaging code and schemas.
3. **Contract namespace.** Messaging contracts live under `packages/contracts/messaging/`
   (`cfmeet.messaging.*`). The contract authority's rules — owners are existing
   services and "contracts describe what exists" — mean they are registered together
   with the service and endpoints that implement them (Workstreams C and D), not ahead
   of it. The validator was not weakened.
4. **Cryptographic core: `packages/messaging-crypto` (`cf-messaging-crypto`).** A shared
   package, independent of every service:
   * public half (default features, used by the backend): `key_package`
     (RFC 9420 validation of published KeyPackages), `binding` (canonical device
     credential and registration formats), `profile`, `transparency` (RFC 9162 Merkle
     tree, proofs and verification);
   * `client` feature: `device` — Ed25519 MLS signature keys and KeyPackage generation.
     Private keys exist only in the client process. The backend must never enable it
     (services/messaging architecture test, Workstream C);
   * `web` feature: the same for `wasm32-unknown-unknown` (OpenMLS `js`, getrandom
     `js`).
   No primitive is implemented here; there is no custom cipher or signature scheme.
5. **Library, versions and licence (pinned exactly).** `openmls =0.9.1` (MIT, released
   2026-10-07, a patch of 0.9.0 released 2026-08-25; MSRV 1.91), `openmls_rust_crypto
   =0.6.0`, `openmls_basic_credential =0.6.0` (MIT). Transitive licences are permissive
   (MIT / Apache-2.0 / BSD / ISC / Unicode) except `hpke-rs*` 0.7 (**MPL-2.0**, file-level
   copyleft; acceptable for unmodified linking, recorded for legal review). Every bump is
   a reviewed change. RustSec advisories are gated for this lockfile in
   `dependency-security.yml`. `hpke-rs 0.7.0` pins its optional libcrux backend to
   `libcrux-kem 0.0.9` (RUSTSEC-2026-0330 / -0331): the crate is in the lockfile but in no
   build graph (the RustCrypto backend is used; CI proves `cargo tree -i libcrux-kem`
   is empty for all features and targets), so those two advisories are ignored for this
   lockfile only, until upstream releases a fixed pin.
6. **Validation rules the backend applies** (`validate_key_package`): exactly one
   `MLSMessage` of wire format `mls_key_package` (≤ 16 KiB, exact TLS decoding);
   OpenMLS `KeyPackageIn::validate` (MLS 1.0, leaf-node signature, KeyPackage signature,
   init ≠ encryption key, extension support, lifetime valid now); the accepted cipher
   suite only; our own lifetime policy on the server clock (OpenMLS does not bound the
   total range on validation: ≤ 90 days + 1 h, ≤ 5 min future skew, ≥ 1 h remaining);
   a `BasicCredential` whose identity is exactly `cfmeet-messaging-device:v1:<device id>`
   and a 32-byte Ed25519 signature key. Results carry public facts only, including the
   RFC 9420 KeyPackageRef used for replay detection.
7. **Proof of possession without a custom signature.** Registration answers a
   single-use server challenge with a *registration KeyPackage* whose leaf node carries
   the RFC 9420 `application_id` extension
   `cfmeet-messaging-registration:v1:<challenge id>:<nonce hex>`. The leaf-node signature
   (verified by OpenMLS) covers it, so a valid registration KeyPackage proves both
   possession of the device signature key and freshness, using only MLS-defined signed
   structures (no cross-protocol reuse of the device key). The device id is
   server-assigned with the challenge.
8. **Key transparency (structure only).** The `transparency` module implements the
   RFC 9162 §2.1 Merkle tree: leaf / node hashing, tree heads, inclusion and consistency
   proofs generated from stored perfect subtrees (O(log n) nodes per append, O(log² n)
   reads per proof) and the RFC 9162 verification algorithms for clients and auditors.
   It is tested against a reference implementation, including detection of rewritten
   history. **It is not an anti-equivocation system**: without independent witnesses or
   gossip a malicious operator could present different views. That is an open decision
   and nothing claims production-certified key transparency.

## Platform evidence

| Target | Evidence | Status |
|---|---|---|
| x86_64-unknown-linux-gnu | `cargo test --all-features` (real KeyPackages generated and validated, tamper and lifetime tests) | tested |
| aarch64-linux-android | `cargo check --features client --target aarch64-linux-android` | compiles; not linked or run on a device — **not certified** |
| aarch64-apple-ios | `cargo check --features client --target aarch64-apple-ios` | compiles; not linked or run on a device — **not certified** |
| wasm32-unknown-unknown | `cargo check --features web --target wasm32-unknown-unknown` | compiles; not run in a browser — **not certified** |

Upstream OpenMLS tests only x86_64 / aarch64 Linux, Windows and macOS on CI and builds
(without testing) Android, iOS and wasm targets.

## Open decisions (owner approval required)

* Final approval of OpenMLS 0.9.x / the RustCrypto provider and the single cipher suite
  (the specification's proposal; no post-quantum confidentiality is claimed — MLS 1.0
  provides none — and no Signal Protocol interoperability is claimed).
* The anti-equivocation architecture for key transparency (independent witnesses /
  auditors, signed tree heads, gossip).
* Legal review of the MPL-2.0 `hpke-rs` dependency; tracking the upstream `hpke-rs` release
  that lifts the `libcrux-kem 0.0.9` pin.
* External cryptographic review of the registration-binding composition (decision 7).

Until approved, device registration and KeyPackage publication are disabled by default
in staging and production (`MESSAGING_E2EE_DEVICES_ENABLED`, ADR 0047).

## Preserved

The Meeting Service's chat (`conference_messages` behind its replaceable `MessageStore`,
and legacy `chat_messages`) is server-processed, unchanged, and migrates only in
Workstreams P / Q.
