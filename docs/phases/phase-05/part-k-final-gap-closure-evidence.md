# Part K — final engineering gap closure and production readiness

Date: 2026-10-20. Branch: `staging`. Scope: the gaps behind the PARTIAL Part K gates (6
encrypted-media processing / derivative quotas, 16 capacity, 25 web XSS / CSP / Trusted
Types), the real web Messaging client, hosted-staging preparation, external-certification
readiness, regression, Swagger, migrations, security scans and branch protection.

Nothing in this work deployed, migrated or reconfigured any production or hosted system:
no push to `main`, no production deployment, database migration, container replacement,
LiveKit / Nginx / DNS change, credential rotation, storage deletion, E2EE activation or
paid telecom operation. Hosted staging was prepared, **not** provisioned (not authorized).

Raw evidence: `docs/phases/phase-05/evidence/` —
`web-messaging-journey-2026-10-20.txt`, `attachments-derivative-quota-2026-10-20.txt`,
`staging-kit-rehearsal-2026-10-20.md`, `swagger-verification-2026-10-20.txt`,
`security-scans-2026-10-20.txt` (and the earlier `load-measurements-2026-10-20.txt`,
`authorization-matrix.txt`).

---

## A. Repository state

| Item | Value |
|---|---|
| Starting `staging` | `5dad041` |
| `main` (unchanged by this work) | `4f8998a` — fully contained in `staging` (merged in `85e5eda`; `git log staging..origin/main` is empty) |
| Final code commit | `8999ac6` (this report is committed on top; see §G) |
| Pushes | `staging` only, fast-forward, no force-push |
| Operational branches | `ops/*`, `audit/*`, `diagnostics/*` left untouched |

Commits of this work:

| Commit | Change |
|---|---|
| `85e5eda` | merge `main` (production ops audit tooling) into `staging` |
| `9efdf4c` | Gate 6: derivative storage reserved before writing; held objects stay charged; `QUOTA_DEFERRED`; drift audit |
| `eb27991` | Meeting Service client Messaging API (relay allowlist, `/me`, directory); service-auth `Idempotency-Key` forwarding; durable device state in the crypto core |
| `ef95ecd` | real end-to-end encrypted Messaging in the CF Meet web app (wasm crypto core in a Worker, sealed IndexedDB, CSP / Trusted Types), Chromium journey across real services, CI job |
| `654d117` | CI fixes: v2 operation count, relay PUT / DELETE coverage, boundary-guard finding in a test literal |
| `3f392fa` | hosted-staging capacity certification kit (prepared, not run) |
| `9c3dbf8` | defect fixes found by the journey: MLS join configuration, KeyPackage waste, logout race |
| `bedbaa7` | RUSTSEC-2026-0382 / -0381 in every Rust lockfile; audit matrix covers the new crypto lockfiles |
| `8999ac6` | Next.js 16.4.2 (critical request smuggling and nine other production advisories) |

**Deployment triggers inspected** (every workflow that can deploy, cut over or publish):

| Workflow | Can it run from `staging`? |
|---|---|
| `deploy-backend.yml` | no: push to `main` only (or manual dispatch); its `deploy` job runs only on push / dispatch |
| `production-v2-cutover.yml` | no: push to `main` (paths-filtered) or manual dispatch |
| `mobile.yml` | EAS preview / production / store submit / OTA jobs are guarded by `github.ref == 'refs/heads/main'` |
| Vercel (`apps/web`, docs/VERCEL.md) | production = `main`; a `staging` push can at most create a Vercel **preview** deployment, never `cfmeet.com` |
| `staging-capacity-certification.yml` (new) | manual dispatch from `staging` only, disabled until an admin sets `STAGING_CERTIFICATION_ENABLED=true`; deploys nothing |

**Branch protection** (GitHub API, 2026-10-20): `main` `protected: false`, `staging`
`protected: false`. Not changed (administration not authorized). Recommended rules for a
repository admin (Settings → Rules → Rulesets):

