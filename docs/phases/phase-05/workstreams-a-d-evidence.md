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

### Specification compliance — NOT PERFORMED

The 60-page Part V Version 2.0 document was **not available** in the remediation session
either (no file attached, none in the repository). The requirement-by-requirement audit
against Workstreams A–D, Appendices G, H, L and N, and the exact wording of AD-01…AD-10,
has therefore **not** been carried out. Nothing in this register claims compliance with
that document. The implementation remains checked only against the directive that
restates it (table above, ADR 0045–0048).

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
| SOFTWARE COMPLETE | Yes, against the directive and ADR 0045–0048 (not against the unseen specification) |
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
