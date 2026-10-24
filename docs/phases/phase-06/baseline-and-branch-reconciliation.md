# Part VI backend closure — baseline and branch reconciliation (Stage 0)

Recorded 2026-10-24. No reset, rebase, force-push or merge was performed.

## Baseline

| Item | Value |
|---|---|
| `origin/staging` at start | `4d92f24820ab3f29fa41b6658ffb3bad9e83650d` (matches the instructed starting SHA; clean tree) |
| `origin/main` at start | `454440c66a84cc643a446fdcb8fb14de30235463` |
| Merge base | `5c2430c19449bbde28993dda2bd88fe1ffde9e92` (the Part VI base, see [`README.md`](README.md) §1) |
| Divergence | `main` 21 commits ahead, `staging` 45 commits ahead |
| CI on the starting SHA | green: Part VI cross-service certification, SDK (TypeScript), SDK (Dart), Dependency and image security, Service boundaries, Messaging, Unified Messaging journeys, Web crypto artifact |

## What `main` has that `staging` does not

21 commits, all one change set: the public / private Swagger split (filtered public
specs, a private engineering catalog built off-repository, a loopback-only Nginx template,
a VPS bootstrap workflow and its gate). Files: `.github/workflows/contracts.yml`,
`.github/workflows/deploy-backend.yml`, `.github/workflows/private-docs-vps.yml` (new),
`docs/api/README.md`, `infra/deployment/private-api-docs.nginx.conf.example` (new),
`scripts/swagger/build-private-docs.py` (new), `scripts/swagger/test_private_docs.py`
(new), `services/meeting-service/src/swagger/mod.rs`.

## Reconciliation rehearsal (scratch worktree, discarded)

`git merge --no-ff origin/main` into a detached copy of `origin/staging`:

* one textual conflict, `.github/workflows/contracts.yml` — both sides only ADD path
  filters and steps; resolution is the union (keep `infra/api-gateway/**` and
  `scripts/swagger/**` in both trigger lists; keep both the Part VI route-manifest steps
  and `main`'s private-catalog step);
* `services/meeting-service/src/swagger/mod.rs` merged automatically;
* on the merged tree: `scripts/swagger/test_private_docs.py`, `public_api.py --check`
  (then 497 public routes), `bundle_openapi.py --check` and the route-catalogue
  generator tests passed. The Meeting Service swagger unit tests on the merged tree:
  see "Merged Meeting Service swagger check" below.

## Plan (requires authorization — RELEASE GATE, not performed)

1. Branch `reconcile/main-into-staging` from `staging`; `git merge --no-ff origin/main`;
   resolve `contracts.yml` by union as above (no history rewrite; a merge commit keeps
   every clone valid).
2. Regenerate nothing by hand: `python3 scripts/contracts/public_api.py --check`,
   `bundle_openapi.py --check`, `sdks/generate.py --check` must pass unchanged.
3. Run the Meeting Service suite, `scripts/swagger/test_private_docs.py`, the contracts
   workflow and the full Part VI certification on the merge commit.
4. Open a PR into `staging`; after review, `staging` contains `main`.
5. Promotion `staging` → `main` is a separate, explicitly authorized decision (Part VI
   workstream Z), after which the private-docs VPS workflow and `deploy-backend.yml` on
   `main` remain manual.

Until step 4 lands, `staging` lacks `main`'s Swagger split: the public / private
documentation behaviour on `main` is not certified together with Part VI.

## Merged Meeting Service swagger check

Pending in this file until recorded by the closure run (see
[`backend-final-closure.md`](backend-final-closure.md) §14).
