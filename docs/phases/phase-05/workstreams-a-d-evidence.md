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
