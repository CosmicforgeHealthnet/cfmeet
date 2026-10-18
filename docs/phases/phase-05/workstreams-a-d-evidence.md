# Part V Workstreams A–D — implementation evidence register

Branch `feature/part-v-a-d-secure-messaging-foundation`, based on `staging` after its
reconciliation with `main` (689728c). Decisions: ADR 0045–0048. The Part V specification
document itself was not available in the implementation session; requirements were taken
from the implementation directive that restates it, and every AD-01…AD-10 owner decision
is treated as not yet approved.

| Gate | Command (local, PostgreSQL 16, no Docker) | Result |
|---|---|---|
| Crypto core | `cd packages/messaging-crypto && cargo test --all-features` | 9 unit + 6 OpenMLS integration tests pass (real KeyPackages; registration binding; tampering; lifetimes; two-device MLS group with AAD authenticated and a rewritten AAD rejected) |
| Crypto targets | `cargo check --features client --target aarch64-linux-android / aarch64-apple-ios`, `--features web --target wasm32-unknown-unknown` | compile only — **not certified** on devices / browsers |
| Service auth | `cd packages/service-auth && cargo test --all-features` | pass (Messaging identity; grants pinned) |
| Platform Access | `cd services/platform-access && cargo test --workspace` | pass, incl. `messaging_scopes` (8) |
| Messaging | `make messaging-test` (with `MESSAGING_E2E_REQUIRED=1`) | 24 unit + 50 integration tests pass: architecture 4, conversations 15, devices 11, foundation 7, migrations 3, outbox 3, startup 3 (real binary), transparency 3, **platform_access_e2e 1** |
| Foundation certification (§10) | `tests/platform_access_e2e.rs` | real Platform Access + real Messaging processes, own databases: explicit scopes, real credential, principals, device registration with proof of possession, PUBLIC material only, E2EE DIRECT conversation, cross-tenant 404, unscoped 403, profile change 409, concurrent dedup, single-winner versions, outbox events, principal suspension 403, credential revocation (fresh for mutations; cached reads flushed via the change feed), restart and `backup.sh` / `restore.sh` recovery, no secret in either database |
| Contracts | `cd packages/contracts/validator && cargo run -- ../ ../../..` | 52 contracts OK (`cfmeet.messaging.common` / `http-api` / `event` 1.1.0, `cfmeet.platform.common` 1.9.1) |
| Boundaries | `python3 -m unittest discover -s scripts/service-boundaries`, `check_service_boundaries.py`, `check_operations_registry.py` | pass (new Messaging rules mutation-tested; 3 Messaging durable responsibilities) |
| Secrets | `python3 scripts/secret-scan/scan_secrets.py` | clean |
| DB isolation | `MESSAGING_MIGRATION_BIN=… infra/deployment/postgres/verify-service-isolation.sh` | OK (runtime role has no DDL; no cross-service connection) |
| RustSec | `cargo audit` (crypto core, service) | clean except RUSTSEC-2026-0330/0331 on `libcrux-kem 0.0.9`, present in the lockfile but in no build graph (CI-proven; ADR 0045) |
| Regression | SMS Gateway full suite; Numbering lib / service / contracts / routing grants; Calling lib / http_contract / architecture / authentication / scopes | pass |
| Docker image | `docker build -f services/messaging/Dockerfile .` | **BLOCKED locally** (Docker Hub 429 rate limit pulling base images); verified by the `messaging.yml` docker job in CI |

Not certified here: end-to-end encrypted send / decrypt / reconnect (after Workstream I),
independently witnessed key transparency, mobile / Web clients, external security review.

## Remediation round — 18 October 2026

### CI failures on `bfc0bd5` and their fixes (commit `2f98f55`)

