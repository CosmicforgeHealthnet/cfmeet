# Outstanding repository work integration review

Reviewed 24 November 2026 against main `4304ed656adf96756dc0fa8f974dbc8a2ee22861`. Read-only Git/PR audit; no branches were merged or PRs closed by this review. The inventory contains 77 remote-ref rows, including the `origin/HEAD` alias shown as `origin`; 54 rows are already ancestors of main. Do not equate ahead-commit counts with missing product functionality: the historical Admin Bridge series was integrated under different commits.

## Integration decisions

No independently missing, completed, verified change was identified for immediate cherry-picking. The current Messaging PR remains intended engineering work requiring its final integration checks. Existing Admin Bridge PRs are historical source branches, mostly drafts against other historical branches; merging their entire tips would replay old contracts and release logic over newer main. This conclusion concerns source integration, not whether every feature has been activated or verified in production.

- PR #173 (`3ff1d51d`) integrated the reconciled Admin Bridge deployment; PR #174 (`ac4567c8`) integrated its real-schema backup drill. Main contains owner reads/commands, verification, audit/privacy/dataset/HQ feed and activation code from the historical series.
- PR #155 (`a2e48b40`) is superseded semantically: main already has reusable `workflow_call`, the named required-CI gate and image building parallel to that gate through the changed-service image plan. Reapplying the old one-file workflow would discard newer release planning and Admin Bridge inputs.
- PR #156 (`bbd68e7c`, `ef195fc6`, `0c24b977`, `9a9ee5d4`) is superseded by the reconciled deployment (`130a4b6f`, `3604672c`, `96f2766a`, PR #173). Main has the bridge Dockerfile, migration/bootstrap, grants, release handling and current runbook. The old standalone dark-bootstrap document is replaced by `docs/operations/admin-bridge-runbook.md`; absence of the old path is not missing functionality.
- PR #133 deploy chain is represented by main commits `3604672c`, `20c7a55a`, `b7e9cba9`, `0eba08be`, `92f71374`, `a30431df`, `86d85539`. Do not restore its old release gates.
- `claude/intelligent-cray-gghkjf` has one patch-equivalent acceptance-session reuse change: already integrated; no new merge.
- WIP `3ce6476c` on `claude/sharp-dijkstra-qgnwtn-messaging-wip` is an interrupted predecessor of main `1a422251` (Messaging server audit). Main has inbox archive/pin, retention changes, private mentions and a later hardened synchronous send-cache/block-projection implementation. Importing the interrupted 45-file patch would regress later work; its old CI is not release evidence.
- Draft report `ddf53d92` on `claude/zealous-dijkstra-e7kr7w` is superseded by main `0bcdd137`. The same report path exists, expanded by 97 lines and explicitly marked superseded by the production activation/closure report. Do not replace current evidence with the older draft.

## Semantic and historical evidence

The distinct Admin Bridge non-patch-equivalent set contains 57 commits. 49 have exact-subject counterparts on main (mapping attached in `OUTSTANDING_ADMIN_NON_EQUIVALENT_COMMITS.json`). Subject matches are historical navigation aids, not sufficient correctness proof. Additional checks below inspect current contracts and source features. Missing old `services/admin-bridge/src/routes.rs` is a refactor to `src/routes/`, including `organizations.rs` implementing `organizations.policies.read`.

All eight owner contracts retain every operation ID exposed by the historical A4 branch. This confirms route surface presence; it does not certify every current implementation branch or production configuration.

| Historical owner branch | Contract operation IDs on branch / main | Missing IDs |
|---|---|---|
| `origin/claude/admin-bridge-a4-communications` | 24 / 24 | None |
| `origin/claude/admin-bridge-a4-meeting` | 42 / 42 | None |
| `origin/claude/admin-bridge-a4-messaging` | 44 / 44 | None |
| `origin/claude/admin-bridge-a4-notifications` | 28 / 28 | None |
| `origin/claude/admin-bridge-a4-organizations` | 75 / 75 | None |
| `origin/claude/admin-bridge-a4-platform-access` | 137 / 137 | None |
| `origin/claude/admin-bridge-a4-calling` | 200 / 200 | None |
| `origin/claude/admin-bridge-a4-developer-platform` | 98 / 98 | None |

