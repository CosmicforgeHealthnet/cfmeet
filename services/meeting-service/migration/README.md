# Migration Crate

This crate manages database migrations for the backend.

## Usage

Run migrations:

```bash
cargo run --package migration
```

Or from the main backend:

```bash
cargo run
```

## Creating New Migrations

1. Create a new migration file in `src/` following the naming pattern: `m{YYYYMMDD}_{HHMMSS}_{description}.rs`
2. Register it in `src/lib.rs` in the `Migrator::migrations()` vector
3. Implement the `up()` and `down()` methods

## Example

See `m20220101_000001_create_user.rs` for an example migration.
