# Backend Docker Testing Guide

This guide covers running the CosmicForge RTC backend using Docker.

## Prerequisites

- Docker Desktop installed and running
- `.env` file configured in the `backend/` directory

## Environment Setup

Your `.env` file should contain:

```bash
# =============================================================================
# APP ENVIRONMENT
# =============================================================================
APP_ENV=dev  # Change to "production" for prod

# =============================================================================
# DATABASE
# =============================================================================
POSTGRES_USER=postgres
POSTGRES_PASSWORD=password
POSTGRES_DB=rtc

# For local development (cargo run)
DATABASE_URL=postgresql://postgres:password@localhost:5432/rtc

# =============================================================================
# REDIS
# =============================================================================
REDIS_URL=redis://127.0.0.1:6379
REDIS_POOL_SIZE=10
REDIS_TIMEOUT_SECS=5

# =============================================================================
# SERVER
# =============================================================================
HOST=localhost
PORT=8080

# =============================================================================
# JWT
# =============================================================================
JWT_SECRET=your-secret-key-change-in-production

# =============================================================================
# GOOGLE OAUTH
# =============================================================================
GOOGLE_CLIENT_ID=your-google-client-id
GOOGLE_CLIENT_SECRET=your-google-client-secret
GOOGLE_REDIRECT_URL=http://localhost:8080/api/v1/auth/oauth/google/callback

# =============================================================================
# EMAIL (SMTP)
# =============================================================================
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=your-email@gmail.com
SMTP_FROM_NAME=CosmicForge

# =============================================================================
# FRONTEND & CORS
# =============================================================================
FRONTEND_URLS=http://localhost:3000
APP_URL=http://localhost:3000

# =============================================================================
# LIVEKIT
# =============================================================================
LIVEKIT_URL=wss://your-livekit-server.livekit.cloud
LIVEKIT_API_KEY=your-api-key
LIVEKIT_API_SECRET=your-api-secret

# =============================================================================
# LOGGING
# =============================================================================
RUST_LOG=debug
# For production: RUST_LOG=backend=info,tower_http=warn
```

## Quick Start

### Development (databases only)

Run PostgreSQL and Redis, then use `cargo run` for the backend:

```bash
cd backend

# Start databases
docker-compose up -d postgres redis

# Run migrations
docker-compose run --rm migration cargo run -- up

# Run backend locally
cargo run
```

### Production (full stack)

Run all services including the backend container:

```bash
cd backend

# Change APP_ENV in .env
# APP_ENV=production

# Start all services
docker-compose up -d

# Run migrations
docker-compose run --rm migration cargo run -- up

# Or build and start
docker-compose up -d --build
```

## View Logs

```bash
# All services
docker-compose logs

# Specific service
docker-compose logs backend
docker-compose logs postgres
docker-compose logs redis

# Follow logs
docker-compose logs -f backend

# Last 100 lines
docker-compose logs --tail=100 backend
```

## Check Service Status

```bash
docker-compose ps
```

## Testing the API

### Health Check

```bash
curl http://localhost:8080/health
```

### Register User

```bash
curl -X POST http://localhost:8080/api/v1/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "username": "testuser",
    "password": "TestPassword123!"
  }'
```

### Login

```bash
curl -X POST http://localhost:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "password": "TestPassword123!"
  }'
```

### Protected Endpoint

```bash
curl http://localhost:8080/api/v1/users/me \
  -H "Authorization: Bearer YOUR_JWT_TOKEN"
```

## Managing Services

### Stop Services

```bash
# Stop all
docker-compose down

# Stop and remove volumes (delete data)
docker-compose down -v
```

### Restart Services

```bash
# Restart all
docker-compose restart

# Restart specific service
docker-compose restart backend
```

### Rebuild After Code Changes

```bash
# Rebuild and start
docker-compose up --build

# Force rebuild without cache
docker-compose build --no-cache
docker-compose up
```

