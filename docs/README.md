# CF Meet Documentation

Comprehensive guides for deploying and operating CF Meet.

## Architecture Overview

```
                            INTERNET
                                │
        ┌───────────────────────┼───────────────────────┐
        │                       │                       │
        ▼                       ▼                       ▼
 ┌─────────────┐        ┌─────────────┐        ┌─────────────┐
 │   Vercel    │        │   Expo /    │        │ Ubuntu VPS  │
 │   (Web)     │        │ App Stores  │        │  (Backend)  │
 │ cfmeet.com  │        │  (Mobile)   │        │ api.cfmeet. │
 └──────┬──────┘        └──────┬──────┘        │    com      │
        │                      │               └──────┬──────┘
        │                      │                      │
        └──────────────────────┴──────────────────────┘
                               │
               ┌───────────────┼───────────────┐
               │               │               │
               ▼               ▼               ▼
        ┌───────────┐   ┌───────────┐   ┌───────────┐
        │ Meeting   │   │  LiveKit  │   │ PostgreSQL│
        │ Service   │   │   (RTC)   │   │  + Redis  │
        │  (Rust)   │   │           │   │           │
        └───────────┘   └───────────┘   └───────────┘
```

## Deployment Platform Summary

| Component | Platform | Description |
|-----------|----------|-------------|
| **Web App** | Vercel | Next.js frontend, auto-deploys from `apps/web` |
| **Mobile App** | Expo / App Stores | React Native app built with EAS |
| **Backend API** | Ubuntu VPS (Docker) | Rust/Axum meeting service |
| **LiveKit** | Ubuntu VPS (Docker) | Self-hosted WebRTC media server |
| **PostgreSQL** | Ubuntu VPS (Docker) | Primary database |
| **Redis** | Ubuntu VPS (Docker) | Cache, sessions, rate limiting |

## Documentation Index

### Main Guide

| Guide | Description |
|-------|-------------|
| **[DEPLOYMENT.md](./DEPLOYMENT.md)** | **START HERE** - Complete step-by-step guide to deploy everything |

### Detailed Guides

| Guide | Description |
|-------|-------------|
| [INFRASTRUCTURE.md](./INFRASTRUCTURE.md) | Ubuntu VPS detailed setup (referenced by DEPLOYMENT.md) |
| [BACKEND.md](./BACKEND.md) | Backend deployment options and configuration |
| [VERCEL.md](./VERCEL.md) | Web app deployment to Vercel |
| [MOBILE.md](./MOBILE.md) | Mobile app deployment with Expo EAS |

### Configuration

| Guide | Description |
|-------|-------------|
| [ENVIRONMENT.md](./ENVIRONMENT.md) | Complete environment variables reference |
| [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) | Common issues and solutions |

### Service Documentation

| Guide | Description |
|-------|-------------|
| [Meeting Service Docs](../services/meeting-service/docs/README.md) | Backend API documentation |
| [LiveKit Integration](../services/meeting-service/docs/LIVEKIT.md) | LiveKit setup and token generation |

## Quick Start

### 1. Deploy Infrastructure (Ubuntu VPS)

Follow [INFRASTRUCTURE.md](./INFRASTRUCTURE.md) to set up:
- Ubuntu VPS with Docker
- PostgreSQL database
- Redis cache
- LiveKit media server
- Nginx reverse proxy with SSL

### 2. Deploy Web Frontend (Vercel)

```bash
# Connect to Vercel
# Root Directory: apps/web
# Framework: Next.js
# Environment: NEXT_PUBLIC_API_URL=https://api.your-domain.com/api/v1
```

See [VERCEL.md](./VERCEL.md) for detailed steps.

### 3. Deploy Mobile App (Expo)

```bash
cd apps/mobile

# Configure API URL in app.json or eas.json
# Build for production
eas build --platform all --profile production

# Submit to stores
eas submit --platform all
```

See [MOBILE.md](./MOBILE.md) for detailed steps.

## Production Checklist

### Security
- [ ] All secrets are strong and unique (use `openssl rand -base64 64`)
- [ ] SSL/TLS enabled on all public endpoints
- [ ] Firewall configured (UFW on Ubuntu)
- [ ] SSH key authentication (disable password auth)
- [ ] CORS configured with specific domains
- [ ] Rate limiting enabled

### Infrastructure
- [ ] PostgreSQL with automated backups
- [ ] Redis with persistence
- [ ] LiveKit with TURN for NAT traversal
- [ ] Health monitoring configured
- [ ] SSL certificate auto-renewal

### DNS Configuration
```
Type    Name        Value               Purpose
A       @           VPS_IP              Root domain (if needed)
A       api         VPS_IP              Backend API
A       livekit     VPS_IP              LiveKit server
CNAME   www         cfmeet.com          WWW redirect (Vercel handles)
```

## Environment Variables Quick Reference

### Backend (services/meeting-service/.env)
```bash
DATABASE_URL=postgres://user:pass@postgres:5432/cfmeet
REDIS_URL=redis://redis:6379
JWT_SECRET=your-64-char-secret
LIVEKIT_URL=wss://livekit.your-domain.com
LIVEKIT_API_KEY=your-api-key
LIVEKIT_API_SECRET=your-api-secret
APP_URL=https://your-domain.com
FRONTEND_URLS=https://your-domain.com
```

### Web App (apps/web/.env.local)
```bash
NEXT_PUBLIC_API_URL=https://api.your-domain.com/api/v1
```

### Mobile App (apps/mobile/.env or eas.json)
```bash
EXPO_PUBLIC_API_URL=https://api.your-domain.com/api/v1
```

## Support

- Check [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) for common issues
- Review service logs: `docker compose logs -f`
- Backend API docs: `https://api.your-domain.com/swagger-ui/`
