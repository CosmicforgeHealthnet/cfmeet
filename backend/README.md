# CosmicForge RTC Backend

Real-time communication backend built with Rust, Axum, and SeaORM.

## Quick Start

### Development (Local)
```bash
# Start databases
docker-compose up -d postgres redis

# Run backend
cargo run
```

### Production (Docker)
```bash
docker-compose up -d --build
```

### Health Check
```bash
curl http://localhost:8080/health
```

## Documentation

See the [docs/](docs/) folder for detailed guides:

| Guide | Description |
|-------|-------------|
| [Quick Start](docs/QUICK_START.md) | Get up and running |
| [Docker Testing](docs/DOCKER_TESTING_GUIDE.md) | Docker setup and commands |
| [Database Schema](docs/DATABASE_SCHEMA.md) | Database structure |
| [Migrations](docs/MIGRATIONS.md) | Database migrations |
| [API Integration](docs/API_INTEGRATION.md) | API usage |
| [Swagger Guide](docs/SWAGGER_GUIDE.md) | API documentation |
| [OAuth Setup](docs/OAUTH_SETUP.md) | Google OAuth configuration |
| [LiveKit](docs/LIVEKIT.md) | WebRTC integration |
| [Meetings](docs/MEETINGS.md) | Meeting functionality |
| [Participants](docs/PARTICIPANTS.md) | Participant management |
| [Chat](docs/CHAT.md) | Chat system |
| [Email Queue](docs/EMAIL_QUEUE.md) | Email processing |
| [Logging](docs/LOGGING.md) | Logging configuration |

## Tech Stack

- **Framework**: Axum
- **ORM**: SeaORM
- **Database**: PostgreSQL
- **Cache**: Redis
- **WebRTC**: LiveKit
- **Auth**: JWT + Google OAuth