Generated-version exceptions reviewed: main Calling HTTP API is 1.35.0 (later than old 1.34.0 pin commit `c01a738e`); Meeting endpoint reference and mobile generated `apps/mobile/lib/api/generated/meeting-service.ts` are already 1.11.0. The Communications contact-notification boolean clippy rewrite is equivalent policy logic, not missing capability.

## Every open PR

Snapshot is informational and may change during the active Messaging push. Historical failed CI is recorded to prevent treating old draft heads as independently verified release candidates. Conflicting means GitHub reported a conflict against that PR's own base, which is often another old branch.

| PR | Existing head / base | Draft | Merge state | Current disposition |
|---|---|---|---|---|
| #175 | `codex/messaging-reliability-phase3` → `main` | Yes | MERGEABLE | Intended current work; integrate only after final required checks and acceptance. Current push still in progress. |
| #156 | `ops/admin-bridge-dark-bootstrap-20261121` → `main` | No | CONFLICTING | Superseded by current main implementation; preserve newer release logic, do not merge old tip. |
| #155 | `automation/green-main-auto-release-20261121` → `main` | No | CONFLICTING | Superseded by current main implementation; preserve newer release logic, do not merge old tip. |
| #154 | `claude/admin-bridge-a4-meeting` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #153 | `claude/admin-bridge-a4-calling` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: test (PostgreSQL), journeys (real processes, real service auth, Postgres, Redis). |
| #152 | `claude/admin-bridge-a4-messaging` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #150 | `claude/admin-bridge-a4-developer-platform` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: test (PostgreSQL), test (PostgreSQL), journeys (real processes, real service auth, Postgres, Redis). |
| #149 | `claude/admin-bridge-a4-platform-access` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #148 | `claude/admin-bridge-a7-a8-local` → `claude/admin-bridge-a4-wiring` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis), Vercel. |
| #147 | `claude/admin-bridge-a4-wiring` → `claude/admin-bridge-local` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis), Vercel. |
| #146 | `claude/admin-bridge-local` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis), Vercel. |
| #145 | `claude/admin-bridge-a4-notifications` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #144 | `claude/admin-bridge-a4-organizations` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #143 | `claude/admin-bridge-a4-communications` → `claude/admin-bridge-a3-verification` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: Pre-deploy tests, journeys (real processes, real service auth, Postgres, Redis). |
| #141 | `claude/admin-bridge-a3-verification` → `claude/admin-bridge-a3-core` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #140 | `claude/admin-bridge-a3-core` → `claude/admin-bridge-a2-calling` | Yes | MERGEABLE | Historical Admin Bridge chain: owner contract coverage and main counterparts present; exclude duplicate tip. Historical failures: journeys (real processes, real service auth, Postgres, Redis). |
| #133 | `claude/admin-bridge-deploy` → `claude/admin-bridge-a2-organizations` | No | CONFLICTING | Superseded by current main implementation; preserve newer release logic, do not merge old tip. Historical failures: ingress. |

## Every remote ref in the inventory

