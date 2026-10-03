# CosmicForge RTC - Quick Start Guide

## Getting Started

### Prerequisites

- Rust 1.70+ installed
- PostgreSQL 14+ database
- Visual Studio Build Tools (Windows)

### Initial Setup

1. **Navigate to backend**:

   ```bash
   cd backend
   ```

2. **Configure environment variables**:
   Create/edit `.env` file:

   ```env
   # Environment
   APP_ENV=dev

   # Database
   DATABASE_URL=postgres://username:password@host:port/database?sslmode=require

   # Server
   JWT_SECRET=your-secret-key-change-this-in-production
   HOST=127.0.0.1
   PORT=8080

   # Frontend URLs (comma-separated for CORS)
   FRONTEND_URLS=http://localhost:3000,http://localhost:3001

   # Primary app URL (used for email links, join URLs, etc.)
   APP_URL=http://localhost:3000

   # Backend URL (for production Swagger UI - only needed when APP_ENV=prod)
   # BACKEND_URL=https://cosmicforge-rtc-api.vercel.app

   # Optional: Email configuration
   SMTP_HOST=smtp.gmail.com
   SMTP_PORT=587
   SMTP_USERNAME=your-email@gmail.com
   SMTP_PASSWORD=your-app-password
   SMTP_FROM_EMAIL=noreply@cosmicforge.com
   SMTP_FROM_NAME=CosmicForge
   ```

3. **Build the project**:

   ```bash
   cargo build
   ```

4. **Run migrations**:

   ```bash
   cd migration
   cargo run -- up
   cd ..
   ```

5. **Start the server**:

   ```bash
   cargo run
   ```

6. **Access the API**:
   - Root: http://localhost:8080/
   - Health Check: http://localhost:8080/health
   - API: http://localhost:8080/api/v1
   - Swagger UI: http://localhost:8080/swagger-ui
   - OpenAPI Spec: http://localhost:8080/api-docs/openapi.json

## Database Migrations

### Running Migrations

```bash
cd migration

# Apply all pending migrations
cargo run -- up

# Rollback last migration
cargo run -- down

# Check migration status
cargo run -- status

# Reset database (drop all + reapply)
cargo run -- fresh
```

### Migration Order

1. Users
2. Meetings
3. Participants
4. Audio/Video Devices
5. Chat Messages
6. Session Logs
7. Webhooks
8. API Keys
9. Email Jobs

## Project Structure

```
backend/
├── src/
│   ├── config/           # Configuration (app, email)
│   │   ├── mod.rs
│   │   ├── app.rs        # AppConfig (db, jwt, host, port)
│   │   └── email.rs      # EmailConfig (SMTP settings)
│   ├── dto/              # Request/Response DTOs
│   │   ├── mod.rs
│   │   ├── auth.rs       # Auth DTOs
│   │   └── common.rs     # Common DTOs
│   ├── handlers/         # Request handlers
│   │   ├── mod.rs
│   │   ├── auth.rs       # Auth handlers
│   │   └── meetings.rs   # Meeting handlers
│   ├── middleware/       # Middleware
│   │   ├── mod.rs
│   │   └── auth.rs       # JWT auth middleware
│   ├── models/           # SeaORM entities
│   │   ├── mod.rs
│   │   ├── users.rs
│   │   ├── meetings.rs
│   │   ├── participants.rs
│   │   ├── audio_video_devices.rs
│   │   ├── chat_messages.rs
│   │   ├── session_logs.rs
│   │   ├── webhooks.rs
│   │   ├── api_keys.rs
│   │   └── email_jobs.rs
│   ├── queues/           # Job queues
│   │   ├── mod.rs
│   │   └── email.rs      # Email queue
│   ├── routes/           # API routes
│   │   ├── mod.rs
│   │   ├── auth.rs
│   │   └── meetings.rs
│   ├── services/         # Business logic
│   │   ├── mod.rs
│   │   ├── auth.rs       # Auth service
│   │   └── email.rs      # Email service
│   ├── swagger/          # OpenAPI/Swagger
│   │   └── mod.rs
│   ├── templates/        # Email templates
│   │   ├── mod.rs
│   │   └── email/
│   │       ├── mod.rs
│   │       ├── base.rs
│   │       ├── verification.rs
│   │       ├── password_reset.rs
│   │       ├── welcome.rs
│   │       └── notification.rs
│   ├── workers/          # Background workers
│   │   ├── mod.rs
│   │   └── email.rs      # Email worker
│   ├── error.rs          # Error types
│   ├── state.rs          # App state
│   ├── lib.rs            # Module exports
│   └── main.rs           # Entry point
├── migration/
│   └── src/
│       ├── lib.rs        # Migration registry
│       ├── main.rs       # Migration CLI
│       └── m2026*.rs     # Individual migrations
├── docs/                 # Documentation
├── Cargo.toml
└── .env
```

