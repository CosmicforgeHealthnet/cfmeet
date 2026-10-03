# CosmicForge RTC - Database Schema Documentation

## Overview

This document provides comprehensive documentation for the CosmicForge RTC database schema. The system uses **PostgreSQL** with **UUID primary keys** and is designed with a **user-centric architecture** (no multi-tenancy).

## Key Design Principles

1. **User-Centric Architecture**: Each user is self-contained and can create meetings & generate API keys
2. **UUID Primary Keys**: All tables use UUIDs for global uniqueness
3. **Guest Support**: Participants can join meetings without being registered users
4. **Dual Authentication**: Support for both local (email/password) and OAuth (Google) authentication
5. **Comprehensive Logging**: All meeting events are tracked in session logs

## Database Tables

### 1. Users

Stores user account information with support for both local and OAuth authentication.

| Field                  | Type                                             | Constraints                              | Description                        |
| ---------------------- | ------------------------------------------------ | ---------------------------------------- | ---------------------------------- |
| id                     | UUID                                             | PRIMARY KEY, DEFAULT gen_random_uuid()   | Unique user identifier             |
| username               | VARCHAR(50)                                      | UNIQUE, NOT NULL                         | Display name                       |
| email                  | VARCHAR(100)                                     | UNIQUE, NOT NULL                         | Email address                      |
| password_hash          | VARCHAR(255)                                     | NULL                                     | Bcrypt hash (NULL for OAuth users) |
| auth_type              | ENUM('local','google')                           | NOT NULL, DEFAULT 'local'                | Authentication method              |
| role                   | ENUM('user','admin','guest')                     | NOT NULL, DEFAULT 'user'                 | User role                          |
| status                 | ENUM('pending_verification','active','inactive') | NOT NULL, DEFAULT 'pending_verification' | Account status                     |
| verification_token     | VARCHAR(6)                                       | NULL                                     | 6-digit email verification code    |
| token_expires_at       | TIMESTAMP                                        | NULL                                     | Verification token expiry          |
| reset_token            | VARCHAR(6)                                       | NULL                                     | 6-digit password reset code        |
| reset_token_expires_at | TIMESTAMP                                        | NULL                                     | Reset token expiry (1 hour)        |
| created_at             | TIMESTAMP                                        | NOT NULL, DEFAULT CURRENT_TIMESTAMP      | Account creation time              |
| updated_at             | TIMESTAMP                                        | NOT NULL, DEFAULT CURRENT_TIMESTAMP      | Last update time                   |
| last_login             | TIMESTAMP                                        | NULL                                     | Last login timestamp               |

**Indexes:**

- `idx_users_email` on `email`
- `idx_users_username` on `username`

**Relationships:**

- Has many `meetings` (as host)
- Has many `participants` (as user)
- Has many `api_keys`
- Has many `webhooks`

---

### 2. Meetings

Stores video conference meeting information.

| Field              | Type                                            | Constraints                            | Description                  |
| ------------------ | ----------------------------------------------- | -------------------------------------- | ---------------------------- |
| id                 | UUID                                            | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique meeting identifier    |
| meeting_identifier | VARCHAR(50)                                     | UNIQUE, NOT NULL                       | Human-readable meeting code  |
| user_id            | UUID                                            | NULL, FK → users(id)                   | Associated user (optional)   |
| host_id            | UUID                                            | NOT NULL, FK → users(id)               | Meeting host                 |
| title              | VARCHAR(255)                                    | NOT NULL                               | Meeting title                |
| metadata           | JSON                                            | NULL                                   | Optional metadata            |
| is_private         | BOOLEAN                                         | NOT NULL, DEFAULT FALSE                | Requires invite/waiting room |
| start_time         | TIMESTAMP                                       | NOT NULL                               | Scheduled start time         |
| end_time           | TIMESTAMP                                       | NULL                                   | Scheduled end time           |
| status             | ENUM('scheduled','ongoing','ended','cancelled') | NOT NULL, DEFAULT 'scheduled'          | Meeting lifecycle status     |
| created_at         | TIMESTAMP                                       | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Creation time                |
| updated_at         | TIMESTAMP                                       | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Last update time             |

**Indexes:**

- `idx_meetings_user_id` on `user_id`
- `idx_meetings_host_id` on `host_id`
- `idx_meetings_identifier` on `meeting_identifier`
- `idx_meetings_status` on `status`

**Relationships:**

- Belongs to `users` (as user, nullable)
- Belongs to `users` (as host)
- Has many `participants`
- Has many `chat_messages`
- Has many `session_logs`

---

### 3. Participants

