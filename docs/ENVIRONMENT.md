# Environment Variables Reference

This document provides a complete reference for all environment variables used across CF Meet components.

## Meeting Service (Backend)

### Application Settings

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `RUST_LOG` | No | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace` |
| `APP_URL` | Yes | - | Public URL of the application (e.g., `https://cfmeet.com`) |
| `FRONTEND_URLS` | Yes | - | Comma-separated list of allowed frontend origins for CORS |
| `HOST` | No | `0.0.0.0` | Server bind address |
| `PORT` | No | `8080` | Server port |

### Database

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | Yes | - | PostgreSQL connection string |

**Format:**
```
postgres://user:password@host:port/database
```

**Examples:**
```bash
# Local development
DATABASE_URL=postgres://postgres:postgres@localhost:5432/cfmeet

# Docker Compose
DATABASE_URL=postgres://cfmeet:password@db:5432/cfmeet

# Managed database (AWS RDS, etc.)
DATABASE_URL=postgres://admin:securepass@cfmeet-db.abc123.us-east-1.rds.amazonaws.com:5432/cfmeet?sslmode=require
```

### Redis

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `REDIS_URL` | Yes | - | Redis connection string |

**Format:**
```
redis://[:password@]host:port[/database]
```

**Examples:**
```bash
# Local development
REDIS_URL=redis://localhost:6379

# With password
REDIS_URL=redis://:mypassword@localhost:6379

# Docker Compose
REDIS_URL=redis://redis:6379

# Managed Redis (ElastiCache, etc.)
REDIS_URL=redis://:authtoken@cfmeet-redis.abc123.ng.0001.use1.cache.amazonaws.com:6379
```

### JWT Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `JWT_SECRET` | Yes | - | Secret key for signing JWTs (min 64 characters recommended) |
| `JWT_ACCESS_TOKEN_EXPIRE_MINUTES` | No | `15` | Access token expiration in minutes |
| `JWT_REFRESH_TOKEN_EXPIRE_DAYS` | No | `7` | Refresh token expiration in days |

**Generating a secure JWT secret:**
```bash
# Using openssl
openssl rand -base64 64

# Using node
node -e "console.log(require('crypto').randomBytes(64).toString('base64'))"
```

### LiveKit Integration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `LIVEKIT_URL` | Yes | - | LiveKit server WebSocket URL |
| `LIVEKIT_API_KEY` | Yes | - | LiveKit API key |
| `LIVEKIT_API_SECRET` | Yes | - | LiveKit API secret |

**Examples:**
```bash
# LiveKit Cloud
LIVEKIT_URL=wss://your-project.livekit.cloud
LIVEKIT_API_KEY=APIxxxxxxxx
LIVEKIT_API_SECRET=xxxxxxxxxxxxxxxxxxxxxxxx

# Self-hosted LiveKit
LIVEKIT_URL=wss://livekit.cfmeet.com
LIVEKIT_API_KEY=devkey
LIVEKIT_API_SECRET=devsecret
```

### Google OAuth

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `GOOGLE_CLIENT_ID` | No* | - | Google OAuth 2.0 client ID |
| `GOOGLE_CLIENT_SECRET` | No* | - | Google OAuth 2.0 client secret |
| `GOOGLE_REDIRECT_URL` | No* | - | OAuth callback URL |

*Required if Google OAuth is enabled.

