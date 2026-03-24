# Backend Docker Testing Guide

This guide will help you test the CosmicForge RTC backend using Docker and Docker Compose.

## Prerequisites

- Docker Desktop installed and running
- `.env` file configured in the `backend/` directory

## Environment Setup

Ensure your `.env` file in the `backend/` directory contains all required configuration:

```bash
# Database Configuration
# For local Docker testing, this will be overridden by docker-compose.yml
DATABASE_URL=postgresql://cosmicforge:cosmicforge123@postgres:5432/cosmicforge_rtc

# Server Configuration
HOST=0.0.0.0
PORT=8080

# Application URL (Frontend on Vercel)
APP_URL=https://your-app.vercel.app

# JWT Secret
JWT_SECRET=your-super-secret-jwt-key-change-this-in-production

# Google OAuth
GOOGLE_CLIENT_ID=your-google-client-id
GOOGLE_CLIENT_SECRET=your-google-client-secret
GOOGLE_REDIRECT_URL=https://your-backend-domain.com/api/v1/auth/google/callback

# Email Configuration
EMAIL_VERIFICATION_REQUIRED=true
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=noreply@cosmicforge.com
SMTP_FROM_NAME=CosmicForge RTC

# LiveKit Configuration
LIVEKIT_URL=wss://your-livekit-server.com
LIVEKIT_API_KEY=your-livekit-api-key
LIVEKIT_API_SECRET=your-livekit-api-secret

# Application Environment
APP_ENV=dev

# Timezone (Optional, defaults to UTC)
TIMEZONE=Africa/Lagos
```

## Quick Start with Docker Compose

### 1. Navigate to Backend Directory

```bash
cd backend
```

### 2. Build and Start Services

```bash
# Build and start all services (PostgreSQL + Backend)
docker-compose up --build

# Or run in detached mode (background)
docker-compose up --build -d
```

This will:

- ✅ Start PostgreSQL database container
- ✅ Wait for database to be healthy
- ✅ Build the backend Docker image
- ✅ Run database migrations
- ✅ Start the backend server

### 3. View Logs

```bash
# View logs from all services
docker-compose logs

# View backend logs only
docker-compose logs backend

# Follow logs in real-time
docker-compose logs -f backend

# View last 100 lines
docker-compose logs --tail=100 backend
```

### 4. Check Service Status

```bash
# List running containers
docker-compose ps

# Check if services are healthy
docker-compose ps
```

## Testing the Backend API

### Test Root Endpoint

```bash
curl http://localhost:8080/
```

**Expected Response:**

```json
{
  "message": "CosmicForge RTC API",
  "version": "1.0.0",
  "status": "running"
}
```

### Test Health Check

```bash
curl http://localhost:8080/health
```

### Test Authentication Endpoints

#### Register a New User

```bash
curl -X POST http://localhost:8080/api/v1/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "password": "TestPassword123!",
    "full_name": "Test User"
  }'
```

#### Login

```bash
curl -X POST http://localhost:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "password": "TestPassword123!"
  }'
```

**Save the JWT token from the response for authenticated requests.**

#### Test Protected Endpoint

```bash
# Replace YOUR_JWT_TOKEN with the token from login response
curl http://localhost:8080/api/v1/users/me \
  -H "Authorization: Bearer YOUR_JWT_TOKEN"
```

### Test Meeting Endpoints

#### Create a Meeting

```bash
curl -X POST http://localhost:8080/api/v1/meetings \
  -H "Authorization: Bearer YOUR_JWT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Test Meeting",
    "description": "Docker test meeting",
    "scheduled_start": "2026-02-10T14:00:00Z",
    "scheduled_end": "2026-02-10T15:00:00Z",
    "is_private": false
  }'
```

#### List Meetings

```bash
curl http://localhost:8080/api/v1/meetings \
  -H "Authorization: Bearer YOUR_JWT_TOKEN"
```

## Managing Docker Services

### Stop Services

```bash
# Stop all services
docker-compose down

# Stop services and remove volumes (delete database data)
docker-compose down -v
```

### Restart Services

```bash
# Restart all services
docker-compose restart

# Restart backend only
docker-compose restart backend
```

### Rebuild After Code Changes

```bash
# Rebuild and restart
docker-compose up --build

# Force rebuild without cache
docker-compose build --no-cache
docker-compose up
```

## Database Management

### Access PostgreSQL Database

```bash
# Connect to PostgreSQL container
docker-compose exec postgres psql -U cosmicforge -d cosmicforge_rtc

# Inside psql, you can run SQL commands:
# \dt           - List all tables
# \d users      - Describe users table
# SELECT * FROM users;
# \q            - Quit
```

