# Part IV — final consolidated closure register (Parts P, Q, R; §§119–166)

Branch `ccr-c5e05129-xhhmmk`, starting point `7d93ab1` (Workstreams A–O closed). This
register records what Parts P–R added, the single consolidated verification pass, and
which external / production gates remain open. It introduces no new mechanism.

Evidence classes are kept apart throughout:

* **local** — executed in this environment (PostgreSQL 16, debug builds, no Docker);
* **remote CI** — executed in GitHub Actions (stated per workflow below);
* **real provider** — executed against a lawful carrier (none so far).

## A. Section closure

| Section | Status | Evidence |
|---|---|---|
| Part P — Migration & Compatibility (§§119–123) | PASS (software) | Relay numbers imported into Numbering as `PRIVACY_RELAY`-class Number Resources (idempotent backfill, binding preserved); authenticated Number Lease contract (`number-relay.lease`, Privacy Relay only); compatibility resolution; `RELAY_NUMBER_AUTHORITY=LOCAL→NUMBERING` only after a verified backfill + compatibility check; routing equivalence and rollback to LOCAL proven across real processes (`relay_cutover_e2e`) and in `numbering_authority`; duplicate inventory retirement **gated** (ADR 0044 §6) — no historical data removed, no production migration run. Platform Access, Calling, Telephony: no contract or id changes; their suites re-run (§C). Contracts: backward-compatible minor bumps only. |
| Part Q — Testing & Certification (§§124–134) | PASS (software) / REAL PROVIDER CERTIFICATION PENDING | One consolidated pass (§C). Deterministic lifecycle Search → … → Reassignment executed through real service boundaries (§C.2). Real-provider checklists retained (§E), not executed: no lawful carrier credentials, test inventory or approved spend exist. |
| Part R — Implementation & Closure (§§135–166) | PASS (software) | A–O previously closed, no regression found; P–X implemented (§B); §§161–166 reviewed (§D). Production / external gates PENDING (§D.2). |

## B. Workstreams

A–O: closed before this part (ADRs 0032–0042). The consolidated pass found no
regression in them.

| WS | Status | Essential changes | Evidence |
|---|---|---|---|
| P — suspension / restoration | Complete | Local-first suspension (grants refused and revoked before the provider route is disabled); restoration re-checks owner, standing, eligibility, capability and provider state; operator / risk suspensions lifted by operators only | `lifecycle_stewardship::p_*` (incl. suspension / grant race) |
| Q — release / quarantine / reassignment | Complete | Mandatory order (grants stop → routing removed → quarantine); POOL_RETURN / PROVIDER_RELINQUISH; unknown provider outcome reconciled by read-back, never resent; late-activity evidence and review; SKIP LOCKED completion; database reassignment gate; reassignment as a fresh basis | `q_*`, `database_guards_*`, `sms_suspend_restore_release_quarantine_and_reassignment_across_real_boundaries` |
| R — history / audit / recovery | Complete | Ownership periods, audited masked claim verification, justified lifecycle-evidence access | `q_pool_return…` (history), HTTP operator test |
| S — usage / billing readiness | Complete | Idempotent internal usage / provider-cost evidence (never customer prices), provisional → final, cost reconciliation | `s_usage_*` |
| T — abuse / fraud / spend | Complete | Allocation velocity / country / count limits, signals with RISK suspension, per-number spend stop, provider-cost breaker, appeals | `s_…t_spend…`, `t_allocation_*` |
| U — portability foundation | Complete (flagged off) | Provider-independent port requests, evidence, submit / cancel / status, port-out keeps routing until completion; `NUMBERING_PORTING_ENABLED=false`; no live portability provider | `u_porting_*` |
| V — provider events / reconciliation | Complete | Recurring fail-closed lifecycle drift reconciliation, incidents | `v_provider_drift_*` |
| W — observability / operations | Complete | Kill switches (DCN inbound, caller identity, SMS sender, provisioning, porting, relay leases) with audit; gauges; runbook §6; backup / restore of Numbering and SMS Gateway resuming without duplicate paid actions | `v_…w_kill_switches…`, `w_a_restored_database_*`, `sms-gateway/disaster_recovery`, `w_load_*` |
| X — Part P migration | Complete (retirement gated) | See §A Part P | `relay_cutover_e2e`, `numbering_authority`, Privacy Relay `migrations` / `migration_upgrade` |
| Y — certification | Software PASS; real provider PENDING | §C, §E | this register |
| Z — closure | Complete | this register | — |