## Database Tables Overview

| Table                   | Purpose             | Key Features                     |
| ----------------------- | ------------------- | -------------------------------- |
| **users**               | User accounts       | Local + OAuth auth, verification |
| **meetings**            | Video conferences   | Lifecycle status, metadata       |
| **participants**        | Meeting attendees   | Guest support, real-time states  |
| **audio_video_devices** | Media devices       | Device tracking per participant  |
| **chat_messages**       | In-meeting chat     | Text communication               |
| **session_logs**        | Event logging       | Comprehensive audit trail        |
| **webhooks**            | Event notifications | User-specific, signed payloads   |
| **api_keys**            | API access          | Usage tracking, expiration       |
| **email_jobs**          | Email queue         | Retry, dead-letter, idempotency  |

## Code Examples

### Create a User (Local Auth)

```rust
use crate::models::users::{self, AuthType, UserRole, UserStatus};
use sea_orm::*;
use uuid::Uuid;

let user = users::ActiveModel {
    id: Set(Uuid::new_v4()),
    username: Set("john_doe".to_owned()),
    email: Set("john@example.com".to_owned()),
    password_hash: Set(Some(hashed_password)),
    auth_type: Set(AuthType::Local),
    role: Set(UserRole::User),
    status: Set(UserStatus::PendingVerification),
    ..Default::default()
};

let result = user.insert(db).await?;
```

### Create a Meeting

```rust
use crate::models::meetings::{self, MeetingStatus};

let meeting = meetings::ActiveModel {
    id: Set(Uuid::new_v4()),
    meeting_identifier: Set("MTG-12345".to_owned()),
    host_id: Set(user_id),
    title: Set("Team Standup".to_owned()),
    is_private: Set(false),
    start_time: Set(chrono::Utc::now().naive_utc()),
    status: Set(MeetingStatus::Scheduled),
    ..Default::default()
};

let result = meeting.insert(db).await?;
```

### Add a Guest Participant

```rust
use crate::models::participants::{self, ParticipantRole, ParticipantStatus};

let participant = participants::ActiveModel {
    id: Set(Uuid::new_v4()),
    meeting_id: Set(meeting_id),
    user_id: Set(None), // NULL for guests
    role: Set(ParticipantRole::Participant),
    display_name: Set("Guest User".to_owned()),
    status: Set(ParticipantStatus::Joined),
    ..Default::default()
};

let result = participant.insert(db).await?;
```

### Send an Email (via Queue)

```rust
// Emails are queued, not sent immediately
let job_id = state.email_service
    .send_verification_email(&email, &username, &token)
    .await?;
```

## API Endpoints

### Authentication (`/api/v1/auth`)

| Method | Endpoint        | Auth | Description              |
| ------ | --------------- | ---- | ------------------------ |
| POST   | `/register`     | No   | Register new user        |
| POST   | `/verify-email` | No   | Verify email with token  |
| POST   | `/login`        | No   | Login and get JWT tokens |
| POST   | `/refresh`      | No   | Refresh access token     |
| POST   | `/logout`       | Yes  | Logout user              |

### Meetings (`/api/v1/meetings`)

| Method | Endpoint     | Auth | Description                  |
| ------ | ------------ | ---- | ---------------------------- |
| GET    | `/`          | Yes  | List meetings (paginated)    |
| POST   | `/`          | Yes  | Create meeting               |
| GET    | `/:id`       | Yes  | Get meeting details          |
| PUT    | `/:id`       | Yes  | Update meeting (host only)   |
| DELETE | `/:id`       | Yes  | Delete meeting (host only)   |
| POST   | `/:id/start` | Yes  | Start meeting (host only)    |
| POST   | `/:id/end`   | Yes  | End meeting (host only)      |
| POST   | `/:id/join`  | No   | Join meeting (guest support) |

## Development Commands

```bash
# Build project
cargo build

# Run in development mode
cargo run

# Run tests
cargo test

# Format code
cargo fmt

# Lint code
cargo clippy

# Check without building
cargo check

# Build for production
cargo build --release
```

## Troubleshooting

### Migration Issues

**Problem**: "relation already exists"

```bash
cd migration
cargo run -- status
# If needed, reset (CAUTION: destroys data)
cargo run -- fresh
```

**Problem**: "could not connect to database"

- Check `.env` file has correct `DATABASE_URL`
- Verify database is running and accessible
- Check SSL mode matches your database configuration

### Build Issues

**Problem**: "linking with link.exe failed"

- Install Visual Studio Build Tools with "C++ build tools" workload
- Restart terminal after installation

**Problem**: "could not compile"

```bash
cargo clean
cargo build
```

### Email Issues

**Problem**: Emails not sending

- Check SMTP configuration in `.env`
- Check server logs for worker startup message
- Query `email_jobs` table to see job status

---

**Last Updated**: 2026-01-17
