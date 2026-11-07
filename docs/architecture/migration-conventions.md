# Migration conventions (DATA-011, DATA-014)

Applies to every SeaORM migration crate under `services/*/migration`.

## Naming and order (enforced)

`scripts/migrations/check_migration_order.py` runs in the Service boundaries workflow on every push and pull request. For each `impl MigratorTrait`:

- Each entry's history name (an explicit `MigrationName::name()`, otherwise the module name) is `m<YYYYMMDD>_<NNNNNN>_<snake_case>`. The date must be real, and the name must not repeat an `m<YYYYMMDD>_` prefix.
- Entries sort strictly ascending in vector order. A new migration is appended at the end, with a name that sorts after the last entry. If the last entry is future-dated, the new name uses a date and sequence after it, not today's date. Don't rely on `sea-orm-cli migrate generate` timestamps.
- A name belongs to exactly one migrator (one history table) in the crate.

History names are the keys stored in the history tables, so applied names are never renamed. The three existing anomalies are frozen in the guard's `BASELINE`: the two `communications` module names and `m20260205_110126_m20260205_000001_add_guest_role`. Don't add new baseline entries; rename the new migration instead.

Known historical gaps are harmless because SeaORM orders by the vector: calling has no `000039`, and developer-platform jumps from 5 to 10. New migrations continue each crate's sequence.

## Separate additive histories (current practice, DATA-011)

Several services keep extra history tables beside the base `seaql_migrations`:

- meeting-service: `account_communications_migrations`, `account_communications_facade_migrations`, `account_security_migrations` and `meetings_calendar_migrations`;
- calling and messaging: one Communications history each;
- communications: two histories.

They exist so that the previous image, which checks for the *exact* set of applied base migrations at start-up, can still boot after an additive release. That makes each release roll back safely, but every additive change needs another history table, migrator binary and readiness check. Backups and restores must include every history (see `infra/deployment/backup/catalog.conf`).

**Target rule.** Readiness becomes "applied ⊇ migrations known to this binary, and no unknown versions". Calling, messaging and developer-platform already have `pending_migrations` helpers that fail closed on unknown versions. With that rule, one history per service is enough.

**Consolidation** copies rows between history tables (metadata only). It runs at a maintenance window after every running image uses the target rule. Until then, new additive changes keep following the separate-history pattern of their service. That pattern is safe; it is only operationally heavy.

## Destructive subcommands are guarded (SEC-015)

Every migration binary embeds the full sea-orm migration CLI, and production images ship it. `fresh`, `refresh` and `reset` drop or roll back the whole history, and most `down()`s drop tables. Each binary's `main` therefore calls `cf_migration_guard::enforce_from_env()` (`packages/migration-guard`, standard library only) as its first statement:

- `down`, `fresh`, `refresh` and `reset` exit with status 2 before anything runs, and before any database connection, unless one of the following holds:
  - `APP_ENV` is `dev`, `development`, `local`, `test` or `ci` (trimmed, case-insensitive). This is the Meeting Service development set plus `ci`. An unset value, `staging`, `production` or any other value counts as production.
  - `CF_MIGRATION_ALLOW_DESTRUCTIVE` names exactly the subcommand being run. `=down` permits `down` and `down -n N` only, never `fresh`. The binary logs the override on stderr.
- `up`, `status`, `generate`, `help`, `--help`, `--version` and no arguments (the CLI treats that as `up`) are never affected.
- The subcommand is the first argument that isn't an option or the value of `-u`/`--database-url` or `-s`/`--database-schema`, so global flags placed before it don't bypass the guard.
- Only the process environment counts. The guard runs before a binary loads `.env`, so a stray `.env` can't unlock a destructive command.

A documented production or staging rollback names the override for that one invocation, after a verified backup:

```sh
CF_MIGRATION_ALLOW_DESTRUCTIVE=down organizations-migration down -n 1
```

CI migration smoke steps and the certification harness (`certification/src/db.rs::migration`) run with `APP_ENV=test`. The service workflows and the Communications Hub workflow run `scripts/migrations/assert_destructive_guard.sh` against every built migration binary. The Meeting Service migration crate's `tests/destructive_guard.rs` covers its five binaries.

**Remaining work (not part of SEC-015).** The guard stops accidental invocations; it doesn't make a deliberate `down` safe. A `down()` that drops tables holding data should also refuse while that data exists. Many recent migrations already do this with a `RAISE EXCEPTION ... refusing to downgrade` check (for example Platform Access `m20261121_000005_first_party_organization_scopes`), and the Communications Hub histories are forward-only. Older `down()`s that drop data-bearing tables unconditionally still need an audit and the same check, tracked separately. Until then, run an overridden `down` only after a verified backup (`infra/deployment/backup/pre-migration-snapshot.sh`).
