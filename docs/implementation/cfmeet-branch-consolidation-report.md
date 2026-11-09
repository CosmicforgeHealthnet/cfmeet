# CF Meet — branch consolidation report

Mandate: final repository consolidation (Phase 1). Audit date 2026-11-09. This file is also the recovery ledger for the cleanup: every deleted branch's original tip SHA is recorded below and stays reachable (see §4).

## 1. Starting state

| Item | Value |
|---|---|
| Remote heads observed | **40** (`git ls-remote --heads origin`), matching the previous independent observation |
| `origin/staging` | `ab349836fa9d34cbb1da3526483becfb5c83ee37` |
| `origin/main` | `586b88cb121d07ac0821f5d334820bae3868f6c2` |
| `origin/blueprint/final-org-events` | `b148ca9b143733a7cdf2629398423c93b95f6a48` |
| Open pull requests | one: #39 `staging` → `main` (production release; left open, not merged) |
| Worktrees / stashes / uncommitted changes | none in this clone (fresh clone; one worktree, clean, no stash) |
| CI / deploy references to non-permanent branches | `organizations.yml` and `backup-restore-drill.yml` push trigger `"blueprint/**"` (harmless glob; removed in this closure). No deploy script references a temporary branch. |

Method: the clone was first shallow, which made three branches look like unrelated history; it was unshallowed (`git fetch --unshallow`) before any comparison. For every branch: `git rev-list --count staging..<b>`, `git merge-base --is-ancestor <b> staging`, and `git cherry` / `git patch-id --stable` for any commit not reachable from `staging`.

## 2. Findings

- **36 branches are ancestors of `staging`**: every commit is already in `staging`; nothing unique.
- **4 branches `blueprint/s18-sip-race`, `-r2`, `-r3`, `-r4`** share one tip `0ee026e9` whose single extra commit is patch-identical (`git patch-id --stable`) to `e9950455` already in `staging` (the SIP connection-race fix and its regression test). Nothing unique.
- **`blueprint/final-org-events`** is the only branch with unique work: 35 commits, 0 behind `staging` (fast-forward). Its CI (run set on `bde8e596`) was green except *Part VI cross-service certification*: `organization_lifecycle_across_real_services` failed at `certification/tests/organization_lifecycle.rs:597`, an outsider read an `ORGANIZATION_ONLY` meeting-space summary with HTTP 200.
- `claude/cosmicforge-shared-scan-gateway-pawadp`, `claude/messaging-realtime-ingress` and `feature/sections-9-23` are older than `main`'s last merge base but fully contained in `staging`.
- `main` is an ancestor of `staging` (178 behind, 0 ahead).

No branch was found with active uncommitted work: there are no open PRs other than #39, no worktrees, and the latest commits on every temporary branch are already in `staging` or in `final-org-events`.

## 3. Branch manifest

