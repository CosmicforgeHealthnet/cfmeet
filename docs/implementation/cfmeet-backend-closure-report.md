# CF Meet — executive backend closure report (Document A)

Date: 2026-11-09. Baseline: CF Meet Consolidated Platform Architecture & End-to-End Implementation
Blueprint v1.0 (18 sections, 149 audit findings, 22 journeys). Integration branch: `staging`.
Production (`main`) untouched; open release PR #39 (`staging` → `main`) left open, not merged.

## 1. Verdict

**BACKEND_ENGINEERING_PARTIAL — BLOCKERS_REMAIN.** All 22 journeys are certified (19 PASS, 1 PASS
with an owner gate, 2 EXTERNAL_BLOCKED with truthful reporting; none NOT_COVERED, PARTIAL or
failed), no P0/P1 finding has implementable backend work left, and the full CI matrix is green.
No implementable backend finding remains open: SEC-012 (§2), INV-002, PLT-008, DATA-017, MSG-009 and
MSG-018 (§6) were all closed after the report was first written, so the strict "every implementable
finding closed" verdict now holds for the code. Repository cleanup is still incomplete: the
38 temporary branches are verified safe to delete, but this session cannot delete remote refs (§5).

## 2. What this closure implemented

| Area | Change | Evidence |
|---|---|---|
| Security (J-07) | `ORGANIZATION_ONLY` meeting spaces are no longer disclosed by identifier lookup (space read, resolve, Hub meeting card) to outsiders (404) or signed-out callers (401); members, invitees and token holders keep access | `a48b0413`; `org04`; real-process J-07 |
| SEC-004 | legacy guest refresh: live participation required, single use, 24 h cap | `1d8bf9c2` |
| SEC-012 (closed) | service tokens bound to the request: optional `bnd` / `htm` / `htu` claims with a documented canonical-path rule (`packages/service-auth/src/binding.rs`; contract `token-claims/v1.schema.json`); `ServiceTokenVerifier::verify_request(method, path, headers)` with versioned modes `off` / `accept` (default) / `require` (`SERVICE_AUTH_REQUEST_BINDING_MODE`) and distinct failures (`request_method_mismatch`, `request_path_mismatch`, `request_binding_missing`); jti replay guard on POST/PUT/PATCH/DELETE (`replay.rs`: bounded in-memory LRU/TTL behind the `ReplayGuard` trait, shared through Redis in Messaging); signers emit behind `SERVICE_AUTH_REQUEST_BINDING=emit` (default `off`; `InternalServiceClient` binds every call, API Gateway / Meeting Service / Messaging scanner bind their direct calls); every receiver (12 services, 20 sites) verifies through `verify_request`. Rollout steps 2–3 are operator steps (`docs/operations/service-token-request-binding.md`) | `packages/service-auth/tests/request_binding.rs`, `binding::tests`, `replay::tests`, `contract_conformance`; `security-scan-gateway/tests/gateway.rs::a_request_bound_token_only_works_for_its_method_and_path`; api-gateway suite with `SERVICE_AUTH_REQUEST_BINDING=emit SERVICE_AUTH_REQUEST_BINDING_MODE=require`; `messaging config::replay_guard::tests` (real Redis) |
| ORG-011 / MTG-016 | organization OWNER / ADMIN (`meetings.organization.manage`) configure, reassign, archive organization meetings, with FULL view | `89efdc91` |
| J-08 | one invited-guest journey end to end | `3b8b7f42` |
| J-11 | real-media personal call closes in both histories | `d7959fd7` |
| J-18 | real-schema backup / restore drill over all 11 databases, new CI job | `42b415fd` |
| J-12 harness | Calling binding gets its own service account | `84b981b1` |
| Frontend contract | generated, CI-checked endpoint reference (618 operations) + handover guide | `3e944b79`, `cea2088d` |
| INV-002 | every contract operation is exercised by a test: route-level tests (401 / 403 / OPERATOR_REQUIRED, validation 400, missing / foreign 404, refused state 409 / 422, success with effect) for the 40 internal / operator operations left under the frozen baseline; the baseline is now empty, so an untested operation fails `scripts/contracts/operation_test_coverage.py` in CI (0 of 1,151 untested) | `services/platform-access/tests/operator_reads.rs`, `services/communications/tests/operator_routes.rs`, `services/calling/tests/webhook_migration.rs`, `services/developer-platform/tests/operator_commands.rs`, `services/developer-platform/tests/webhook_migration_routes.rs`, `services/numbering/tests/operator_http.rs` (22 tests); 3900b3db, 18f14dd0, f71fb728, de76d5ab, cca0d321, 86955834 |
| Docs | audit closure register, certification report, runbook additions, ADR 0053 update, API inventory note | this branch |

## 3. Recovered and consolidated (from `blueprint/final-org-events`, 35 commits)