1. `main`: require a pull request with ≥ 1 approving review from a code owner; dismiss stale
   approvals; require the status checks Frontend, Meeting service, Messaging, Unified
   Messaging integration journeys, Web crypto artifact, Dependency and image security,
   Service boundaries, Contracts and service auth, Platform Access service, Calling
   service, SMS Gateway service, Numbering service, Telephony service, Privacy Relay service;
   require branches up to date; block force pushes and deletion; restrict who can push.
2. `staging`: require the same status checks; block force pushes and deletion.
3. GitHub environments with required reviewers: `production` for `deploy-backend.yml` /
   `production-v2-cutover.yml`, and `hosted-staging` before enabling the capacity workflow.

---

## B. Encrypted-media processing and derivative quotas (gate 6)

ADR 0054 Amendment 1; migration `m20261121_000019_derivative_reservations`; code in
`services/messaging/src/attachments/{derivatives,lifecycle,quota}.rs`.

| Requirement | Implementation | Evidence |
|---|---|---|
| Derivative bytes count against the environment's storage budget | READY thumbnail bytes are committed bytes (`messaging_derivative_storage_account()` trigger) | `attachments.rs` (real PostgreSQL + MinIO): `real_object_storage_thumbnail_accounting` |
| No write beyond the budget | each attempt **reserves** its size under row locks (derivative row, then usage row) before writing; over budget → `QUOTA_DEFERRED`, nothing written | 8 racing workers + a second environment: reservations never exceed the budget |
| Crash / failed write safety | per-attempt object keys `drv/{env}/thumbnail/{att}/{attempt}.png`; a superseded or failed attempt's object is enqueued for deletion with its reservation as charge | crash between upload and ack; injected write failures |
| Held / pending deletions stay charged | `messaging_object_deletions.charged_bytes`; released only when the object is really deleted (`DONE`), kept while `PENDING` / `HELD` / `DEAD` | legal hold, retention purge, replacement tests |
| Deferred work resumes | `requeue_quota_deferred` on budget changes (policy update) and on retry | deferral → policy raise → READY |
| Drift detection | `audit_all` every 60 lifecycle ticks; gauge `messaging_storage_accounting_drift_environments`; `reconcile` repair | quota audit tests |
| Orphans | `reconcile_derivative_orphans` for unreferenced `drv/` objects | orphan test |
| PRIVATE_E2EE never processed server-side | unchanged: gate, query filter and database trigger; E2EE attachments are nameless `application/octet-stream` ciphertext | journey step 8 (server evidence `e2ee_attachments=1`, nameless) |
| Transcoding | not required by any policy, **not built and not claimed** (not invented) | ADR 0054 |

Result: 22 / 22 attachment tests pass (`evidence/attachments-derivative-quota-2026-10-20.txt`),
contract `cfmeet.messaging.common` 1.14.0 adds `QUOTA_DEFERRED`.