Stores information about meeting participants (supports guests).

| Field             | Type                                | Constraints                            | Description                   |
| ----------------- | ----------------------------------- | -------------------------------------- | ----------------------------- |
| id                | UUID                                | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique participant identifier |
| meeting_id        | UUID                                | NOT NULL, FK → meetings(id)            | Associated meeting            |
| user_id           | UUID                                | NULL, FK → users(id)                   | Linked user (NULL for guests) |
| role              | ENUM('host','participant','viewer') | NOT NULL, DEFAULT 'participant'        | Meeting role                  |
| join_time         | TIMESTAMP                           | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Join timestamp                |
| leave_time        | TIMESTAMP                           | NULL                                   | Leave timestamp               |
| status            | ENUM('waiting','joined','kicked')   | NOT NULL, DEFAULT 'waiting'            | Current state                 |
| is_muted          | BOOLEAN                             | NOT NULL, DEFAULT FALSE                | Audio mute state              |
| is_video_on       | BOOLEAN                             | NOT NULL, DEFAULT TRUE                 | Video state                   |
| is_screen_sharing | BOOLEAN                             | NOT NULL, DEFAULT FALSE                | Screen share state            |
| display_name      | VARCHAR(100)                        | NOT NULL                               | Name shown in meeting         |

**Indexes:**

- `idx_participants_meeting_id` on `meeting_id`
- `idx_participants_user_id` on `user_id`
- `idx_participants_status` on `status`

**Relationships:**

- Belongs to `meetings`
- Belongs to `users` (nullable for guests)
- Has many `audio_video_devices`
- Has many `chat_messages`
- Has many `session_logs`

---

### 4. Audio/Video Devices

Stores participant device information.

| Field          | Type                                             | Constraints                            | Description              |
| -------------- | ------------------------------------------------ | -------------------------------------- | ------------------------ |
| id             | UUID                                             | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique device identifier |
| participant_id | UUID                                             | NOT NULL, FK → participants(id)        | Linked participant       |
| device_type    | ENUM('audio_input','audio_output','video_input') | NOT NULL                               | Device type              |
| device_name    | VARCHAR(100)                                     | NOT NULL                               | Device name              |
| is_active      | BOOLEAN                                          | NOT NULL, DEFAULT TRUE                 | Active device flag       |
| created_at     | TIMESTAMP                                        | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Creation time            |

**Indexes:**

- `idx_devices_participant_id` on `participant_id`

**Relationships:**

- Belongs to `participants`

---

### 5. Chat Messages

Stores in-meeting chat messages.

| Field          | Type      | Constraints                            | Description               |
| -------------- | --------- | -------------------------------------- | ------------------------- |
| id             | UUID      | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique message identifier |
| meeting_id     | UUID      | NOT NULL, FK → meetings(id)            | Associated meeting        |
| participant_id | UUID      | NOT NULL, FK → participants(id)        | Message sender            |
| message        | TEXT      | NOT NULL                               | Chat content              |
| created_at     | TIMESTAMP | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Message timestamp         |

**Indexes:**

- `idx_chat_meeting_id` on `meeting_id`
- `idx_chat_created_at` on `created_at`

**Relationships:**

- Belongs to `meetings`
- Belongs to `participants`

---

### 6. Session Logs

Stores meeting event logs.

| Field          | Type      | Constraints                            | Description                                            |
| -------------- | --------- | -------------------------------------- | ------------------------------------------------------ |
| id             | UUID      | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique log identifier                                  |
| meeting_id     | UUID      | NOT NULL, FK → meetings(id)            | Associated meeting                                     |
| participant_id | UUID      | NULL, FK → participants(id)            | Associated participant (NULL for meeting-level events) |
| event_type     | ENUM      | NOT NULL                               | Event type (see below)                                 |
| event_time     | TIMESTAMP | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Event timestamp                                        |
| metadata       | JSON      | NULL                                   | Additional event data                                  |

**Event Types:**

- `meeting_start`
- `meeting_end`
- `participant_join`
- `participant_leave`
- `role_change`
- `screen_share_start`
- `screen_share_end`

**Indexes:**

- `idx_logs_meeting_id` on `meeting_id`
- `idx_logs_event_time` on `event_time`
- `idx_logs_event_type` on `event_type`

**Relationships:**

- Belongs to `meetings`
- Belongs to `participants` (nullable)

---

### 7. Webhooks

Stores webhook configuration for event notifications.