| Workflow (run on `bfc0bd5`) | Root cause | Fix | Regression coverage |
|---|---|---|---|
| Contracts and service auth ([run 39110802279](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39110802279)) — `an_unmodified_copy_is_valid` | The validator test sandbox listed the deployable services that existed before Part V and omitted `services/messaging`, so the registered `cfmeet.messaging.*` 1.1.0 contracts were rejected. The validator was right; the fixture was stale. | `packages/contracts/validator/tests/validator.rs`: the sandbox includes `messaging`. Ownership rules unchanged. | `contracts_for_unimplemented_services_are_rejected` now uses a service that does not exist (`directory`); new `messaging_contracts_require_the_messaging_service` removes `services/messaging` and asserts the real contracts are rejected. 18/18 validator tests; 52 contracts OK. |
| Messaging ([run 39110802342](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39110802342)) — "the backend never links client key generation" | The step grepped an inverse `cargo tree -i openmls_basic_credential` for `did not match`; current Cargo prints `warning: nothing to print` when the crate is absent, so a correct graph failed. A command failure would also have been indistinguishable from "absent". | `scripts/messaging/check-backend-crypto-graph.sh` reads the full resolved graph (`-e normal --target all`, per-package features). Violation = `openmls_basic_credential` or the `client` / `web` feature on `cf-messaging-crypto`. Exit 2 when cargo fails or the graph has no `openmls`. The backend is checked with default, `--all-features` and `--no-default-features`; the crypto core with default (absent) and `--features client` (present). | `scripts/messaging/test-check-backend-crypto-graph.sh` (runs in CI): allowed graphs pass; `client` / `web` features and a deliberately prohibited backend crate (crypto core with `client` as a normal dependency) fail; a missing directory, an unknown feature and a graph without OpenMLS exit 2. 10/10 locally. |

The guard was not disabled, made informational or relaxed.

### Specification compliance — Version 2.0 review (18 October 2026)

The 60-page specification (*Part V Version 2.0, 18 October 2026*) was supplied in the
remediation session and read in full. Status: **PASS** = implemented and tested;
**CORRECTED** = a discrepancy found by this review, fixed and tested in this round;
**PARTIAL** = the A–D part is done, the rest belongs to a later workstream or an external
gate; **PENDING** = owner / external decision. Page numbers are the PDF's. Security
records: `docs/architecture/messaging-security-architecture.md`.

#### Defects found and corrected

| # | Requirement | Found | Correction | Evidence |
|---|---|---|---|---|
| 1 | H.1 (p. 50): AAD must bind tenant / application / environment, conversation, message or client message id, sender device, security profile and protocol version | AAD v1 bound only conversation, client message id and device | AAD v2 binds profile, protocol, suite, tenant, application, environment, conversation, client message id, device; the acceptance record (v2) binds the same scope and profile; ADR 0048 revised. No v1 value had ever been persisted | `binding::tests` (every field changes the AAD; foreign profile / protocol / suite / version rejected); real two-device OpenMLS group: tenant transplant, device re-attribution and profile relabel all rejected by the recipient; `sequence::tests` |
| 2 | Appendix G (p. 44): PRIVATE_E2EE is the default for "transactional / consultation / support contexts unless tenant policy requires managed mode" | SUPPORT had no allowed or default profile (an earlier proposal) | Platform policy version 2: every context defaults to PRIVATE_E2EE; MANAGED stays opt-in per application and is never a default by itself (a MANAGED-only context has no default) | `policy::tests`; `conversations::every_context_class_follows_its_persisted_policy` |
| 3 | G.5 (p. 49): "clients cache signed tree heads"; §142: KT signing keys are server secrets | Tree heads were unsigned | Ed25519-signed heads over a length-prefixed (log id, size, root, timestamp) encoding with a dedicated key file (`MESSAGING_KT_SIGNING_KEY_FILE`, required; inline refused); 503 instead of an unsigned head; contracts 1.2.0 | RFC 8032 vectors; `transparency` tests (pinned-key verification, forged root / size / log / time / key rejected, fail-closed without key); startup refusals; real-process certification verifies a signed head |
| 4 | §144 (p. 31): CI must add protocol test vectors and dependency / licence review | Neither existed | RFC 9420 published vectors (pinned commit, SHA-256-checked download, required in CI); licence allowlist on the resolved graph in CI | `rfc9420_vectors` (300 cases × 7 MLSMessage encodings round-trip; suite 0x0001 SignWithLabel, RefHash, ExpandWithLabel, DeriveSecret, DeriveTreeSecret, EncryptWithLabel); `check_licenses.py` (156 / 379 packages) and its unit tests |
| 5 | §145 (p. 31): guards must fail on provider identifiers in public contracts and prevent managed-content modules entering E2EE storage / delivery paths | Not enforced | Architecture tests `no_provider_identifiers_in_the_public_contracts`, `managed_content_never_enters_e2ee_paths` | `messaging` architecture suite |
| 6 | §28 (p. 11): policy snapshot covers threads and reactions | Missing | `messages` (replies, threads, reactions, pins — null until Workstream G) in the effective policy (platform policy version 2) | `policy::tests`, contract 1.2.0 |
| 7 | Workstream A (p. 36) and §165 (p. 39): threat model, protocol / version registry, downgrade rules recorded | Only partly, inside ADR 0045 | `docs/architecture/messaging-security-architecture.md`; ADR 0045 revised | document review |