**Migration 19 safety.** Up: `ADD COLUMN … NOT NULL DEFAULT 0` (metadata-only on
PostgreSQL ≥ 11), nullable `pending_object_key`, CHECK constraints (validated by a table
scan under the migration's lock — proportional to the derivative / deletion row counts),
status-constraint swap, a non-concurrent rebuild of `ix_messaging_attachment_derivatives_due`
(blocks writes to that table while it builds), replaced trigger functions and one new
trigger. No data is rewritten. Down: releases what open deletions and reservations still
charge, restores migration 18's function, index and constraint, and drops the new columns;
tested by the up / down / up backfill test in `attachments.rs` and the CI migration smoke.
Caveat for rollback: objects of derivative attempts still in flight are no longer
referenced after `down`; they are found by the orphan reconciler of the newer release, not
by the older one — reconcile before rolling the image back. Run it in a maintenance window
on large deployments; take the backup first (`infra/deployment/backup/backup.sh`).

---

## C. Web Messaging in the actual CF Meet web app

Route `/dashboard/messages` of `apps/web` (sidebar "Messages"), the shipped production
build, against the real Meeting Service, Platform Access and Messaging binaries.

| Element | Implementation |
|---|---|
| Crypto | `packages/messaging-crypto/wasm` (OpenMLS 0.9.1) compiled to WebAssembly, reproducibly (rustc 1.97.0, wasm-bindgen 0.2.129, canonical path) and checked in CI (`web-crypto-artifact.yml`); runs only in a dedicated Worker that verifies the module's SHA-384 before compiling it |
| Keys at rest | IndexedDB sealed with AES-256-GCM under a **non-extractable** key; record AAD binds account / conversation / sequence; one tab per device (Web Locks) |
| Device registration | proof of possession through Meeting's verified-login bridge; the browser never holds a Messaging or Platform Access credential |
| Messaging API | Meeting relay `/api/v2/messaging/relay/v1/**` — explicit allowlist, principal signed into the service token, never taken from the request |
| Realtime | device-bound client token → single-use ticket → WebSocket; periodic catch-up |
| Attachments | encrypted in the Worker before upload (XChaCha20-Poly1305 stream), descriptor inside the MLS message |
| Multi-device and history | new devices added by a member device; device-to-device history transfer with a comparison code (HPKE, no server key) |
| Logout | device revoked, all local keys and records erased |

Journey (`unified_messaging_e2e.rs::web_messaging_client_in_chromium_across_real_services`,
CI job "Web Messaging client in Chromium (real services)"): 15 steps — registration,
encrypted conversation, send / receive, XSS payloads inert, reload from sealed storage (0
plaintext hits, wrapping key non-extractable), offline delivery in order, reconnect,
encrypted attachment, MANAGED conversation labelled as server-readable, second device +
history transfer, device removal erases state, CSP + Trusted Types, tampered crypto module
refused, expired session, logout erases the device. Server-side evidence:
`e2ee_messages=16 plaintext_hits=0 e2ee_attachments=1 revoked_devices=2`.

**Defects found and fixed by the journey** (`9c3dbf8`):

1. `Device::join` used OpenMLS's default join configuration, which omits the ratchet tree
   from Welcomes the member sends later. Any device added by a member that had itself
   joined by Welcome was refused (`WELCOME_REFUSED`) and stranded; the journey passed only
   when the group's creator happened to be the designated adder (4 of 6 runs failed
   before the fix). Fixed in the shared crypto core; Rust regression test
   `tests/multi_device_welcome.rs`.
2. The adder claimed (consumed) the newcomer's KeyPackages before checking key
   transparency; failed attempts drained its stock. Verification now precedes any claim,
   and a waiting device tops up its stock.
3. Logout revoked the device while polling was still running; a poll could detect the
   revocation, wipe and reopen the store, leaving a fresh wrapping key after logout.
   Background work now stops first.

After the fixes: 6 / 6, then 3 / 3 on Next.js 16.4.2, consecutive local passes
(`evidence/web-messaging-journey-2026-10-20.txt`); CI links in §G.

Not covered: the mobile app (`apps/mobile`) has no Messaging client yet; browsers other
than Chromium were not run.

---

## D. Web security — XSS, CSP, Trusted Types, integrity (gate 25)

| Control | State | Verified by |
|---|---|---|
| Nonce CSP with `strict-dynamic`, no `unsafe-inline` / `unsafe-eval`, no plugins, no framing | every page | `tests/csp.test.mjs`, `tests/browser-security.test.mjs` (built app, Chromium) |
| WebAssembly only inside the Worker | pages carry no `'wasm-unsafe-eval'`; the Worker has its own CSP (`default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; worker-src 'none'`) | journey step 1 (`pageCspWithoutWasmEval: true`); closes the former "§J 11" |
| Trusted Types enforced | `require-trusted-types-for 'script'`; one default policy that only admits same-origin `/_next/static/*.js` script URLs, no HTML / script creation | browser-security test (client navigation works, untyped sinks refused) |
| DOM XSS | message text rendered as React text only; static guard against HTML sinks in `src/`; hostile payloads inert in the real app | journey step 4 |
| Crypto module integrity | SHA-384 pinned at build time and checked by the Worker before compiling; tampering refused | journey step 13 |
| Release integrity | SHA-384 manifest of 48 static assets and 4 served public executables (crypto Worker, glue, wasm, Trusted Types policy); tampering / addition / deletion detected | `tests/build-assets.test.mjs`, Frontend CI |
| Dependencies | Next.js 16.1.5 → 16.4.2 (critical HTTP request smuggling, GHSA-ggv3-7p47-pfv8, and nine other production advisories): production dependencies 0 vulnerabilities | §G security scans |
| Session tokens | the login bearer is in JavaScript-readable cookies (pre-existing design): an XSS could act as the user through the relay (MANAGED content, metadata) but cannot read PRIVATE_E2EE content without a device. Moving the refresh token to an HttpOnly cookie changes the live login flow and needs its own rollout | threat model R4 |