| Field        | Type                      | Constraints                            | Description               |
| ------------ | ------------------------- | -------------------------------------- | ------------------------- |
| id           | UUID                      | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique webhook identifier |
| user_id      | UUID                      | NOT NULL, FK → users(id)               | Webhook owner             |
| event_type   | ENUM                      | NOT NULL                               | Event subscription type   |
| endpoint_url | VARCHAR(255)              | NOT NULL                               | Webhook endpoint          |
| secret       | VARCHAR(64)               | NOT NULL                               | Payload signing secret    |
| status       | ENUM('active','inactive') | NOT NULL, DEFAULT 'active'             | Webhook state             |
| created_at   | TIMESTAMP                 | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Creation time             |
| updated_at   | TIMESTAMP                 | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Last update time          |

**Event Types:**

- `meeting_start`
- `meeting_end`
- `participant_join`
- `participant_leave`

**Indexes:**

- `idx_webhooks_user_id` on `user_id`
- `idx_webhooks_event_type` on `event_type`

**Relationships:**

- Belongs to `users`

---

### 8. API Keys

Stores API access keys with usage tracking.

| Field       | Type                     | Constraints                            | Description           |
| ----------- | ------------------------ | -------------------------------------- | --------------------- |
| id          | UUID                     | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique key identifier |
| user_id     | UUID                     | NOT NULL, FK → users(id)               | Key owner             |
| api_key     | VARCHAR(64)              | UNIQUE, NOT NULL                       | API key string        |
| usage_limit | INTEGER                  | NOT NULL, DEFAULT 1000                 | Max calls per month   |
| used_count  | INTEGER                  | NOT NULL, DEFAULT 0                    | Current usage count   |
| expires_at  | TIMESTAMP                | NOT NULL                               | Expiration date       |
| status      | ENUM('active','revoked') | NOT NULL, DEFAULT 'active'             | Key status            |
| created_at  | TIMESTAMP                | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Creation time         |
| updated_at  | TIMESTAMP                | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Last update time      |

**Indexes:**

- `idx_api_keys_user_id` on `user_id`
- `idx_api_keys_api_key` on `api_key`
- `idx_api_keys_status` on `status`

**Relationships:**

- Belongs to `users`

---

## Relationship Summary

### Entity Relationship Diagram (Text)

```
Users (1:N)
├── Meetings (as user) (1:N) - nullable
├── Meetings (as host) (1:N)
│   ├── Participants (1:N)
│   │   ├── AudioVideoDevices (1:N)
│   │   ├── ChatMessages (1:N)
│   │   └── SessionLogs (1:N)
│   ├── ChatMessages (1:N)
│   └── SessionLogs (1:N)
├── Participants (nullable for guests) (1:N)
├── Webhooks (1:N)
└── API Keys (1:N)
```

### Cardinality Table

| Table 1      | Relationship | Table 2            | Cardinality | On Delete |
| ------------ | ------------ | ------------------ | ----------- | --------- |
| Users        | has          | Meetings (as user) | 1:N         | SET NULL  |
| Users        | hosts        | Meetings (as host) | 1:N         | CASCADE   |
| Meetings     | has          | Participants       | 1:N         | CASCADE   |
| Users        | linked to    | Participants       | 1:N         | SET NULL  |
| Participants | has          | AudioVideoDevices  | 1:N         | CASCADE   |
| Meetings     | has          | ChatMessages       | 1:N         | CASCADE   |
| Participants | sends        | ChatMessages       | 1:N         | CASCADE   |
| Meetings     | has          | SessionLogs        | 1:N         | CASCADE   |
| Participants | linked to    | SessionLogs        | 1:N         | SET NULL  |
| Users        | configures   | Webhooks           | 1:N         | CASCADE   |
| Users        | owns         | APIKeys            | 1:N         | CASCADE   |

---

## Foreign Key Actions

### CASCADE

When a parent record is deleted, all child records are automatically deleted:

- Users → Meetings (as host)
- Meetings → Participants
- Participants → AudioVideoDevices
- Meetings → ChatMessages
- Participants → ChatMessages
- Meetings → SessionLogs
- Users → Webhooks
- Users → APIKeys

### SET NULL

When a parent record is deleted, the foreign key in child records is set to NULL:

- Users → Meetings (as user)
- Users → Participants
- Participants → SessionLogs

---

## Security Considerations

### 1. Password Security

- Passwords are hashed using bcrypt with default cost factor
- OAuth users have NULL password_hash
- Never expose password_hash in API responses

### 2. API Key Security

- API keys should be generated using cryptographically secure random generators
- Store API keys securely (consider hashing)
- Validate API key status and expiration before use
- Check usage limits before processing requests

### 3. Webhook Security