Not a defect, recorded: A forged message injected before the genuine one makes OpenMLS
consume that generation's key, so the genuine copy fails (denial of service; G.2 accepts
service denial by a compromised server). The profile is fixed at creation, stricter than
G's "immutable when the first protected application message is committed".

#### Requirement matrix — Workstreams A–D

| Requirement | Source | Implementation | Test evidence | Status | Owner decision |
|---|---|---|---|---|---|
| Part V branches from a main-equivalent baseline | §3 p. 3; WS A p. 36 | `staging` was reconciled with `main` 689728c (merge 6e4b70b) before branching, on the owner's explicit instruction to target staging (the specification names main because staging was then stale) | ancestry: `main` ⊂ `staging` | PASS (owner instruction recorded) | — |
| Security decisions, threat model, two profiles, protocol / version registry, downgrade rules | WS A p. 36; G p. 44–49 | security architecture doc; ADR 0045 / 0048 | review | CORRECTED (#7) | AD-01…AD-10 PENDING |
| Crypto dependency / licence review | WS A p. 36; §144 p. 31 | ADR 0045 §5; `check_licenses.py` in CI | licence tests | CORRECTED (#4) | `hpke-rs` MPL-2.0 legal review PENDING |
| `cfmeet.messaging` identity; Messaging permissions least-privilege | §11 p. 7; §17 p. 8; §20 p. 9 | `packages/service-auth` | `messaging_flows_are_least_privilege` | PASS | — |
| Service-boundary guard extensions | §145 p. 31 | `check_service_boundaries.py` (4 Messaging rules), architecture tests | 77 guard tests; architecture suite | CORRECTED (#5) | — |
| No custom crypto; MLS 1.0 + suite 0x0001 only; fail closed on others | G.3 p. 47 | `packages/messaging-crypto` (OpenMLS 0.9.1) | KeyPackage validator tests; RFC 9420 vectors | PASS | AD-01 / AD-02 PENDING |
| No libsignal, no PQ claim, no parity claim | AD-03 / AD-04 / AD-10; §9 | no such dependency or text | licence check rejects AGPL; RustSec | PASS | PENDING |
| Messaging scopes (nine; `messaging.admin` audited privileged grant) | §14 p. 7 | `services/platform-access/src/identity/scopes.rs` | `messaging_scopes` (8) | PASS | — |
| Platform Access stays the authority; body tenant ids never authority; no hard-coded first-party names | §13–16 p. 7–8 | credential authenticate / principal resolve / change feed | PA + Messaging suites, certification step 9 | PASS | — |
| Trusted device association, delegated principal resolution | WS B p. 36 | device registration under a resolved ACTIVE principal | `devices`, certification step 5 | PASS | — |
| Messaging Client Access Tokens | §18 p. 8 | — | — | Later (realtime I / SDK S); not a WS B outcome | — |
| Peer service: own Rust/Axum code, migrations, DB, config, CI, artifact | §11 p. 7; WS C p. 36 | `services/messaging` | build, migration smoke, Docker job | PASS | — |
| Own DB URL, runtime and migration roles, isolation proof | §123 p. 28 | infra scripts | `verify-service-isolation.sh` (CI) | PASS | — |
| Client / server crypto interfaces | WS C p. 36; App. L p. 55 | public half (backend) and `client` / `web` features | CI graph guard, two-device test | PARTIAL — Web / Android / iOS wrappers, attachment / backup formats, verification codes are later | — |
| Ciphertext-only E2EE storage boundary | §124 p. 28; WS C | `OpaqueCiphertext`, `E2EE_SECRET_IN_SCHEMA`, column scans | architecture, migration smoke, certification step 15 | PASS (no message store yet: F) | — |
| Health / readiness / service-auth | WS C | `/health`, `/ready`, `/internal/v1` service tokens | `foundation`, `startup` | PASS | — |
| CI contents | §144 p. 31 | `messaging.yml` | jobs on the final commit | PARTIAL — vectors, licence, WASM / native compile, KT proofs, migration smoke, contract parity, boundaries, image scan present; fuzz / property tests and secure-backup vectors not yet (X / Y, backups) | — |
| Device registration: fresh authenticated session, proof of possession | G.4 p. 48 | challenge + registration KeyPackage (leaf `application_id` binding) | `devices` (replay, expiry, foreign device, tamper) | PASS | Independent review of the PoP composition PENDING (AD-10) |
| KeyPackages: bounded expiry / usage, replayed / consumed rejected, depletion tracked | G.4 p. 48 | unique KeyPackageRef, lifetime policy, per-device cap, `SKIP LOCKED` claims | `devices` | PASS — depletion alerts are W | — |
| Revocation blocks API use, revokes KeyPackages | G.4 p. 49 | terminal revoke | `devices`, certification | PASS — MLS leaf removal / epoch advance is E | — |
| Key changes produce transparency entries | G.4 p. 49 | leaf on register and revoke | `transparency` | PASS | — |
| KT: monotonic position, inclusion / consistency proofs, signed heads, no contact identifiers | G.5 p. 49 | RFC 9162 log; signed heads | `transparency`, certification | CORRECTED (#3) | — |
| KT: witnesses / anti-equivocation; pinned KEYTRANS draft | G.5 p. 49; AD-05 p. 45; N.1 p. 57 | reported `UNWITNESSED`; not aligned to a draft yet | — | PENDING | AD-05 PENDING |
| H.4 endpoints present in A–D | H.4 p. 52 | devices, key-packages, principal devices, KT proofs, conversations | contract parity test | PASS (membership-commits, messages, read, attachments, reports, backups are later) | — |
| Kill switches | §134 p. 29 | E2EE device switch only | config tests | PARTIAL — independent switches for E2EE conversation creation, managed messaging, KT publication are W | — |
| Local development without Docker | §143 p. 31 | PostgreSQL only | all suites run without Docker | PASS | — |
| Conversation aggregate, UUID identity, opaque external reference | §23–24 p. 10 | `messaging_conversations` | `conversations` | PASS | — |
| Eight context classes, product-neutral | §25–26 p. 10; §102 p. 23 | `ContextClass` | `conversations`; `PRODUCT_DOMAIN_IN_MESSAGING` | PASS | — |
| Lifecycle (six states), server-authoritative, versioned, concurrency-safe | §27 p. 10–11 | API + DB trigger + version trigger | `conversations` (races single winner) | PASS | — |
| Policy snapshot | §28 p. 11 | content-addressed immutable snapshots | `policy`, `conversations` | CORRECTED (#6) | — |
| Direct: two principals, idempotent get-or-create | §29 p. 11 | partial unique index, idempotency keys | concurrent dedup tests, certification step 12 | PASS | — |
| Expiry separate from retention | §38 p. 12 | `expires_at`, expiry worker | `conversations` | PASS | — |
| Immutable `security_profile` + protocol version, no silent downgrade | WS D p. 36; G p. 44 | CHECK + trigger + API | `conversations`, certification step 11 | PASS | — |
| PRIVATE_E2EE default for eligible contexts, MANAGED explicit | G p. 44; §160 p. 34 | platform policy v2 | `policy`, `conversations` | CORRECTED (#2) | — |
| Failed encryption initialization never falls back | G p. 44; App. N p. 57 | `E2EE_NOT_AVAILABLE`, nothing written | `conversations` | PASS | — |
| Sequence foundation; unique, gap-free per transaction | §42 p. 13; §126 p. 28 | `allocate_sequence` | concurrency tests | PASS | — |
| E2EE binding of client / server fields | H.1 p. 50 | AAD v2 + acceptance record v2 | MLS two-device test | CORRECTED (#1) | — |
| Transactional outbox: same transaction, leases, retry, dead-letter | §119 p. 27 | `events/outbox.rs` | `outbox` | PASS — no consumer yet (realtime / webhooks later) | — |
| Cross-tenant / environment IDOR | §149 p. 32 | scoped queries | `conversations`, certification step 9 | PASS for A–D resources | — |
| Membership, roles, MLS commits | §35–37 p. 11–12; WS E p. 36 | owner / counterpart association only | — | Later (E) | — |

#### Appendix N gates (cryptographic certification) — A–D share

| Gate (p. 56–57) | A–D evidence | Status |
|---|---|---|
| Protocol conformance | RFC 9420 vectors (messages, crypto-basics 0x0001) on native Rust; SDK wrappers absent | PARTIAL |
| Encryption / integrity | AAD tamper rejected in a real MLS group; KeyPackage tamper rejected | PARTIAL (ciphertext / header tamper with the send path: F) |
| Replay | registration replay, KeyPackage replay rejected | PARTIAL (message replay: F) |
| Forward secrecy / post-compromise security | — | Later (E / Y) |
| Device lifecycle | register / revoke / KeyPackage limits | PARTIAL (link, lost device, offline: E / S) |
| Group membership | — | Later (E) |
| Key transparency | proofs vs a reference implementation, key substitution detected by inclusion proof, signed heads | PARTIAL (split-view simulation, cached-head monitoring: clients; witnesses: AD-05) |
| Attachments / backups / Web security | — | Later (K, L, backups, X) |
| Downgrade | API, CHECK and trigger; no fallback | PASS for conversations |
| Leak scan | both databases scanned in the certification; log / content architecture tests | PARTIAL (traces / crash dumps: W / X) |
| Independent assessment | not performed | PENDING (AD-10) |

### Approval register (all PENDING owner decision)

| Decision | Implementation candidate | Known limitation | Status |
|---|---|---|---|
| MLS / OpenMLS adoption | MLS 1.0 (RFC 9420) via OpenMLS `=0.9.1`, `openmls_rust_crypto =0.6.0` | Pre-1.0 library; every bump is a reviewed change | PENDING |
| Cipher suite | `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` only; others rejected at KeyPackage validation | Classical only | PENDING |
| Signal / libsignal exclusion | Not used; no Signal-protocol or interoperability claim | — | PENDING |
| Post-quantum policy | None implemented; no post-quantum claim | `libcrux-kem 0.0.9` is in the lockfile but in no build graph (RUSTSEC-2026-0330/0331, scoped ignore, CI-proven) | PENDING |
| Key transparency and witnesses | RFC 9162 Merkle log, inclusion / consistency proofs, reported `UNWITNESSED` | No signed tree heads by an independent witness; equivocation is not detectable by clients yet | PENDING |
| Web security limitations | `web` feature compile-checked for wasm32 | Not run in a browser; no Web key-storage design certified | PENDING |
| History on new devices | Not implemented (Workstream E+) | — | PENDING |
| Encrypted attachments | Not implemented | — | PENDING |
| SMS downgrade rules | No downgrade path exists; PRIVATE_E2EE cannot become MANAGED (API, CHECK, trigger) | Rules for later fallbacks undefined | PENDING |
| Independent cryptographic assessment | Not performed | Registration proof-of-possession composition is unreviewed | PENDING (external) |
| `hpke-rs` MPL-2.0 | Transitive via `openmls_rust_crypto`, unmodified | File-level copyleft obligations need legal confirmation | PENDING (legal) |
| Mobile / browser runtime certification | Android / iOS / wasm compile-only | Not run on devices | PENDING |

E2EE device operations stay disabled by default outside local / test
(`MESSAGING_E2EE_DEVICES_ENABLED`). Nothing here enables production E2EE.

### Status classification

| Classification | A–D |
|---|---|
| SOFTWARE COMPLETE | Yes for the A–D scope of the Version 2.0 specification after the seven corrections above (later-workstream items listed as such) |
| INTEGRATION READY | Staging only, once PR 9 is merged; the service is not deployed anywhere |
| EXTERNAL SECURITY CERTIFICATION PENDING | Yes — no independent assessment |
| PRODUCTION APPROVAL PENDING | Yes — every decision above |

### Branch administration

Verified on 18 October 2026: the eight historical branches
(`backup/pre-platform-layout` 300383b, `ccr-3ee568f2-bpv1kl` 380e693,
`ccr-c5e05129-xhhmmk` b10d112, `chore/auto-deploy-meeting-service-main` e2ad624,
`chore/platform-monorepo-layout` e8ee532, `chore/rename-meeting-service` 727febd,
`claude/peaceful-curie-7yqa4g` 04ab2eb, `fix/meeting-deploy-ssh-build-keepalive` 2281aeb)
each have zero commits not reachable from `staging` (6e4b70b), no open pull request
(PRs 1, 2, 6, 7, 8 are closed) and no reference in any workflow or script. Deletion was
attempted and **refused by the session's Git proxy (HTTP 403)**; they still exist.
No branch is protected and no ruleset exists. `deploy-backend.yml` deploys only on a push
to `main` (or a manual dispatch); a push to `staging` deploys nothing.

### GitHub Actions on the final implementation commit `b8f3f40`

`b8f3f40` adds one more fix: Calling's `repeated_loop_blocks_are_one_incident` failed at
random on `91658c0` ([run 39116719995](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39116719995))
because a random destination UUID contained "650", the substring the test uses to detect a
leaked extension number. The check now removes the incident's own ids first. Pre-existing,
unrelated to Part V; a real leak still fails it.

All 16 workflows pass on `b8f3f40`:
Messaging ([39118281447](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281447): crypto core, service lint and graph guard, PostgreSQL tests with real Platform Access incl. the §10 certification, build / migration smoke / isolation, **Docker image**),
Contracts and service auth ([39118281479](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281479)),
Service boundaries ([39118281337](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281337)),
Dependency and image security ([39118281424](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281424)),
Platform Access ([39118281491](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281491)),
Calling ([39118281381](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281381)),
Calling RTC certification ([39118281362](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281362)),
Enterprise calling certification ([39118281341](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281341)),
Communications end-to-end journeys ([39118281368](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281368)),
Numbering ([39118281347](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281347)),
SMS Gateway ([39118281392](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281392)),
Privacy Relay ([39118281363](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281363)),
Telephony ([39118281340](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281340)),
Telephony SIP certification ([39118281483](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281483)),
Compose config ([39118281376](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281376)),
Deploy Backend ([39118281403](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39118281403): pre-deploy tests pass, "Deploy to VPS" **skipped** — nothing deployed).

The Docker-image blocker of the first session is resolved by CI (the local build was only
rate-limited). Known, not fixed here: Calling's `a_stuck_queue_entry_is_an_incident_and_drains_later`
reads the process-global `calling_enterprise_queue_depth` gauge and can fail when run in
parallel on a small machine (2 of 3 local runs; green in every CI run).

### PR 9 status

Open, not merged. Merge gates not met: the Version 2.0 specification review (document
unavailable) and an approving review (none; the only GitHub identity available to the
implementation session is the PR author). The feature branch is retained.
