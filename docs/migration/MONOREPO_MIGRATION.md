# Monorepo migration record

> **Current paths:** `apps/web/` and `services/meeting-service/`.
> The historical sections below intentionally retain the directory names that existed at each migration stage (`backend/`, `frontend/`, then `services/core-api/`). See the final section for the pre-Phase-2 service rename.

**Date:** 2026-10-03
**Scope:** Repository consolidation and repository-layout naming only. No application behaviour, API, database, LiveKit or authentication changes were intended.

## Sources

| Source repository | Branch / commit imported | Commits | Imported under |
|---|---|---:|---|
| `CosmicforgeHealthnet/rtc_backend` | `main` @ `05836e3` | 6 (incl. 1 merge) | `backend/` |
| `CosmicforgeHealthnet/rtc_frontend` | `main` @ `d5637b1` | 3 | `frontend/` |

The source repositories were read only and remain legacy recovery sources until deployments from `cfmeet` are certified.

## Original consolidation method

1. Fresh clone of each source repository.
2. `git filter-repo --to-subdirectory-filter backend` and `frontend` to preserve historical commits under monorepo prefixes.
3. The stale frontend gitlink was removed during the history rewrite because it had no `.gitmodules` entry or reachable source commit.
4. The two rewritten histories were merged into the new monorepo without squashing.

Old→new SHA maps remain in `commit-map-backend.txt` and `commit-map-frontend.txt`.

## Stale `rtc_frontend` gitlink

The original frontend contained a mode-160000 entry named `rtc_frontend` with no `.gitmodules` definition and no reachable source commit. It contained no source code and was intentionally excluded from the monorepo import.

## Pre-existing issues imported unchanged

These issues predate the monorepo work:

| Check | Result |
|---|---|
| `cargo fmt --check` | formatting drift in imported Rust code |
| `cargo clippy` | warnings in backend and migration crates |
| `npm run lint` | existing frontend lint errors/warnings |
| Browser console | existing hydration mismatch and `ensureUtc … endsWith` TypeError |
| Frontend fallback API URL | fallback port differs from backend default; set `NEXT_PUBLIC_API_URL` |

## Security note

The imported backend history contains an old local-development PostgreSQL password that had already been removed from current code. It was preserved with history. If it was ever reused on a real environment, rotate it there.

## First platform layout restructure

**Safety point:** `backup/pre-platform-layout` → `300383b`.

The published consolidation initially used `backend/` and `frontend/`. A normal, non-force Git restructure then changed the layout to:

| Before | After at that stage |
|---|---|
| `backend/` | `services/core-api/` |
| `frontend/` | `apps/web/` |
| `docs/DEPLOYMENT.md` | `infra/deployment/README.md` |
| `docs/MONOREPO_MIGRATION.md` | `docs/migration/MONOREPO_MIGRATION.md` |

This also established `packages/contracts/`, `infra/docker/`, `docs/architecture/`, `docs/api/`, `docs/decisions/` and `docs/phases/` as platform boundaries/documentation areas.

Historical commits still show the paths that existed when those commits were created. That is expected and preserves normal Git history.

## Pre-Phase-2 domain rename: `core-api` → `meeting-service`

Before any Phase 2 implementation began, the service name was corrected to reflect its actual domain responsibility:

```text
services/core-api/
        ↓ normal Git rename
services/meeting-service/
```

Reason: the imported Rust backend is the control plane for the Meeting & Conference Platform and is the implementation base for Phase 2. Calling it `core-api` incorrectly suggested that future calling, messaging, Privacy Relay, telephony and numbering capabilities should all live in that service.

The rename is architectural naming only. No meeting business behaviour is intentionally changed by it.

The current Phase 2 ownership is:

- backend/control plane: `services/meeting-service/`
- web UI: `apps/web/`
- shared contracts: `packages/contracts/`
- media infrastructure: `infra/livekit/`
- specification and closure evidence: `docs/phases/phase-02/`

See ADR 0002: `docs/decisions/0002-meeting-service-domain-name.md`.

### History verification

Use `--follow` across both normal rename steps, for example:

```bash
git log --follow -- services/meeting-service/src/main.rs
git log --follow -- apps/web/src/utils/timezone.ts
```

The backend history therefore naturally reads:

```text
rtc_backend → backend/ → services/core-api/ → services/meeting-service/
```

No published history is rewritten.

### Existing local checkouts

If an existing checkout was built before either backend-directory rename, run:

```bash
cd services/meeting-service
cargo clean
```

This clears Rust/Swagger debug artefacts that may contain absolute paths to an older `target/` location. Fresh clones, CI and Docker/release builds are not affected.

## Deployment roots after the final rename

- Vercel Root Directory: `apps/web`
- Meeting Service Docker build context: `services/meeting-service/`
- Meeting Service Dockerfile: `services/meeting-service/Dockerfile`

## Retiring the source repositories

Archive `rtc_backend` and `rtc_frontend` only after:

1. CI is green on the final `cfmeet` main branch;
2. the web app has deployed successfully from `apps/web`;
3. the Meeting Service has deployed successfully from `services/meeting-service/`;
4. `/health` and a full meeting join have been validated from the monorepo deployment; and
5. no newer commits need to be reconciled from the legacy repositories.
