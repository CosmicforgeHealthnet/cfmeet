# Monorepo migration record

**Date:** 2026-10-03
**Scope:** Repository consolidation only. No application behaviour, API, database,
LiveKit or authentication changes.

## Sources

| Source repository | Branch / commit imported | Commits | Imported under |
|-------------------|--------------------------|---------|----------------|
| `CosmicforgeHealthnet/rtc_backend`  | `main` @ `05836e3` | 6 (incl. 1 merge) | `backend/`  |
| `CosmicforgeHealthnet/rtc_frontend` | `main` @ `d5637b1` | 3 | `frontend/` |

Neither source repository had tags, so no tag namespacing was needed. Any future
tags imported from them should use the `backend-` / `frontend-` prefixes.
The source repositories were only read, never written to.

## Method

1. Fresh clone of each repository.
2. `git filter-repo --to-subdirectory-filter backend` (and `frontend`). Every
   commit keeps its original author, committer, dates and message. Only the
   paths, and therefore the SHAs, change.
3. For the frontend, `--path rtc_frontend --invert-paths` also drops the stale
   gitlink described below.
4. Both rewritten histories were merged into a new root commit with
   `git merge --allow-unrelated-histories --no-ff`.

Old→new SHA mappings: [`migration/commit-map-backend.txt`](migration/commit-map-backend.txt),
[`migration/commit-map-frontend.txt`](migration/commit-map-frontend.txt).

Verification:

- `git rev-parse HEAD:backend` right after the import equals the tree of
  `rtc_backend@05836e3` (`6609e12…`), so the content is byte-identical.
- `frontend/` right after the import is identical to `rtc_frontend@d5637b1`
  minus the `rtc_frontend` gitlink.
- `git log -- backend/` shows all 6 original backend commits, and
  `git log -- frontend/` shows all 3 original frontend commits.

## The `rtc_frontend` gitlink

`rtc_frontend` contained a mode-160000 entry named `rtc_frontend`
(→ `1f659c4ef4b8…`), added in the initial commit `ea86461`. Findings:

- There is no `.gitmodules`, so the entry has no URL and can't be checked out as a submodule.
- Commit `1f659c4` does not exist in `rtc_frontend`.
- On disk it is an empty directory, and nothing references it.

This is the classic result of running `git add .` while a nested `.git`
directory was present. It held no source code, so it was removed from every
historical commit and **not** carried into the monorepo.

## Changes made for the new layout

| File | Change | Why |
|------|--------|-----|
| `frontend/.gitignore` | added `!.env.example` | `.env*` would otherwise ignore the new `frontend/.env.example` |
| `frontend/.env.example` | new | documents `NEXT_PUBLIC_API_URL`; no secrets |
| root `.gitignore`, `.editorconfig` | new | monorepo-wide rules (both app `.gitignore` files kept as-is) |
| root `docker-compose.yml` | new | local deps (postgres, redis, livekit) + optional `app` profile; all ports on 127.0.0.1 |
| `infra/livekit/livekit.dev.yaml` | new | local LiveKit dev config (public dev key pair) |
| `Makefile`, `scripts/dev.sh` | new | root developer commands |
| `.github/workflows/*.yml` | new | path-aware CI (neither source repo had CI) |

**No changes were needed** in `backend/Dockerfile`, `backend/docker-compose.yml`,
`backend/.dockerignore`, Cargo manifests, migration paths, `next.config.ts`,
`tsconfig.json` or package scripts. They all use paths relative to their own
application root, which `backend/` and `frontend/` keep. The backend docs
already used `cd backend` / `cd frontend`, which now match the real layout.

## Not migrated

- The `rtc_frontend` gitlink (see above).
- The `claude/focused-wright-qpidr6` branch of `rtc_backend`, which was identical to `main`.
- Build output and dependencies (`target/`, `node_modules/`, `.next/`). None were tracked.

Kept intentionally: `backend/.claude/` and `frontend/.claude/` (local
assistant permission files that were tracked in the source repos),
`backend/DOCS_CLEANUP_SUMMARY.md` and `frontend/docs/*.html`.

## Pre-existing issues (imported unchanged)

These reproduce identically on the original repositories and are **not**
caused by the migration:

| Check | Result |
|-------|--------|
| `cargo fmt --check` | fails: formatting drift in ~30 files |
| `cargo clippy` | passes with warnings (16 backend, 2 migration) |
| `npm run lint` | fails: 3 errors / 7 warnings (`docs/generate-pdf.js` `require()`, `setState` in effect in `src/app/(room)/[id]/page.tsx`) |
| Browser console | hydration mismatch in `DashboardLayout`; `TypeError … endsWith` in `ensureUtc` (`src/utils/timezone.ts`) on the dashboard |
| Frontend default API URL | `src/services/api.ts` falls back to `http://localhost:3001/api/v1`, but the backend defaults to `:8080`, so set `NEXT_PUBLIC_API_URL` |

CI reports `cargo fmt --check` and `npm run lint` without failing the build until
dedicated fix commits land.

## Security note

`rtc_backend` history (commits `d7cdfde`, `ac4b09d`, now `519fb5e`, `e09db94`)
contains a docker-compose Postgres password for the local dev container
(`POSTGRES_PASSWORD` / `DATABASE_URL` in `docker-compose.yml` and
`docs/DOCKER_TESTING_GUIDE.md`). It was removed in `4925158`. It is a
local-container default, not a cloud credential. It is preserved in history
exactly as it is in `rtc_backend`. If that value was ever reused on a real
database, rotate it there. No other credentials were found in either history.

## Retiring the source repositories

`rtc_backend` and `rtc_frontend` are now legacy source repositories. Archive
them only after:

1. this repository is pushed to `CosmicforgeHealthnet/cfmeet` and its CI has run;
2. Vercel (Root Directory `frontend`) and the backend image build (context `backend/`) have each deployed successfully from it;
3. no new commits have landed on either source repository since the import (otherwise re-import them first).