### Run Migrations Manually

```bash
# Access backend container
docker-compose exec backend bash

# Run migrations
./migration up

# Or rollback
./migration down
```

### Reset Database

```bash
# Stop services and remove volumes
docker-compose down -v

# Start fresh
docker-compose up --build
```

## Troubleshooting

### ✗ Port 8080 Already in Use

```bash
# Find process using port 8080
netstat -ano | findstr :8080

# Kill the process or change the port in docker-compose.yml
# Modify ports section: - "8081:8080"
```

### ✗ Container Exits Immediately

```bash
# Check logs for errors
docker-compose logs backend

# Run container interactively
docker-compose run --rm backend bash
```

### ✗ Database Connection Failed

```bash
# Check if PostgreSQL is running
docker-compose ps postgres

# Check PostgreSQL logs
docker-compose logs postgres

# Verify DATABASE_URL in docker-compose.yml
# Should be: postgresql://cosmicforge:cosmicforge123@postgres:5432/cosmicforge_rtc
```

### ✗ Environment Variables Not Loading

```bash
# Check if .env file exists in backend directory
ls -la .env

# View environment variables in container
docker-compose exec backend env

# Restart services after .env changes
docker-compose down
docker-compose up
```

### ✗ Build Fails

```bash
# Clean build with no cache
docker-compose build --no-cache

# Remove old images
docker image prune -a

# Rebuild
docker-compose up --build
```

### ✗ Changes Not Reflected

```bash
# Ensure you rebuild after code changes
docker-compose up --build

# Or force rebuild
docker-compose build --no-cache backend
docker-compose up
```

## Advanced Usage

### Run Specific Service

```bash
# Start only PostgreSQL
docker-compose up postgres

# Start only backend (requires postgres to be running)
docker-compose up backend
```

### Execute Commands in Running Container

```bash
# Access backend shell
docker-compose exec backend bash

# Run a command without entering the container
docker-compose exec backend ls -la
```

### View Resource Usage

```bash
# Check CPU and memory usage
docker stats
```

### Clean Up Everything

```bash
# Stop all containers and remove everything
docker-compose down -v --remove-orphans

# Remove all unused Docker resources
docker system prune -a --volumes
```

## Production Deployment Differences

When deploying to production:

1. **Use External Database**: Comment out the `DATABASE_URL` override in `docker-compose.yml` and use Aiven Cloud PostgreSQL or other managed database
2. **Remove PostgreSQL Service**: You don't need the postgres service in production if using a managed database
3. **Update APP_URL**: Set to your actual frontend URL on Vercel
4. **Update GOOGLE_REDIRECT_URL**: Use your production backend domain
5. **Set Strong Secrets**: Use cryptographically secure values for `JWT_SECRET`
6. **Use Production LiveKit**: Configure production LiveKit credentials
7. **HTTPS/TLS**: Use a reverse proxy (nginx, Traefik) or cloud load balancer for SSL termination

## Testing Checklist

Before deploying to production:

- [ ] Backend starts without errors
- [ ] Database migrations run successfully
- [ ] Root endpoint responds
- [ ] User registration works
- [ ] User login returns JWT token
- [ ] Protected endpoints require authentication
- [ ] Meeting creation works
- [ ] Meeting listing works
- [ ] Email verification emails are sent
- [ ] Google OAuth callback works
- [ ] LiveKit token generation works
- [ ] All environment variables are set correctly

## Next Steps

1. ✅ Test all API endpoints locally
2. ✅ Verify email functionality
3. ✅ Test OAuth flows
4. ✅ Deploy backend to production (Railway, Fly.io, AWS, etc.)
5. ✅ Deploy frontend to Vercel
6. ✅ Update frontend environment variables with production backend URL
7. ✅ Set up monitoring and logging

## Useful Commands Reference

```bash
# Start services
docker-compose up                    # Foreground
docker-compose up -d                 # Background
docker-compose up --build            # Rebuild and start

# Stop services
docker-compose down                  # Stop and remove containers
docker-compose down -v               # Also remove volumes
docker-compose stop                  # Stop without removing

# View logs
docker-compose logs                  # All services
docker-compose logs -f backend       # Follow backend logs
docker-compose logs --tail=50        # Last 50 lines

# Service management
docker-compose restart               # Restart all
docker-compose restart backend       # Restart backend only
docker-compose ps                    # List services

# Execute commands
docker-compose exec backend bash     # Access backend shell
docker-compose exec postgres psql    # Access database

# Build
docker-compose build                 # Build all services
docker-compose build --no-cache      # Clean build

# Clean up
docker-compose down --remove-orphans # Remove orphan containers
docker system prune -a               # Clean everything
```
