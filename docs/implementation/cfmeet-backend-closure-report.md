# CF Meet — executive backend closure report (Document A)

Date: 2026-11-09. Baseline: CF Meet Consolidated Platform Architecture & End-to-End Implementation
Blueprint v1.0 (18 sections, 149 audit findings, 22 journeys). Integration branch: `staging`.
Production (`main`) untouched; open release PR #39 (`staging` → `main`) left open, not merged.

## 1. Verdict

**BACKEND_ENGINEERING_PARTIAL — BLOCKERS_REMAIN.** All 22 journeys are certified (19 PASS, 1 PASS
with an owner gate, 2 EXTERNAL_BLOCKED with truthful reporting; none NOT_COVERED, PARTIAL or
failed), no P0/P1 finding has implementable backend work left, and the full CI matrix is green.
Four implementable backend findings remain open, all P2/P3 (§6); MSG-009 and MSG-018 were closed after the report (§6 note). They block no journey and no frontend
contract, so the backend contract is frozen and handed over; they keep the strict "every
implementable finding closed" verdict from being claimed. Repository cleanup is also incomplete: the
38 temporary branches are verified safe to delete, but this session cannot delete remote refs (§5).

## 2. What this closure implemented

| Area | Change | Evidence |
|---|---|---|
| Security (J-07) | `ORGANIZATION_ONLY` meeting spaces are no longer disclosed by identifier lookup (space read, resolve, Hub meeting card) to outsiders (404) or signed-out callers (401); members, invitees and token holders keep access | `a48b0413`; `org04`; real-process J-07 |
| SEC-004 | legacy guest refresh: live participation required, single use, 24 h cap | `1d8bf9c2` |
| ORG-011 / MTG-016 | organization OWNER / ADMIN (`meetings.organization.manage`) configure, reassign, archive organization meetings, with FULL view | `89efdc91` |
| J-08 | one invited-guest journey end to end | `3b8b7f42` |
| J-11 | real-media personal call closes in both histories | `d7959fd7` |
| J-18 | real-schema backup / restore drill over all 11 databases, new CI job | `42b415fd` |
| J-12 harness | Calling binding gets its own service account | `84b981b1` |
| Frontend contract | generated, CI-checked endpoint reference (618 operations) + handover guide | `3e944b79`, `cea2088d` |
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
| SEC-012 | P2 | Bind service tokens to the request (`htm` method / `htu` path claims) and add a shared `jti` replay cache on mutating routes. Design: (1) receivers accept and verify the optional claims via a `verify_request(method, path, headers)` API (claims are a closed set, so receivers must ship first); (2) signers emit them behind a flag; (3) receivers require them. Touches every service's verifier and signer (~50 sites) and needs a staged deployment. Mitigations today: private network, TLS, audience + ≤ 300 s lifetime, request/correlation-id binding, replay checks in the Hub and scan gateway. |
| INV-002 | P2 | automated tests for the 48 internal / operator operations still under the frozen coverage baseline |
| PLT-008 | P2 | credential expiry webhook (last-use and `expiringSoon` already served) |
| DATA-017 | P3 | reconciliation sweepers for Messaging, Developer Platform and Numbering foreign references |

Closed after this report (commit `23f42c92`): **MSG-009** — Hub call cards for call parties without a profile at launch (materialized once on first timeline / activity read) and a deduplicated, version-monotonic realtime nudge (`communications.timeline.changed` → Messaging `timeline.changed`, members only, live and replay); tests `services/communications/tests/unified.rs::call_cards_reach_parties_without_a_profile_and_timeline_changes_are_nudged_once`, `services/messaging/tests/realtime_receipts_timeline.rs::hub_timeline_nudges_reach_members_once_per_version`. **MSG-018** — server-side receipt-event coalescing (one leading and one trailing receipt per member and window, monotonic, privacy re-checked; `receipts.query` for reconnect); tests `realtime_receipts_timeline.rs::receipt_events_are_coalesced_per_member_and_window_and_snapshots_read_current_cursors`, `load.rs::read_receipt_burst_is_coalesced` (30 readers × 40 reads: 1,200 reads → 60 receipt events). Records: `docs/operations/messaging-section-13.md`, `docs/operations/communications-hub-section-14.md`.

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