Organizations outbox delivery to subscribed consumers over signed service HTTP with retries, dead
letters and audited replay (no longer `DisabledDispatcher` when configured); Meeting Service
organization-event ingress (dedupe, per-aggregate fences) and the offboarding worker suspending
organization-scope principals; organization-scope Messaging (relay, realtime session tokens,
colleague resolution) with a per-organization Messaging binding; organization teams / policies relay
with safe member display identity (ORG-006); web Contacts and Messages (MSG-005/006/008/009) and
organization Messages view; positive password reset, QR route, canonical `PROVIDER_BLOCKED` label, DP
console revocation check; real-process certification of J-04/05/06/07/10/12/14/17/19; RTC Egress
capture on staging builds; API inventory and activation runbook.

## 4. Reused, not rebuilt

Calling and LiveKit infrastructure (including the SIP connection-race fix and its regression test),
Messaging and its realtime gateway, Communications Hub, Platform Access, Developer Platform and API
Gateway, Numbering / SMS Gateway / Telephony / Privacy Relay, the backup tooling, the migration guard.
No second authority was introduced: Organizations stays the only first-party business-organization
authority; the Developer Platform workspace model and the Platform Access technical tenant model are
unchanged.

## 5. Removed or deprecated

38 temporary remote branches verified safe to remove (0 unique commits); deletion is blocked in this session (git proxy 403) and is one owner command: `scripts/repository/delete-consolidated-branches.sh --apply` (branch consolidation report §5). No API removed.
Deprecated with migration (unchanged): legacy v1 meeting create paths, legacy meeting webhooks, the Hub
block-events ingest, legacy API keys (see API inventory §4).

## 6. Open backend follow-ups (exact)

| Finding | Sev | Remaining work |
|---|---|---|
| — | — | none |

Closed after this report (commit `23f42c92`): **MSG-009** — Hub call cards for call parties without a profile at launch (materialized once on first timeline / activity read) and a deduplicated, version-monotonic realtime nudge (`communications.timeline.changed` → Messaging `timeline.changed`, members only, live and replay); tests `services/communications/tests/unified.rs::call_cards_reach_parties_without_a_profile_and_timeline_changes_are_nudged_once`, `services/messaging/tests/realtime_receipts_timeline.rs::hub_timeline_nudges_reach_members_once_per_version`. **MSG-018** — server-side receipt-event coalescing (one leading and one trailing receipt per member and window, monotonic, privacy re-checked; `receipts.query` for reconnect); tests `realtime_receipts_timeline.rs::receipt_events_are_coalesced_per_member_and_window_and_snapshots_read_current_cursors`, `load.rs::read_receipt_burst_is_coalesced` (30 readers × 40 reads: 1,200 reads → 60 receipt events). Records: `docs/operations/messaging-section-13.md`, `docs/operations/communications-hub-section-14.md`.

Closed since this report was first written:

| Finding | Sev | Closure evidence |
|---|---|---|
| PLT-008 | P2 | 9bb7f35e: credential.expiring_soon / credential.expired worker sweep (services/developer-platform/src/control/credential_lifecycle.rs, worker.rs; migration m20261203_000014; catalog + webhook schema); tests services/developer-platform/tests/credential_lifecycle.rs (thresholds, dedupe, crash recovery, outage + delivery retry, scope authz, schedule) |
| DATA-017 | P3 | 9bb7f35e (developer-platform: src/reference_reconciliation.rs, bin developer-platform-reconcile-references, worker job, remote checks at Platform Access) + 5eae90cf (messaging + numbering: src/reference_reconciliation.rs, bins *-reconcile-references); tests tests/reference_reconciliation.rs in each service (missing/invalid refs reported, nothing deleted); report only; runbook docs/operations/hardening-sections-2-3.md §6.3 |

## 7. Owner and external gates (kept disabled; not reported as live)

Production Organizations authority cutover; off-host backup target and age key custody; production
migration approval; API Gateway / Developer Platform live deployment; production secrets and trust
bundles; per-organization Enterprise Calling and Messaging credentials / bindings; APNs, FCM and Web
Push; SMS / phone verification provider; lawful PSTN carrier and real numbers; independent PRIVATE_E2EE
approval; physical-device acceptance; hosted staging deployment of this build.

## 8. Handover set

| Document | Path |
|---|---|
| A. Executive closure report | `docs/implementation/cfmeet-backend-closure-report.md` |
| B. API inventory | `docs/implementation/final-api-inventory.md` + `docs/frontend/cfmeet-frontend-endpoint-reference.md` |
| C. Frontend handover guide | `docs/frontend/cfmeet-frontend-handover.md` |
| D. Certification report | `docs/certification/cfmeet-final-certification-report.md` |
| E. Audit closure register | `docs/implementation/cfmeet-audit-closure-register.md` |
| F. Activation runbook | `docs/operations/final-production-activation-runbook.md` |
| G. Branch cleanup verification | `docs/implementation/cfmeet-branch-consolidation-report.md` |
