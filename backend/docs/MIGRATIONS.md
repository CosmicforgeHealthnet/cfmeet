# CosmicForge RTC - Migration Guide

## Overview

This project uses SeaORM migrations with PostgreSQL. Migrations are located in `migration/src/`.

## Migrations

| #   | Migration                           | Table               | Description                    |
| --- | ----------------------------------- | ------------------- | ------------------------------ |
| 1   | m20260116_000001_create_users       | users               | User accounts with auth types  |
| 2   | m20260116_000002_create_meetings    | meetings            | Video conference meetings      |
| 3   | m20260116_000003_create_participants| participants        | Meeting attendees (with guests)|
| 4   | m20260116_000004_create_audio_video_devices | audio_video_devices | Media device tracking |
| 5   | m20260116_000005_create_chat_messages | chat_messages     | In-meeting chat                |
| 6   | m20260116_000006_create_session_logs | session_logs       | Event logging                  |
| 7   | m20260116_000007_create_webhooks    | webhooks            | Event notifications            |
| 8   | m20260116_000008_create_api_keys    | api_keys            | API access keys                |
| 9   | m20260117_000001_create_email_jobs  | email_jobs          | Email queue for async delivery |

## Running Migrations

```bash
cd migration

# Apply all pending migrations
cargo run -- up

# Check migration status
cargo run -- status

# Rollback last migration
cargo run -- down

# Rollback all and reapply (DESTROYS DATA)
cargo run -- fresh

# Apply specific number of migrations
cargo run -- up -n 1
```

## Creating a New Migration

### 1. Create the migration file

```bash
cd migration
cargo run -- generate create_my_table
```

This creates `migration/src/m{timestamp}_create_my_table.rs`.

### 2. Implement the migration

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MyTable::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MyTable::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(ColumnDef::new(MyTable::Name).string().not_null())
                    .col(
                        ColumnDef::new(MyTable::CreatedAt)
                            .timestamp()
                            .not_null()
                            .extra("DEFAULT CURRENT_TIMESTAMP"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MyTable::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum MyTable {
    Table,
    Id,
    Name,
    CreatedAt,
}
```

### 3. Register in lib.rs

Add to `migration/src/lib.rs`:

```rust
mod m20260117_000002_create_my_table;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            // ... existing migrations
            Box::new(m20260117_000002_create_my_table::Migration),
        ]
    }
}
```

### 4. Create the SeaORM entity

Create `src/models/my_table.rs`:

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "my_table")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

### 5. Register in models/mod.rs

```rust
pub mod my_table;
pub use my_table::Entity as MyTable;
```

## Common Patterns

### UUID Primary Key

```rust
.col(
    ColumnDef::new(Table::Id)
        .uuid()
        .not_null()
        .primary_key()
        .extra("DEFAULT gen_random_uuid()"),
)
```

### Foreign Key

```rust
.col(ColumnDef::new(Table::UserId).uuid().not_null())
.foreign_key(
    ForeignKey::create()
        .name("fk_table_user")
        .from(Table::Table, Table::UserId)
        .to(Users::Table, Users::Id)
        .on_delete(ForeignKeyAction::Cascade),
)
```

### Enum Type

```rust
// In up()
manager
    .create_type(
        Type::create()
            .as_enum(MyEnum::Type)
            .values([MyEnum::Value1, MyEnum::Value2])
            .to_owned(),
    )
    .await?;

// In down()
manager
    .drop_type(Type::drop().name(MyEnum::Type).to_owned())
    .await?;
```

### Index

```rust
manager
    .create_index(
        Index::create()
            .name("idx_table_column")
            .table(Table::Table)
            .col(Table::Column)
            .to_owned(),
    )
    .await?;
```

## Troubleshooting

### "relation already exists"

```bash
cargo run -- status  # Check what's applied
cargo run -- fresh   # Reset (destroys data)
```

### "could not connect to database"

Check `backend/.env` has correct `DATABASE_URL`.

### Migration not running

Ensure migration is registered in `migration/src/lib.rs`.

## Environment

The migration CLI loads `.env` from the backend root directory (`../`). Make sure `DATABASE_URL` is set:

```env
DATABASE_URL=postgres://user:password@localhost:5432/cosmicforge?sslmode=require
```

---

**Last Updated**: 2026-01-17