## Migrations

Run database migrations using the migration service:

```bash
# Apply all pending migrations
docker-compose run --rm migration cargo run -- up

# Check migration status
docker-compose run --rm migration cargo run -- status

# Rollback last migration
docker-compose run --rm migration cargo run -- down

# Reset database (DESTROYS ALL DATA)
docker-compose run --rm migration cargo run -- fresh
```

**Note**: The migration service uses the `tools` profile, so it won't start with `docker-compose up`. It only runs when explicitly called.

## Database Access

```bash
# Connect to PostgreSQL
docker-compose exec postgres psql -U postgres -d rtc

# Inside psql:
# \dt           - List tables
# \d users      - Describe users table
# SELECT * FROM users;
# \q            - Quit
```

## Redis Access

```bash
# Connect to Redis CLI
docker-compose exec redis redis-cli

# Inside redis-cli:
# KEYS *        - List all keys
# GET key       - Get value
# TTL key       - Check TTL
# QUIT          - Exit
```

## Reset Everything

```bash
# Stop and remove all data
docker-compose down -v

# Start fresh
docker-compose up -d postgres redis
```

## Troubleshooting

### Port Already in Use

```bash
# Find process using port
netstat -ano | findstr :8080

# Or change port in .env
PORT=8081
```

### Container Exits Immediately

```bash
# Check logs
docker-compose logs backend

# Run interactively
docker-compose run --rm backend bash
```

### Database Connection Failed

```bash
# Check if postgres is running
docker-compose ps postgres

# Check logs
docker-compose logs postgres

# Verify connection
docker-compose exec postgres pg_isready
```

### Redis Connection Failed

```bash
# Check if redis is running
docker-compose ps redis

# Test connection
docker-compose exec redis redis-cli ping
```

### Environment Variables Not Loading

```bash
# Verify .env exists
ls -la .env

# Check env in container
docker-compose exec backend env

# Restart after .env changes
docker-compose down
docker-compose up -d
```

### Missing Tables / Migration Not Run

```bash
# Check migration status
docker-compose run --rm migration cargo run -- status

# Run pending migrations
docker-compose run --rm migration cargo run -- up

# If still failing, reset and reapply (DESTROYS DATA)
docker-compose run --rm migration cargo run -- fresh
```

## Production Checklist

Before deploying:

- [ ] Set `APP_ENV=production`
- [ ] Use strong `JWT_SECRET`
- [ ] Configure production database credentials
- [ ] Set production `FRONTEND_URLS` and `APP_URL`
- [ ] Configure production Google OAuth redirect URL
- [ ] Set `RUST_LOG=backend=info,tower_http=warn`
- [ ] Use HTTPS (reverse proxy or load balancer)

## Commands Reference

```bash
# Start
docker-compose up -d                    # All services (background)
docker-compose up -d postgres redis     # Databases only
docker-compose up --build               # Rebuild and start

# Stop
docker-compose down                     # Stop containers
docker-compose down -v                  # Stop and delete data
docker-compose stop                     # Stop without removing

# Migrations
docker-compose run --rm migration cargo run -- up      # Apply migrations
docker-compose run --rm migration cargo run -- status  # Check status
docker-compose run --rm migration cargo run -- down    # Rollback last
docker-compose run --rm migration cargo run -- fresh   # Reset all

# Logs
docker-compose logs                     # All services
docker-compose logs -f backend          # Follow backend
docker-compose logs --tail=50           # Last 50 lines

# Management
docker-compose restart                  # Restart all
docker-compose ps                       # List services
docker-compose exec backend bash        # Shell access
docker-compose exec postgres psql       # Database access
docker-compose exec redis redis-cli     # Redis access

# Build
docker-compose build                    # Build all
docker-compose build --no-cache         # Clean build

# Cleanup
docker-compose down -v --remove-orphans
docker system prune -a --volumes
```