**Browser-enforced Subresource Integrity — still open.** Measured on the production build
(Next.js 16.4.2, Turbopack): `/` 14, `/login` 18, `/dashboard/messages` 19 external
scripts, **0** with an `integrity` attribute, and chunks loaded at runtime are added by the
bundler without integrity. Next.js's `experimental.sri` reads its manifest at render time
but only for the few root bootstrap scripts (`getRequiredScripts`); page chunks inserted by
React and by the Turbopack runtime get none. Closing it needs either framework support or a
change of bundler / a patched loader for the live product, which this work did not make.
Mitigations in place: same-origin first-party serving, the nonce CSP, the release manifest
and the Worker's own integrity check of the crypto module. Residual risk R3 in
`docs/architecture/web-messaging-client-threat-model.md`. **Gate 25 stays PARTIAL** for this
one item; everything else in the gate passes in the real app.

---

## E. Capacity (gate 16)

Prepared, **not executed on hosted staging** (no environment authorized).

* `scripts/staging/` — `preflight.mjs` (target safety, `/ready`, credential, principals,
  MANAGED smoke), `capacity-runner.mjs` (MANAGED send throughput, history paging, realtime
  fan-out over WebSockets, reconnection storm, search, attachment pipeline with presigned
  uploads; correctness asserted; devices revoked and policy restored afterwards),
  `report.mjs` (Markdown; `MEASURED` without owner thresholds, `PASS` / `FAIL` only
  against an owner-approved thresholds file — no threshold is invented), runbook
  `scripts/staging/README.md` (environment prerequisites, migration preflight, smoke,
  rollback, evidence).
* Safety: CF Meet production hosts always refused; hosted targets need `https`, the
  explicit `CFMEET_STAGING_ALLOWED_HOSTS` allowlist and an owner authorization reference;
  credentials only from private files.
* `.github/workflows/staging-capacity-certification.yml` — manual dispatch from `staging`
  only, behind `STAGING_CERTIFICATION_ENABLED` and the protected `hosted-staging`
  environment.
* Rehearsal in CI (Messaging → service test) and locally against the real Platform Access,
  Messaging binary and MinIO: every workload correct, 5 devices revoked, policy restored,
  refusals proven (`evidence/staging-kit-rehearsal-2026-10-20.md`). A loopback rehearsal on
  a development host — **not** a capacity result.

The development-host measurements of `load-measurements-2026-10-20.txt` (Part K
remediation §F) remain the only capacity numbers; no maximum supported user count is
claimed.

---

## F. External certification readiness

