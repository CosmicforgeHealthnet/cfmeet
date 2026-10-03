# CosmicForge RTC - Documentation

This directory contains documentation for the CosmicForge RTC backend.

## Documentation Files

### [QUICK_START.md](./QUICK_START.md)

**Quick Reference Guide**

- Project setup and configuration
- Running migrations
- Project structure overview
- Common code examples
- Development commands
- Troubleshooting

**Use this when**: You want to get the project running quickly or need quick code snippets.

---

### [DATABASE_SCHEMA.md](./DATABASE_SCHEMA.md)

**Complete Database Schema Documentation**

- All 9 database tables with field descriptions
- Entity relationships and foreign keys
- Indexes and constraints
- SeaORM model usage examples
- Security best practices

**Use this when**: You need detailed information about the database structure or how to work with models.

---

### [README_MIGRATIONS.md](./README_MIGRATIONS.md)

**Migration Guide**

- List of all migrations
- Migration commands
- How to create new migrations
- Troubleshooting migration issues

**Use this when**: You need to run, modify, or create database migrations.

---

### [SWAGGER_GUIDE.md](./SWAGGER_GUIDE.md)

**API Documentation Guide**

- How Swagger/OpenAPI works in this project
- Adding new endpoints to documentation
- Using utoipa macros
- Testing with Swagger UI

**Use this when**: You need to document new API endpoints or understand the API structure.

---

### [EMAIL_QUEUE.md](./EMAIL_QUEUE.md)

**Email Queue System**

- Architecture overview
- Adding new email types
- Email templates
- Worker configuration
- Monitoring and debugging

**Use this when**: You need to add new email types or debug email delivery.

---

### [LIVEKIT.md](./LIVEKIT.md)

**LiveKit Integration Guide**

- Architecture overview (control plane vs data plane)
- Token generation and lifecycle
- Client SDK integration examples
- Security considerations
- Troubleshooting guide

**Use this when**: You need to understand how LiveKit handles real-time video/audio, integrate client SDKs, or debug connection issues.

---

### [MEETINGS.md](./MEETINGS.md)

**Meeting Lifecycle Management**

- Meeting creation, update, and deletion
- Join meeting by UUID or meeting code
- Time-based validation (early/late join prevention)
- Auto-end worker for scheduled meetings
- Session logging and audit trail

**Use this when**: You need to implement meeting features or understand the meeting lifecycle.

---

### [PARTICIPANTS.md](./PARTICIPANTS.md)

**Participant Management**

- Listing and kicking participants
- Waiting room management (admit/deny)
- Media control (audio, video, screen sharing)
- Participant roles and permissions
- Status transitions

**Use this when**: You need to implement participant management or media control features.

---

### [CHAT.md](./CHAT.md)

**In-Meeting Chat System**

- Hybrid real-time architecture (LiveKit + REST API)
- Complete frontend implementation guide with code examples
- LiveKit data channels for instant delivery
- REST API for persistence and late joiner history
- React hook examples
- Volatile messages (deleted when meeting ends)

**Use this when**: You need to implement the chat feature in your frontend application.

---

## Database Tables

The schema includes 9 tables:

1. **users** - User accounts with local/OAuth authentication
2. **meetings** - Video conference meetings
3. **participants** - Meeting attendees (supports guests)
4. **audio_video_devices** - Media device tracking
5. **chat_messages** - In-meeting text chat
6. **session_logs** - Event logging and audit trail
7. **webhooks** - User-specific event notifications
8. **api_keys** - API access with usage tracking
9. **email_jobs** - Email queue for async delivery

## Key Features

- **UUID Primary Keys** - All tables use PostgreSQL UUIDs
- **Dual Authentication** - Local (email/password) + Google OAuth
- **Guest Support** - Anonymous users can join meetings
- **Type-Safe Models** - SeaORM entities with Rust enums
- **Email Queue** - Async email delivery with retry and dead-letter queue
- **API Management** - Usage tracking and rate limiting
- **LiveKit Integration** - Real-time video/audio via WebRTC SFU
- **Multi-Origin CORS** - Supports multiple frontend URLs for flexible deployment
- **Health Monitoring** - Built-in health check endpoint for load balancers

## Project Structure

```
backend/
├── src/
│   ├── config/         # App, email, and LiveKit configuration
│   ├── dto/            # Request/Response DTOs
│   ├── handlers/       # Request handlers (controllers)
│   ├── middleware/     # Auth middleware
│   ├── models/         # SeaORM entities
│   ├── queues/         # Job queues (email)
│   ├── routes/         # Route definitions
│   ├── services/       # Business logic
│   ├── swagger/        # OpenAPI documentation
│   ├── templates/      # Email templates
│   ├── workers/        # Background workers
│   ├── error.rs        # Error types
│   ├── state.rs        # App state
│   ├── lib.rs          # Module exports
│   └── main.rs         # Entry point
├── migration/          # Database migrations
├── docs/               # This folder
├── Cargo.toml
└── .env
```

## Reading Order

**New to the project?** Read in this order:

1. Start with **QUICK_START.md** to get the project running
2. Read **DATABASE_SCHEMA.md** for schema details
3. Check **SWAGGER_GUIDE.md** for API documentation
4. See **LIVEKIT.md** for real-time video/audio integration
5. See **MEETINGS.md** for meeting lifecycle management
6. See **PARTICIPANTS.md** for participant management
7. See **CHAT.md** for implementing the chat feature
8. See **EMAIL_QUEUE.md** if working with emails

---

**Last Updated**: 2026-01-25