| Ref | Ahead | Head purpose | Disposition |
|---|---|---|---|
| `origin` | 0 | Chat: preview with caption before sending a file, instant photos, WhatsApp-style file previews | Already integrated by ancestry (or HEAD alias). |
| `origin/automation/green-main-auto-release-20261121` | 1 | ci: expose coordinated release as reusable workflow and overlap CI with image build | Superseded workflow features already on main; do not overwrite current planner. |
| `origin/ccr-a48695ce-yy5erg` | 0 | Contracts: cfmeet.messaging.event 1.13.0 â€” optional internal first_party on messaging.message.created (gap audit S1) | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a0` | 0 | Admin bridge A0: cfmeet.platform.common is 1.24.0 in the contracts README | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a1` | 0 | Admin bridge A1: record A0's pinnable commit on main | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a2-calling` | 29 | Admin bridge A2.7: Calling reads (bridge side) | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a2-communications` | 0 | Communications: the bundled internal contract is also 1.10.0 | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a2-developer-platform` | 0 | Admin bridge A2.1: Developer Platform operator routes name ADR 0068 | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a2-meeting` | 24 | Admin bridge A2.5: email_masked carries a pattern only a masked value matches | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a2-messaging` | 27 | Admin bridge A2.6: the discovery test's blocked example is meetings.reports.list | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a2-notifications` | 0 | Admin bridge: HMAC key files are base64url text, the same file HQ reads | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a2-organizations` | 0 | Organizations: the operator console keeps its legacy X-CF-Operator | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a2-platform-access` | 0 | Admin bridge A2.3: the decision record is ADR 0068 | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-a3-core` | 31 | Admin bridge A3.0: expected_version may be the owner's integer row version | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a3-verification` | 36 | Frontend endpoint reference: regenerate for the Calling 1.33.0 contract | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-calling` | 41 | Calling A4/A7 CI: command routes in the contract check, bounded event metric label, table ownership | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-communications` | 39 | Communications A7: abuse.report.changed.v1 owner events to the admin bridge | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-developer-platform` | 47 | Developer Platform A4: public API 1.10.0 pins, fixture-marked test tokens, pricing doc wording | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-meeting` | 50 | Meeting Service tests: the three owner-delivered export operations, and artifact refusal at its current version | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-messaging` | 43 | Messaging tests: read the message envelope's lifecycle; enable attachments in the partner environment | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-notifications` | 42 | Notifications A4/A7: test the real settlement refusal and a seeded failed-delivery day | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-organizations` | 40 | Organizations: the export download test expects invitations in creation order | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-platform-access` | 39 | Platform Access: CI expectations for the A4 admin bridge migrations | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a4-wiring` | 47 | Admin bridge 1.16.0: id-addressed reads, and commands that say their real target | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-a7-a8-local` | 55 | Admin bridge A7: Meeting Service user export and dataset pulls | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-batch-1` | 0 | Contracts: regenerate the public catalogue and SDK pins for Calling 1.34.0 | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-batch-2` | 0 | Admin bridge: contract 2.0.0, agreed with HQ | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/admin-bridge-deploy` | 21 | Backup catalog: the additive migration histories added since | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/admin-bridge-local` | 37 | Admin bridge: no schema property may be named like a secret, link or sealed blob | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/claude/cfmeet-consolidation-closure-vk2pe3` | 0 | Release: staging -> main â€” closure report production verdict (#47) | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/intelligent-cray-gghkjf` | 1 | fix: create acceptance accounts once per campaign and reuse their sessions; bounded 429 waits | Patch-equivalent to main; no missing change. |
| `origin/claude/practical-babbage-bru9w0` | 0 | Messaging: read Platform Access refusals as answers, not outages | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn` | 0 | Meeting Service test: read the reset code from its own line, not the first 6-digit run | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-activation` | 0 | Organizations: dedicated public Swagger document; backup drill covers every catalog service | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-activation-docs` | 0 | docs: closure report evidence (release 43967106423, organization acceptance 43977766389, certify 43978199476) | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-c7-governed` | 0 | Calling: device evidence applies only to its owner's pairs (C7 in governed scopes) | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-calling-features` | 0 | Merge remote-tracking branch 'origin/main' into claude/sharp-dijkstra-qgnwtn-calling-features | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-calls` | 0 | Calls: contact calls from Contacts are placed, "on another call", faster hang-up | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-chat-ux` | 0 | Chats: no browser verification, silent setup, accurate older-message labels (ADR 0067) | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-contacts` | 0 | Communications contacts audit: K1-K11, KF1-KF3 | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-e2ee` | 0 | Merge remote-tracking branch 'origin/main' into claude/sharp-dijkstra-qgnwtn-e2ee | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-gaps` | 0 | Merge remote-tracking branch 'origin/main' into claude/sharp-dijkstra-qgnwtn-gaps | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-gaps7` | 0 | CI fixes for the last 7 gaps: Telephony table count, boundary guard, Organizations clippy | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-hotfix` | 0 | Messaging: never serve one-to-one send decisions from the B2 cache | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-ids` | 0 | Merge remote-tracking branch 'origin/main' into claude/sharp-dijkstra-qgnwtn-ids | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-meeting-fixes` | 0 | feat(web, mobile): map VALIDATION_ERROR details onto form fields | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-meetings` | 0 | Meetings server audit: production CORS, legacy grace, waiting rooms, in-app notices, iTIP invites, registrations, extend and calendar feed | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-messaging` | 0 | Messaging server audit: send-policy cache, archive/pin, disappearing messages, request logs, private mentions, rich private messages | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-messaging-wip` | 1 | WIP messaging audit (interrupted by container restart): send cache, inbox archive/pin, retention, request logs | Interrupted predecessor; superseded by main 1a422251 and later safety fixes. |
| `origin/claude/sharp-dijkstra-qgnwtn-notifications` | 0 | Notifications: every user-facing notice reaches the bell | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-rtcflake` | 0 | Calling RTC certification: a publish only succeeds once the media server receives it | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-settings-dev` | 0 | Apply the account-privacy migration wherever the Meeting Service is started | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-train` | 0 | Meeting-service swagger: served Messaging pin is 107 operations, 74 native | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-verification` | 0 | diagnose: count email-ledger purge failures (A2 evidence) | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-verification-2` | 0 | Verification journey: request only scopes the application is allowed | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/sharp-dijkstra-qgnwtn-verification-3` | 0 | Verification journey: check C6 on the effective LiveKit permission | Already integrated by ancestry (or HEAD alias). |
| `origin/claude/zealous-dijkstra-e7kr7w` | 1 | Docs: Session 3 completion report (draft, production evidence pending) | Superseded historical report on main. |
| `origin/codex/admin-bridge-backup-drill` | 0 | Include Admin Bridge in the real-schema backup drill | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/admin-bridge-integration` | 0 | Synchronize bridge release operations with workflow | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/audit-production-followup` | 0 | Gracefully stop a failed media replacement before restoring its predecessor | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/calling-recording-consent` | 0 | Allow admitted app calls to exchange recording consent | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/device-local-recording` | 0 | Revert frontend recording changes at owner's request | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/messaging-reliability-phase3` | 11 | Checkpoint authenticator-protected storage and resumable history architecture in draft | Current PR #175; final checks required. |
| `origin/codex/notification-production-logout-check` | 0 | test(production): observe Calling revocation through the web adapter | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/notification-release-messaging-journey` | 0 | test(web): verify encrypted chat profile through Chat info | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/recording-callback-preparation` | 0 | Merge remote-tracking branch 'origin/main' into codex/recording-callback-preparation | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/recording-provider-activation` | 0 | Include witness origin correction in activation batch | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/recording-receipt-diagnostics` | 0 | Prepare the Meeting provider callback with idle-media and rollback guards | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/release-avatar-check` | 0 | Keep avatar compression wiring check independent of parameter names | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/runtime-audit-activation` | 0 | Patch URL stack-overflow advisory across all affected lockfiles and cover log dependencies | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/server-audit-closure` | 0 | Give the block projection migration a dated history name | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/swagger-reachability-dark` | 0 | Restore Swagger address and darken API reference | Already integrated by ancestry (or HEAD alias). |
| `origin/codex/witness-endpoint-fix` | 0 | Configure the witness base URL before Messaging appends its protocol path | Already integrated by ancestry (or HEAD alias). |
| `origin/docs/notification-n0-n1-n4-closure` | 0 | docs(notification): record N0/N1/N4 production closure | Already integrated by ancestry (or HEAD alias). |
| `origin/feat/landing-redesign` | 0 | Web: keep the security source scan clean (comment wording in the CSP builder) | Already integrated by ancestry (or HEAD alias). |
| `origin/ops/admin-bridge-dark-bootstrap-20261121` | 4 | docs: distinguish dark Admin Bridge VPS bootstrap from gated HQ activation | Historical chain; route coverage/current main counterparts present; do not replay stale tip. |
| `origin/staging` | 0 | Merge pull request #69 from CosmicforgeHealthnet/claude/sharp-dijkstra-qgnwtn | Already integrated by ancestry (or HEAD alias). |

## Limits and release follow-through

This review does not authorize bypassing required checks, activating unreviewed cryptography or assuming production state from source presence. Final main SHA, latest main CI, actual VPS service/image/migration/flag inspection and Messaging certification belong to the deployment report. No source-integration verdict is issued for PR #175 until its final implementation and checks are complete.