| Gate | What exists (tested) | What is still needed (owner / external) |
|---|---|---|
| 13 SMS public carrier | SMS Bridge and SMS Gateway against a synthetic carrier across real processes (scenario 5, gateway-death journey) | a contracted carrier, sandbox credentials, a lawful route; run the bridge suite against it (no paid traffic was sent) |
| 20 Real-device MLS runtime | crypto core on wasm in Chromium inside the shipped web app; Android / iOS targets compile-checked only | a Messaging client in `apps/mobile` (not built yet), then runs on physical Android / iOS devices; other browsers |
| 21 Independent cryptographic review | ADRs 0045–0052, `packages/messaging-crypto` (incl. the wasm crate), the MLS Delivery Service, history transfer, the web threat model; the defect in §C shows the value of an independent review | commission the assessment; scope above |
| 22 Key-transparency witnesses | `messaging-kt-witness` binary, quorum cosigning, fork / rollback refusal tested (`transparency_witness`) | independent operators each running a witness with their own key; `MESSAGING_KT_WITNESSES_FILE`; the web client's log-key pin (`NEXT_PUBLIC_MESSAGING_KT_LOG_KEY` / `_LOG_ID`) set per environment |
| 23 Push providers | APNs, FCM HTTP v1, Web Push adapters against verifying local fakes (`push_adapters`) | sandbox accounts and credentials; deliveries to real devices |
| 30 Hosted staging | §E kit | authorization, provisioning, the runs of §E |
| 31 Owner approval | — | explicit approval after the gates pass |

---

## G. Regression, verification and CI

### G.1 CI on the final code commit

Workflows run per push for the paths they own; `8999ac6` changed only `apps/web`, so the
other workflows' latest runs are on `bedbaa7` (same code outside `apps/web`). The full
matrix dispatched on the report commit is listed in the hand-off.

| Workflow | Commit | Result | Run |
|---|---|---|---|
| Frontend | `8999ac6` | success | [39502019540](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39502019540) |
| Web crypto artifact | `8999ac6` | success | [39502019577](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39502019577) |
| Messaging | `8999ac6` | in progress at commit time | [39502019621](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39502019621) |
| Unified Messaging integration journeys | `8999ac6` | in progress at commit time | [39502019568](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39502019568) |
| Service boundaries | `8999ac6` | in progress at commit time | [39502019753](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39502019753) |
| Unified Messaging integration journeys (incl. web client) | `bedbaa7` | success | [39500880409](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880409) |
| Meeting service | `bedbaa7` | success | [39500880342](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880342) |
| Dependency and image security | `bedbaa7` | success | [39500880515](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880515) |
| Contracts and service auth | `bedbaa7` | success | [39500880488](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880488) |
| Platform Access service | `bedbaa7` | success | [39500880377](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880377) |
| Calling service | `bedbaa7` | in progress at commit time | [39500880479](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880479) |
| Calling RTC certification | `bedbaa7` | success | [39500880376](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880376) |
| Enterprise calling certification | `bedbaa7` | success | [39500880465](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880465) |
| Communications end-to-end journeys | `bedbaa7` | success | [39500880443](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880443) |
| Telephony service | `bedbaa7` | success | [39500880361](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880361) |
| Telephony SIP certification | `bedbaa7` | success | [39500880466](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880466) |
| Numbering service | `bedbaa7` | success | [39500880549](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880549) |
| SMS Gateway service | `bedbaa7` | success | [39500880489](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880489) |
| Privacy Relay service | `bedbaa7` | success | [39500880486](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880486) |
| Object storage | `bedbaa7` | success | [39500880597](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880597) |
| Webhooks package | `bedbaa7` | success | [39500880520](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39500880520) |
| Messaging (staging-kit rehearsal added) | `3f392fa` | success | [39493578300](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39493578300) |

### G.2 Local verification on the final tree

| Suite | Result |
|---|---|
| Web: typecheck, production build, release manifest (48 assets + 4 executables), `csp` 6/6, `build-assets` 3/3, `messaging-realtime` 7/7, `messaging-session` 5/5, `browser-security` 3/3 (Chromium) | pass |
| Web journey on real services (Chromium) | 3/3 on the final tree (after 6/6 with the crypto fix) |
| Crypto core: all features (incl. `multi_device_welcome`, `device_state`, `mls_compatibility`), clippy both feature sets, fmt | pass |
| Messaging: `attachments` 22/22 (real MinIO), `messaging` 10/10, `history_transfer` 2/2, staging-kit rehearsal | pass |
| Meeting: `api_contract_matrix` 13/13, `unified_messaging_api` 6/6, relay unit test, v2 operation coverage 153/153 | pass |
| Repository guards: service boundaries, secret scan | clean |