| Branch | Original tip SHA | Relationship to `staging` (`ab349836`) | Unique work | Recovery action | Disposition |
|---|---|---|---|---|---|
| `blueprint/final-org-events` | `b148ca9b143733a7cdf2629398423c93b95f6a48` | 35 commits ahead, 0 behind (fast-forward) | 35 commits: org event delivery, offboarding, org Messaging, lifecycle certification, web Contacts/Messages, ORG-006 relay, API inventory, runbook | integrated into staging after J-07 authorization fix | INTEGRATED → DELETE |
| `blueprint/s02-account-security` | `dcfcf1a32afd98df9cd318f7498db97e891a3cea` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s02-s03-hardening` | `fb4900032e57935dc81501684485f6b2ed73159c` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s02-s03-sec015-guard` | `57240b27b585c7b606799c658df9b2ed1d9ac993` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s02-s03-sec016-alerts` | `7284c3fc179542ecb5e85176375020bab9f077b4` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s02-s03-sec018-roles` | `29b9c7163d35c49d5a1d8d0d502d7eeff95303e1` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s04-meeting-organizations` | `4ed16e3b8e2161bd40d3587507aff0ba2f5db71d` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s04-organizations-service` | `5062b6afd14a00fbd73931cb3dcd677989c83b6b` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s06-07-org-teams-provisioning` | `e5e96153f802ff6b1106c052dde80c1991a7b67c` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s08-workspaces` | `010018245687a6e2e7bd654a017ab1e5354ac475` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s09-s11-profile-notifications` | `a167a5f7252f2d03d663653e7b7d547eb4021d52` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s10-meetings-calendar` | `10959b7fa35c545b7c9ac1749b53e9d092d90f76` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s12-calling-lifecycle` | `bc85daf2922ec6dd654a9467a0e4a9f3c582c900` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s12-org-policy-calling-scope` | `736605fc4ae5bf7d248af1475bfe5f0ce285b293` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s13-messaging` | `4c25700443ca4c9fdfbff2b7c12f16fae75f48ad` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s14-hub-revocation` | `9592564ca1d06ea64002f81b0ae0a993380db99b` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s15-developer-platform` | `c53b60847fd851e598fbad0016342254cc401345` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s16-retirement-tooling` | `3a90d07766e7f8939764e343fc0008ee2261497c` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-developer-portal` | `977b792aa2ec5f48604d05b28865d57497a9b1f7` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-integration` | `ccb7caa923c6f0ac48ca638920df7a20ac2500d4` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-integration-r2` | `3ebaac24a2b25c75b8c00c6aa03679a8284843bd` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-integration-r3` | `3ebaac24a2b25c75b8c00c6aa03679a8284843bd` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-mobile` | `f00feef4e2acc18cd50534e99dde7fd258bf3ff5` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-web-account` | `2b3e2fb134ae4d421f4afab3d13f6f672d31554f` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-web-calling` | `24bd189f2b491645fa03a24dc84c8c8a1dc782cf` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-web-meetings` | `afce090f552637cccf2cdd59dedfdbd2e5dc2440` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s17-web-messaging` | `c0fcedcefd96101f4e95073f042287c7074ea1dd` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s18-certification` | `361585a205476e5c8e3437e1c30d14529433630e` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s18-integration` | `ab349836fa9d34cbb1da3526483becfb5c83ee37` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `blueprint/s18-sip-race` | `0ee026e9b939b84d1c33be744533d58986047d48` | 1 commit not an ancestor (0ee026e9) | none: patch-identical to staging e9950455 (git patch-id) | none needed | DELETE |
| `blueprint/s18-sip-race-r2` | `0ee026e9b939b84d1c33be744533d58986047d48` | 1 commit not an ancestor (0ee026e9) | none: patch-identical to staging e9950455 (git patch-id) | none needed | DELETE |
| `blueprint/s18-sip-race-r3` | `0ee026e9b939b84d1c33be744533d58986047d48` | 1 commit not an ancestor (0ee026e9) | none: patch-identical to staging e9950455 (git patch-id) | none needed | DELETE |
| `blueprint/s18-sip-race-r4` | `0ee026e9b939b84d1c33be744533d58986047d48` | 1 commit not an ancestor (0ee026e9) | none: patch-identical to staging e9950455 (git patch-id) | none needed | DELETE |
| `claude/cosmicforge-shared-scan-gateway-pawadp` | `18eebeba5ee26858e048eac3edeb7db1eea0dda7` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `claude/messaging-realtime-ingress` | `53e26d809e58bd47745e68410f1983176ceb2651` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `feature/sections-9-23` | `71eedfebcdac788c80a2f3743527cc953b07e257` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `fix/rtc-quarantine-wait` | `6d708f04598a70fe094d111e90f41abdf7ffd7ed` | ancestor of staging (fully merged) | none | none needed | DELETE |
| `fix/webhook-marker-false-positive` | `379c8740aaf8ccde6d86fc5809dc315b21ea492f` | ancestor of staging (fully merged) | none | none needed | DELETE |

Note: the table lists 38 branches (40 heads minus `main` and `staging`); `blueprint/final-org-events` is the only one with unique work. The closure's own temporary branch `claude/cfmeet-consolidation-closure-vk2pe3` was created for this work and is also to be removed.

## 4. Recovery and integration

| Step | Result |
|---|---|
| Unique work recovered | `blueprint/final-org-events` (35 commits, `b148ca9b`) taken as the base of the closure branch; nothing else unique existed |
| Known failure fixed before merge | J-07 disclosure (`a48b0413`); next real-process failure (J-12 harness, `84b981b1`) fixed; Part VI certification then green (run 42579359634) |
| Further closure work | SEC-004, ORG-011/MTG-016, J-08, J-11, J-18, frontend reference + guide, final documents |
| Integration into `staging` | fast-forward `ab349836` → `094090e3` (no force push, no history rewrite); later documentation commits fast-forwarded the same way |
| Excluded work | none: no branch held contradictory or unsafe unique commits; the 4 `s18-sip-race*` tips are patch-identical to `e9950455` |
| `main` | unchanged at `586b88cb`; PR #39 (`staging` → `main`) left open, not merged |
| Automation | `organizations.yml` and `backup-restore-drill.yml` no longer trigger on `blueprint/**` (`3b8b7f42`); no deploy script references a temporary branch |

## 5. Branch deletion — BLOCKED in this session

Every temporary branch was verified safe to delete (34 ancestors of `staging`, 4 patch-identical, 0 unique). Deletion failed: the session's git proxy answers `HTTP 403` to ref deletions (`git push origin --delete …`), while pushes to `staging` succeed, and the GitHub tools available to the agent have no delete-branch operation. No branch was deleted.

To finish (owner, from any clone with push rights; deletions are pinned to the recorded tips and re-verified against `staging`):

```bash
bash scripts/repository/delete-consolidated-branches.sh          # dry run: "would delete" × 39
bash scripts/repository/delete-consolidated-branches.sh --apply  # deletes; prints the remaining heads
```

Expected end state: `origin/main` and `origin/staging` only. Original tips stay recoverable: 38 of 39 are reachable from `staging`; `0ee026e9` (the `s18-sip-race*` tip) is patch-identical to `e9950455` and listed above.

## 6. Verification output (at the time of writing)

```text
$ git ls-remote --heads origin | wc -l
41
$ git rev-parse origin/staging origin/main
094090e32d994a2b8140ff446cc0195d58dc2816
586b88cb121d07ac0821f5d334820bae3868f6c2
$ git branch --list
* claude/cfmeet-consolidation-closure-vk2pe3
  staging
$ git worktree list
/home/user/cfmeet  094090e3 [claude/cfmeet-consolidation-closure-vk2pe3]
$ git status --short
(clean)
```

Remote heads are still 41 because deletion is blocked (§5); the two permanent branches exist and `staging` contains the accepted implementation.