**Setup:**
1. Go to [Google Cloud Console](https://console.cloud.google.com/apis/credentials)
2. Create OAuth 2.0 credentials
3. Add authorized redirect URIs

**Examples:**
```bash
GOOGLE_CLIENT_ID=123456789-abcdef.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=GOCSPX-xxxxxxxxxx
GOOGLE_REDIRECT_URL=https://api.cfmeet.com/api/v1/auth/oauth/google/callback
```

### Email (SMTP)

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `SMTP_HOST` | Yes | - | SMTP server hostname |
| `SMTP_PORT` | No | `587` | SMTP server port |
| `SMTP_USERNAME` | Yes | - | SMTP authentication username |
| `SMTP_PASSWORD` | Yes | - | SMTP authentication password |
| `SMTP_FROM_EMAIL` | Yes | - | Sender email address |
| `SMTP_FROM_NAME` | No | `CF Meet` | Sender display name |
| `SMTP_USE_TLS` | No | `true` | Enable TLS encryption |

**Examples:**
```bash
# Gmail (use App Password)
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=xxxx-xxxx-xxxx-xxxx
SMTP_FROM_EMAIL=noreply@cfmeet.com
SMTP_FROM_NAME=CF Meet

# SendGrid
SMTP_HOST=smtp.sendgrid.net
SMTP_PORT=587
SMTP_USERNAME=apikey
SMTP_PASSWORD=SG.xxxxxxxx
SMTP_FROM_EMAIL=noreply@cfmeet.com

# Mailgun
SMTP_HOST=smtp.mailgun.org
SMTP_PORT=587
SMTP_USERNAME=postmaster@mg.cfmeet.com
SMTP_PASSWORD=xxxxxxxx
SMTP_FROM_EMAIL=noreply@cfmeet.com
```

### Swagger / API Documentation

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `SWAGGER_PUBLIC_SERVER_URL` | No | - | Public server URL for Swagger UI |

**Example:**
```bash
SWAGGER_PUBLIC_SERVER_URL=https://api.cfmeet.com
```

### Feature Flags

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `FEATURE_MEETING_V2_ENABLED` | No | `false` | Enable enhanced meeting features |
| `FEATURE_WAITING_ROOM_V2_ENABLED` | No | `false` | Enable waiting room V2 |
| `FEATURE_RTC_CONTROL_ENABLED` | No | `false` | Enable RTC control features |

---

## Web App (Next.js Frontend)

### API Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `NEXT_PUBLIC_API_URL` | Yes | - | Backend API base URL |

**Note:** Variables prefixed with `NEXT_PUBLIC_` are exposed to the browser.

**Examples:**
```bash
# Development
NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1

# Staging
NEXT_PUBLIC_API_URL=https://staging-api.cfmeet.com/api/v1

# Production
NEXT_PUBLIC_API_URL=https://api.cfmeet.com/api/v1
```

### Build-time Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ANALYZE` | No | `false` | Enable bundle analyzer |

---

## Mobile App (Expo/React Native)

### API Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `EXPO_PUBLIC_API_URL` | Yes | - | Backend API base URL |

**Note:** Variables prefixed with `EXPO_PUBLIC_` are exposed to the app.

**Examples:**
```bash
# Development
EXPO_PUBLIC_API_URL=http://192.168.1.100:8080/api/v1

# Staging (eas.json preview profile)
EXPO_PUBLIC_API_URL=https://staging-api.cfmeet.com/api/v1

# Production (eas.json production profile)
EXPO_PUBLIC_API_URL=https://api.cfmeet.com/api/v1
```

### EAS Build Variables

These are configured in `eas.json`:

```json
{
  "build": {
    "production": {
      "env": {
        "EXPO_PUBLIC_API_URL": "https://api.cfmeet.com/api/v1"
      }
    }
  }
}
```

---

## Infrastructure

### PostgreSQL

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `POSTGRES_USER` | Yes | - | Database superuser name |
| `POSTGRES_PASSWORD` | Yes | - | Database superuser password |
| `POSTGRES_DB` | Yes | - | Default database name |

### Redis

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `REDIS_PASSWORD` | No | - | Redis authentication password |

### LiveKit (Self-hosted)

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `LIVEKIT_KEYS` | Yes | - | API key-secret pairs |
| `LIVEKIT_LOG_LEVEL` | No | `info` | Log level |

**Example `livekit.yaml`:**
```yaml
port: 7880
rtc:
  tcp_port: 7881
  port_range_start: 50000
  port_range_end: 60000
  use_external_ip: true
keys:
  devkey: devsecret
  prodkey: prodsecret
```

---

## Environment Files

### Development

Create `.env` files in each component directory:

```bash
# services/meeting-service/.env
RUST_LOG=info,meeting_service=debug
DATABASE_URL=postgres://postgres:postgres@localhost:5432/cfmeet
REDIS_URL=redis://localhost:6379
JWT_SECRET=<dev-jwt-secret>
LIVEKIT_URL=ws://localhost:7880
LIVEKIT_API_KEY=devkey
LIVEKIT_API_SECRET=devsecret
APP_URL=http://localhost:3000
FRONTEND_URLS=http://localhost:3000
SMTP_HOST=localhost
SMTP_PORT=1025
SMTP_USERNAME=test
SMTP_PASSWORD=test
SMTP_FROM_EMAIL=noreply@localhost
```

```bash
# apps/web/.env.local
NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1
```

```bash
# apps/mobile/.env
EXPO_PUBLIC_API_URL=http://192.168.1.100:8080/api/v1
```

### Production

Create `.env.prod` or use environment variable injection:

```bash
# services/meeting-service/.env.prod
RUST_LOG=info,meeting_service=info
DATABASE_URL=postgres://cfmeet:SECURE_PASSWORD@db.cfmeet.com:5432/cfmeet?sslmode=require
REDIS_URL=redis://:REDIS_PASSWORD@redis.cfmeet.com:6379
JWT_SECRET=<production-jwt-secret>
LIVEKIT_URL=wss://livekit.cfmeet.com
LIVEKIT_API_KEY=YOUR_LIVEKIT_API_KEY
LIVEKIT_API_SECRET=YOUR_LIVEKIT_API_SECRET
APP_URL=https://cfmeet.com
FRONTEND_URLS=https://cfmeet.com,https://www.cfmeet.com
GOOGLE_CLIENT_ID=YOUR_GOOGLE_CLIENT_ID
GOOGLE_CLIENT_SECRET=YOUR_GOOGLE_CLIENT_SECRET
GOOGLE_REDIRECT_URL=https://api.cfmeet.com/api/v1/auth/oauth/google/callback
SMTP_HOST=smtp.sendgrid.net
SMTP_PORT=587
SMTP_USERNAME=apikey
SMTP_PASSWORD=YOUR_SENDGRID_API_KEY
SMTP_FROM_EMAIL=noreply@cfmeet.com
SMTP_FROM_NAME=CF Meet
SWAGGER_PUBLIC_SERVER_URL=https://api.cfmeet.com
FEATURE_MEETING_V2_ENABLED=true
FEATURE_WAITING_ROOM_V2_ENABLED=true
FEATURE_RTC_CONTROL_ENABLED=true
```

---

## Security Best Practices

### Secrets Management

1. **Never commit secrets to git**
   ```gitignore
   # .gitignore
   .env
   .env.*
   !.env.example
   *.pem
   *.key
   google-play-key.json
   ```

2. **Use secrets management services in production:**
   - AWS Secrets Manager
   - HashiCorp Vault
   - Docker Secrets
   - Kubernetes Secrets

3. **Rotate secrets regularly:**
   - JWT secrets: Every 90 days
   - API keys: Every 6 months
   - Database passwords: Every 90 days

### Environment Isolation

Keep separate credentials for each environment:

| Environment | Database | Redis | LiveKit |
|-------------|----------|-------|---------|
| Development | Local | Local | Local |
| Staging | Staging DB | Staging Redis | Staging LiveKit |
| Production | Production DB | Production Redis | Production LiveKit |

### Validation

The Meeting Service validates required environment variables on startup. If a required variable is missing, the service will fail to start with a clear error message.

```bash
# Example error
Error: Required environment variable DATABASE_URL is not set
```

---

## Quick Reference Card

### Minimum Required Variables

**Meeting Service:**
```bash
DATABASE_URL=postgres://user:pass@host:5432/db
REDIS_URL=redis://host:6379
JWT_SECRET=your-64-char-secret
LIVEKIT_URL=wss://your-livekit-url
LIVEKIT_API_KEY=your-key
LIVEKIT_API_SECRET=your-secret
APP_URL=https://your-domain.com
FRONTEND_URLS=https://your-frontend.com
SMTP_HOST=smtp.provider.com
SMTP_USERNAME=user
SMTP_PASSWORD=pass
SMTP_FROM_EMAIL=noreply@your-domain.com
```

**Web App:**
```bash
NEXT_PUBLIC_API_URL=https://api.your-domain.com/api/v1
```

**Mobile App:**
```bash
EXPO_PUBLIC_API_URL=https://api.your-domain.com/api/v1
```