### G.3 Swagger / OpenAPI

* 11 peer documents served by the Meeting Service Swagger UI render in Chromium with the
  schema host blocked: **446 operations, 840 schemas, 0 resolver errors** (unchanged).
* Meeting Service document: **205 operations (153 v2, 52 legacy v1)**. For v2: the route
  table equals the document (`j_e2e_06`), all 153 are routed with the standard error
  contract (`j_api_01`), success and error paths are covered for 153 / 153 (strict
  coverage gate). The 52 legacy v1 operations are documented but outside these gates.
* The 8 new client Messaging operations (`/api/v2/messaging/{me,directory,directory/resolve,principals/names}`, relay GET / POST / PUT / DELETE) are published and covered.
* Production Swagger was neither redeployed nor re-verified (no deployment authorized).

### G.4 Security scans (`evidence/security-scans-2026-10-20.txt`)

| Scan | Result |
|---|---|
| RustSec, all 15 lockfiles (incl. the new `messaging-crypto/wasm` and `web-harness`, now in the CI matrix) | 0 vulnerabilities |
| New advisories of 2026-10-19/20 | RUSTSEC-2026-0382 (`rustls-webpki` → 0.103.16) and RUSTSEC-2026-0381 (`webpki-roots` → 1.0.10) fixed in 11 lockfiles |
| Documented exceptions | RUSTSEC-2023-0071 (`rsa`, unchanged justification); RUSTSEC-2026-0330/0331 (`libcrux-kem`, proven unlinked, unchanged); RUSTSEC-2026-0381 for the `webpki-roots 0.26.11` shim only — it holds no certificates and re-exports the fixed 1.0.10; enforced by `scripts/security/check_webpki_roots_shim.py` (pinned checksum, self-tested) |
| npm `apps/web` | production 0; 5 high in the ESLint toolchain only (dev, not shipped) |
| npm `apps/mobile` | 38 (23 high, 15 moderate), pre-existing, Expo toolchain; not changed here (mobile release is out of scope) — **open** |
| npm web-harness, swagger-render | 0 |
| Licences | messaging-crypto 156, wasm 157, web-harness 157, Messaging 406 packages: all allowed |
| SBOM | CycloneDX 1.5, 884 components from 15 lockfiles (CI artifact) |
| Secret scan | clean (tracked files; image filesystems in CI) |

### G.5 Known CI instability (not caused by this work)

The Calling service's `test (PostgreSQL)` job fails intermittently on a different test each
run — `answered_is_not_connected_and_can_fail_when_media_fails` (Platform Access 401),
`callback_relay_concurrency` (`TARGET_UNAVAILABLE`), and before this work
`a_stuck_queue_entry_is_an_incident_and_drains_later` (shared metrics gauge, commit
`b7fd12f`). No Calling code was changed. Root cause (shared state between parallel tests)
is not yet fixed; recorded as a follow-up.

---

## H. 31-gate production acceptance matrix