- Use webhook secrets to sign payloads
- Verify webhook signatures on the receiving end
- Use HTTPS for webhook endpoints

### 4. Guest Access

- Guests (participants with NULL user_id) have limited privileges
- Implement proper authorization checks for guest users
- Consider rate limiting for guest actions

---

## Best Practices

### 1. Querying Data

Always use indexes when filtering:

```rust
// ✅ Good - Uses indexed column
Users::find()
    .filter(users::Column::Email.eq(email))
    .one(db).await?;

// ✅ Good - Uses indexed column
Meetings::find()
    .filter(meetings::Column::Status.eq(MeetingStatus::Ongoing))
    .all(db).await?;
```

### 2. Guest Participants

Handle nullable user_id properly:

```rust
// Check if participant is a guest
if participant.user_id.is_none() {
    // Handle guest participant
} else {
    // Handle registered user
}
```

### 3. Enum Usage

Use type-safe enums from the models:

```rust
use crate::models::users::{AuthType, UserRole, UserStatus};

let user = users::ActiveModel {
    auth_type: Set(AuthType::Local),
    role: Set(UserRole::User),
    status: Set(UserStatus::Active),
    // ...
};
```

### 4. Timestamps

Always update `updated_at` when modifying records:

```rust
user.updated_at = Set(chrono::Utc::now().naive_utc());
user.update(db).await?;
```

---

## Migration Commands

### Run Migrations

```bash
cd migration
cargo run -- up
```

### Check Migration Status

```bash
cargo run -- status
```

### Rollback Last Migration

```bash
cargo run -- down
```

### Reset Database (Drop all tables and reapply)

```bash
cargo run -- fresh
```

---

---

### 9. Email Jobs

Stores queued email jobs for async processing with retry support.

| Field           | Type                                                       | Constraints                            | Description                      |
| --------------- | ---------------------------------------------------------- | -------------------------------------- | -------------------------------- |
| id              | UUID                                                       | PRIMARY KEY, DEFAULT gen_random_uuid() | Unique job identifier            |
| idempotency_key | VARCHAR(255)                                               | UNIQUE, NOT NULL                       | Key to prevent duplicate sends   |
| to_email        | VARCHAR(255)                                               | NOT NULL                               | Recipient email address          |
| to_name         | VARCHAR(255)                                               | NULL                                   | Recipient display name           |
| subject         | VARCHAR(500)                                               | NOT NULL                               | Email subject line               |
| html_body       | TEXT                                                       | NOT NULL                               | HTML version of email body       |
| text_body       | TEXT                                                       | NOT NULL                               | Plain text version of email body |
| status          | ENUM('pending','processing','sent','failed','dead_letter') | NOT NULL, DEFAULT 'pending'            | Job processing status            |
| retry_count     | INTEGER                                                    | NOT NULL, DEFAULT 0                    | Number of send attempts          |
| max_retries     | INTEGER                                                    | NOT NULL, DEFAULT 3                    | Maximum retry attempts           |
| next_retry_at   | TIMESTAMP                                                  | NULL                                   | When to attempt next retry       |
| last_error      | TEXT                                                       | NULL                                   | Last error message if failed     |
| created_at      | TIMESTAMP                                                  | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Job creation time                |
| updated_at      | TIMESTAMP                                                  | NOT NULL, DEFAULT CURRENT_TIMESTAMP    | Last update time                 |
| sent_at         | TIMESTAMP                                                  | NULL                                   | When email was successfully sent |

**Indexes:**

- `idx_email_jobs_status_next_retry` on `(status, next_retry_at)`
- `idx_email_jobs_idempotency_key` on `idempotency_key`

**Status Values:**

- `pending` - Waiting to be processed
- `processing` - Currently being processed by a worker
- `sent` - Successfully sent
- `failed` - Failed but may retry
- `dead_letter` - Failed permanently after max retries

**See Also**: [EMAIL_QUEUE.md](./EMAIL_QUEUE.md) for full email system documentation.

---

## Schema Version

**Version**: 1.2.0
**Last Updated**: 2026-01-18
**Total Tables**: 9
**Architecture**: User-Centric (No Multi-Tenancy)

### Changelog

| Version | Date       | Changes                                                   |
| ------- | ---------- | --------------------------------------------------------- |
| 1.2.0   | 2026-01-18 | Added `reset_token` and `reset_token_expires_at` to Users |
| 1.1.0   | 2026-01-17 | Added Email Jobs table for async email processing         |
| 1.0.0   | 2026-01-17 | Initial schema with Users, Meetings, Participants, etc.   |