Commits (oldest first): `7e2934b` stewardship domain, `52561d0` APIs / contracts /
permissions, `0198f57` relay-number authority cutover, `78912b6` backup / restore + CI
gates, `2416ed6` docs / runbook / capacity, and the final certification commit.

## C. Consolidated verification (local)

Environment: 4 vCPU Xeon 2.8 GHz, 15 GB RAM, PostgreSQL 16 on loopback, debug builds, no
Docker. Failures found during the pass were fixed and their suites re-run (C.4).

### C.1 Suites

| Gate | Result |
|---|---|
| Contracts (`make contracts-validate contracts-test`) | OK — 49 contracts, 228 examples, 310 counter-examples, 589 JSON files; 17 / 17 validator mutation tests |
| Service boundaries + operations registry + secret scan | 69 + 12 unit tests OK; boundary guard OK; `secret scan: clean` |
| Database isolation (`make database-isolation`) | `service database isolation: OK` (runtime / migrator roles cannot connect across services) |
| fmt + strict clippy (`-D warnings`) | Numbering, Privacy Relay, SMS Gateway, service-auth: clean |
| service-auth | 128 passed, 0 failed |
| Numbering (whole workspace, real Platform Access) | 206 passed after fix (one race-test assertion corrected, C.4); `lifecycle_stewardship` 14 / 14 incl. backup / restore with `DISASTER_RECOVERY_CERT_REQUIRED=1` |
| Privacy Relay (whole workspace) | 172 passed after fix (lease ledger declared platform-owned in the migration test, C.4); incl. `numbering_authority`, `disaster_recovery`, `migration_upgrade`, `relay_load` |
| SMS Gateway (whole workspace) | 69 passed, 0 failed; incl. `disaster_recovery` |
| Telephony (whole workspace) | 165 passed, 0 failed; incl. the real SIP / media edge certification (Asterisk + SIPp) |
| Calling (whole workspace, real Platform Access) | 680 passed, 0 failed |
| Platform Access (whole workspace) | 103 passed, 0 failed |
| Calling ↔ Privacy Relay / Telephony integration (real binaries) | `privacy_relay_e2e` 3 / 3, `telephony_e2e` 2 / 2 |
| Dependency audit (`cargo audit`, Numbering / Privacy Relay / SMS Gateway) | only RUSTSEC-2023-0071 (`rsa`, no fixed release; already ignored with justification in `dependency-security.yml`); no lockfile changed on this branch. Container image scans: not executed locally (no Docker) |

### C.2 Synthetic lifecycle certification (§128), real service boundaries

`make communications-e2e` — Numbering served on a real port, real processes of Platform
Access, SMS Gateway, Calling, Privacy Relay, Telephony, each with its own database:

* `sms_journeys…`, `sms_lifecycle_safety…` — pass;
* `sms_suspend_restore_release_quarantine_and_reassignment_across_real_boundaries` — pass:
  Search → KYC → Reserve → Provision → Assign → Activate → inbound / outbound SMS →
  Suspend → Restore → Release → Quarantine → Reassignment to another tenant, previous
  owner isolated (cooldown elapse simulated in Numbering's database);
* `relay_numbers_move_to_numbering_leases_with_identical_routing_and_a_safe_rollback` — pass;
* `voice_journeys…` — **SKIPPED here** (LiveKit server / SIP binaries and the RTC Python
  client are not installed in this environment); the voice path is covered locally by
  Telephony's SIP edge certification and the Calling integration journeys, and in full
  by `communications-e2e.yml` when it runs in CI.

Unknown outcomes, duplicate callbacks and prior-owner isolation: `provider_contract`,
`provisioning_activation`, `lifecycle_stewardship::q_relinquished…`, `q_pool_return…`.

### C.3 Capacity (local, in-process, debug build — not carrier capacity)

| Workload | n | Concurrency | Throughput | p50 / p95 |
|---|---|---|---|---|
| Inbound voice resolution with grant issuance | 400 | 16 | 137 / s | 104 / 194 ms |
| Relay-lease contention (20 numbers × 5 holders) | 100 | 16 | 1257 / s | 12 / 19 ms (exactly 20 granted) |
| Carrier usage ingestion (1000 keys, each twice) | 2000 | 16 | 1409 / s | 12 / 20 ms (1000 rows) |

Privacy Relay `relay_load` and Calling `control_plane_load` / `enterprise_load` passed in
their suites (correctness assertions). Search / reservation, KYC and SMS-event
concurrency are exercised for correctness (`inventory_reservations`, `kyc_eligibility`,
SMS `outbound` with four concurrent dispatchers), not benchmarked.

### C.4 Defects found and fixed in the pass

1. Numbering clippy (doc indentation, argument count, `is_multiple_of`, test style) — fixed.
2. `p_a_grant_racing_a_suspension…`: a racing grant request can be refused
   `CAPABILITY_NOT_USABLE` (422) as well as 404 — test accepted both refusals (no grant
   issued either way); 10 / 10 re-runs pass.
3. Privacy Relay `migrations`: `relay_number_leases` declared platform-owned — fixed.
4. New cross-process journey: cooldown simulation needed the number's own
   `quarantine_until` (with its revision) — fixed; journey passes.
5. CI configuration: Numbering / Privacy Relay smoke table counts (36 → 50, 26 → 27), the
   port request's internal provider id in Numbering's provider-column allowlist, and
   Platform Access binaries for the Calling e2e jobs of the Privacy Relay and Telephony
   workflows.
6. `privacy-relay/tests/dcn_caller.rs`: the "no number in the answer" check matched the
   4-digit run `7946`, which a random UUID can contain — now checks the whole subscriber
   number; 5 / 5 re-runs pass.

### C.5 Remote CI

Dispatched with `workflow_dispatch` on this branch (no pull request was opened):

| Workflow | Result |
|---|---|
| Contracts and service auth | success (`448d6a2`) |
| Service boundaries | success (`448d6a2`) |
| Dependency and image security | success (`448d6a2`) |
| Calling service | success (`448d6a2`) |
| Telephony service | failure on `448d6a2` (Calling e2e job without Platform Access binaries — a CI wiring gap since Workstream B); success on `97c201a` after the fix |
| Privacy Relay service | failure on `448d6a2` (smoke table count 26 → 27; same Calling e2e wiring gap); on `97c201a` everything passed except one intermittent pre-existing assertion in `dcn_caller` (a random UUID can contain the digits `7946`), fixed in the following commit and re-dispatched |
| Numbering, SMS Gateway, Communications e2e, Platform Access | **not executed remotely**: `workflow_dispatch` returns 404 for them (these workflow files are not on the default branch, which GitHub requires for dispatch); they run on push to main / staging or on a pull request. Their table-count smoke steps were run locally against the real migrations (Numbering 50 tables, Privacy Relay 27, SMS Gateway checks pass) |


## D. §§161–166

1. **§161–163 coding order and milestones.** The recommended order was followed
   (foundation → catalog → KYC → provider contract → inventory → provisioning →
   ownership → grants → voice → SMS → stewardship → migration). First functional
   milestone (a synthetic DCN from search to authorized voice / SMS): `dcn_lifecycle`
   Scenario A and `communications_e2e`. Provider-independent integration milestone: the
   provider-neutral adapter contract and reusable certification suite (`provider_contract`)
   with the deterministic simulator; no business logic depends on a provider.
2. **§164 production acceptance gates.**

   | Gate | Status |
   |---|---|
   | Architecture and service boundaries | PASS (local: `service-boundaries`, `database-isolation`, no cross-service FK / connect) |
   | Consumer authentication | PASS (real Platform Access in Numbering / Calling suites) |
   | Numbering and lifecycle | PASS (software) |
   | Capability accuracy | PASS for synthetic inventory; real carrier capability validation PENDING |
   | Voice and SMS | PASS (synthetic, real processes); public PSTN / carrier SMS PENDING |
   | Privacy / quarantine | PASS (software) |
   | KYC / compliance | PASS (software, deterministic verifier + manual review); regulatory review PENDING |
   | Provider independence | PASS |
   | Risk | PASS (software); thresholds need production tuning — PENDING |
   | Reliability / security | PASS (local); see §C security row |
   | Scale and operations | Local measurements only (§C); production capacity PENDING |
   | Real-provider certification | PENDING (§E) |

3. **§165 Definition of Done (CosmicForge-owned software).** Numbering and SMS Gateway;
   Platform Access authority; complete DCN lifecycle; all ownership classes; voice and SMS
   integration; capability truth; KYC / eligibility; provider-independent execution;
   quarantine and reassignment; permanent assignment history; portability foundation;
   allocation / fraud / SMS / spend controls; migrations, APIs, contracts, workers and
   observability; Part III compatibility — all implemented and covered by the suites in
   §C. **SOFTWARE COMPLETE** (local evidence; remote CI as stated in §C).
4. **§166 Architecture lock — unchanged.** Platform Access authenticates; Numbering owns
   DCN resources and lifecycle (and now the relay-number inventory authority, by lease);
   Calling owns logical calls; Telephony owns public voice execution; Privacy Relay owns
   temporary privacy sessions (sessions, ids, generations and inbound mapping stay there);
   SMS Gateway owns SMS transport; providers remain replaceable adapters. No new layer,
   no new cross-service database access.

### Closure taxonomy (§134)

| Status | Claimed? | Basis |
|---|---|---|
| SOFTWARE COMPLETE | **Yes** | every CosmicForge-owned gate in §C passes locally |
| PROVIDER INTEGRATION READY | **Yes** | provider-neutral contract, environment-restricted registry, reusable certification suite with gated live cases, deterministic simulator passing it |
| REAL PROVIDER CERTIFIED | **No — REAL PROVIDER CERTIFICATION PENDING** | no lawful provider credentials / inventory / approved spend |
| PRODUCTION PASS | **No** | regulatory, provider, operations, scale and rollout gates pending |

## E. Real-provider certification checklists (§§131–132) — REAL PROVIDER CERTIFICATION PENDING

Execute only with a lawful provider contract, credentials in the deployment secret store,
supported test inventory, approved spend and written authorization. Run the gated live
cases of `provider_contract` (ADR 0036) against the adapter, then:

| # | Check | Pass criterion | Status |
|---|---|---|---|
| 1 | Lawful provisioning | search → reserve → provision of a real number with the provider's KYC requirements mapped into F; binding recorded; no provider id leaves Numbering | PENDING |
| 2 | Inbound PSTN voice | a call from a public handset reaches the routed target through Telephony's DCN allowlist and a Numbering grant | PENDING |
| 3 | Outbound PSTN voice | caller identity presented only with a consumed caller-identity grant; carrier shows the DCN | PENDING |
| 4 | Inbound SMS | carrier SMS reaches the routed target via the SMS destination resolver | PENDING |
| 5 | Outbound SMS | submission, delivery receipt, cost; UNKNOWN outcomes resolved by read-back | PENDING |
| 6 | Capability validation | provider capability facts become VERIFIED only with evidence; downgrades propagate | PENDING |
| 7 | Provider events | signed callbacks authenticated, de-duplicated, order-safe; drift raises incidents | PENDING |
| 8 | Suspension / restoration / release | provider route disable / enable / release confirmed by read-back; quarantine completion requires provider confirmation | PENDING |
| 9 | Costs and limitations | usage / cost evidence reconciles with the provider invoice; documented limitations (porting, SMS registration, country rules) | PENDING |

No OTP, WhatsApp, banking or third-party platform acceptance is claimed or guaranteed
(§133); catalog and API texts keep that qualification (ADR 0034).