| Gate | Starting status | Final status | Evidence | Remaining dependency |
|---|---|---|---|---|
| 1 Domain correctness | PASS | PASS | Messaging suites; G.2 | — |
| 2 Migrations, constraints, DB role isolation | PASS | PASS | migration smoke incl. migration 19; per-service roles; §B safety review | — |
| 3 Tenant / IDOR isolation | PASS | PASS | authorization matrix; relay allowlist tests | — |
| 4 Realtime delivery guarantees | PASS | PASS | `realtime`, `realtime_redis`; journey steps 3, 6, 7 | — |
| 5 MANAGED attachments with real storage and scanner | PASS | PASS | `attachments` (real MinIO), M11 journey | — |
| 6 Encrypted-media processing | PARTIAL | **PASS** | §B: derivative bytes reserved, committed and charged; 22 / 22 tests with real MinIO; PRIVATE_E2EE never processed | — (transcoding not required, not claimed) |
| 7 Meeting migration (real processes) | PASS | PASS | UM e2e | — |
| 8 Breakout / private Meeting chat migration | PASS | PASS | real-process e2e | — |
| 9 External developer journey | PASS | PASS | `platform_access_e2e` | — |
| 10 First-party integration | PASS | PASS | UM e2e; web client through the Meeting relay | — |
| 11 Privacy Relay revocation | PASS | PASS | scenario 4 | — |
| 12 SMS Bridge software | PASS | PASS | scenario 5 | — |
| 13 SMS public carrier edge | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | — | carrier contract, sandbox credentials (§F) |
| 14 Webhook privacy | PASS | PASS | scenario 9 | — |
| 15 Concurrency | PASS | PASS | incl. 8 racing derivative workers | — |
| 16 Capacity | PARTIAL | PARTIAL | dev-host measurements; §E kit + rehearsal | authorized hosted staging and its runs |
| 17 Fault injection | PASS | PASS | previously certified cases; derivative crash / write-failure tests | — |
| 18 Authentication / authorization security | PASS | PASS | service-auth suites; relay signs the principal; forwarded headers allowlisted | — |
| 19 MLS conformance | PASS | PASS | RFC 9420 vectors; `multi_device_welcome` regression for the join defect | — |
| 20 Real-device MLS runtime | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | shipped web client in Chromium (§C) — not a physical device | `apps/mobile` Messaging client; physical devices (§F) |
| 21 Independent cryptographic review | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | review package (§F) | assessor |
| 22 KT external witness | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | witness software tested | independent operators |
| 23 Push providers | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | adapters vs verifying fakes | provider sandbox accounts |
| 24 Device recovery | PASS | PASS | `history_transfer`; journey step 10 in the shipped app | devices: gate 20 |
| 25 Web XSS / CSP / Trusted Types | PARTIAL | PARTIAL | §D: real app in Chromium — CSP, Trusted Types, XSS, Worker-only wasm, crypto-module integrity, release manifest | browser-enforced SRI for every script (framework / bundler) |
| 26 Dependency advisories | PASS | PASS | §G.4: 0 RustSec vulnerabilities in 15 lockfiles; web production npm 0 | `apps/mobile` npm advisories (mobile release, out of scope) |
| 27 Licence / SBOM / image / secret scans | PASS | PASS | §G.4 | — |
| 28 Compliance | PASS | PASS | `compliance`; held deletions stay charged | — |
| 29 Production E2EE disabled, fail-closed | PASS | PASS | `operations` (server refuses E2EE devices in production); the web client maps `E2EE_DEVICES_DISABLED` to a MANAGED-only state (code path, not exercised by the journey) | — |
| 30 Hosted staging deployment | NOT_TESTED | NOT_TESTED | §E kit prepared | authorization |
| 31 Owner production approval | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | — | owner |

Totals — start: PASS 21, PARTIAL 3, FAIL 0, BLOCKED_EXTERNAL 6, NOT_TESTED 1.
Final: **PASS 22, PARTIAL 2 (16, 25), FAIL 0, BLOCKED_EXTERNAL 6, NOT_TESTED 1.** The target
of PASS 23 / PARTIAL 1 was not reached: gate 25's integrity item remains.

---

## I. Final declarations

| Declaration | Answer | Basis |
|---|---|---|
| SOFTWARE COMPLETE | **NO** | gate 25 SRI item; no Messaging client in `apps/mobile`; `apps/mobile` npm advisories; Calling test instability |
| BACKEND INTEGRATION READY | **YES** | every backend suite and real-process journey passes on the final code commit (§G.1); derivative quotas complete; relay and identity bridge certified. Caveat: the intermittent Calling test failures of §G.5 are not yet root-caused |
| WEB MESSAGING INTEGRATION READY | **YES** | the shipped web app runs real E2EE Messaging against real services in Chromium, in CI (§C) |
| HOSTED STAGING CERTIFIED | **NO** | not provisioned / not authorized |
| EXTERNAL CERTIFICATION COMPLETE | **NO** | gates 13, 20, 21, 22, 23 |
| PRODUCTION RELEASE APPROVED | **NO** | gates 16, 25, 30, 31 open; no owner approval |
