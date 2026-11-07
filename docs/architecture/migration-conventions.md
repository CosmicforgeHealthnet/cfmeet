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
