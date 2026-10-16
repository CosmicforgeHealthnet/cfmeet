# Backend Deployment Guide (Meeting Service)

This guide covers deploying the CF Meet Meeting Service as a Docker container on a VPS or cloud provider.

## Overview

The Meeting Service is a Rust/Axum application that provides:
- Authentication (email/password, OAuth)
- Meeting management (create, join, end)
- Participant management
- Real-time features via LiveKit integration
- API keys and webhooks for developers

## Prerequisites

- Docker and Docker Compose installed
- Domain name configured (e.g., `api.cfmeet.com`)
- SSL certificate (Let's Encrypt recommended)
- PostgreSQL 15+ database
- Redis 7+ instance
- LiveKit server (self-hosted or LiveKit Cloud)

## Deployment Options

### Option 1: Single VPS with Docker Compose (Recommended for small-medium scale)

```yaml
# docker-compose.prod.yml
version: "3.8"

services:
  meeting-service:
    image: cfmeet-meeting-service:latest
    container_name: cfmeet-api
    restart: unless-stopped
    ports:
      - "8080:8080"
    environment:
      - DATABASE_URL=postgres://user:pass@db:5432/cfmeet
      - REDIS_URL=redis://redis:6379
      - RUST_LOG=info,meeting_service=debug
    env_file:
      - .env.prod
    depends_on:
      - db
      - redis
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
      timeout: 10s
      retries: 3

  db:
    image: postgres:15-alpine
    container_name: cfmeet-db
    restart: unless-stopped
    volumes:
      - postgres_data:/var/lib/postgresql/data
    environment:
      POSTGRES_USER: cfmeet
      POSTGRES_PASSWORD: ${DB_PASSWORD}
      POSTGRES_DB: cfmeet
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U cfmeet"]
      interval: 10s
      timeout: 5s
      retries: 5

  redis:
    image: redis:7-alpine
    container_name: cfmeet-redis
    restart: unless-stopped
    volumes:
      - redis_data:/data
    command: redis-server --appendonly yes
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 5s
      retries: 5

  nginx:
    image: nginx:alpine
    container_name: cfmeet-nginx
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf:ro
      - ./ssl:/etc/nginx/ssl:ro
      - ./certbot/www:/var/www/certbot:ro
    depends_on:
      - meeting-service

volumes:
  postgres_data:
  redis_data:
```

### Option 2: Cloud Managed Services (AWS/GCP/Azure)

For production at scale, use managed services:

| Component | AWS | GCP | Azure |
|-----------|-----|-----|-------|
| Compute | ECS Fargate / EKS | Cloud Run / GKE | Container Apps / AKS |
| Database | RDS PostgreSQL | Cloud SQL | Azure Database |
| Cache | ElastiCache Redis | Memorystore | Azure Cache |
| CDN | CloudFront | Cloud CDN | Azure CDN |

## Step-by-Step Deployment

### 1. Build the Docker Image

```bash
# Clone repository
git clone https://github.com/CosmicforgeHealthnet/cfmeet.git
cd cfmeet

# Build the image
docker build -t cfmeet-meeting-service:latest ./services/meeting-service

# Tag for registry (optional)
docker tag cfmeet-meeting-service:latest your-registry/cfmeet-meeting-service:latest
docker push your-registry/cfmeet-meeting-service:latest
```

### 2. Configure Environment Variables

Create `.env.prod` file:

```bash
# Application
RUST_LOG=info,meeting_service=debug
APP_URL=https://cfmeet.com
FRONTEND_URLS=https://cfmeet.com,https://www.cfmeet.com

# Database
DATABASE_URL=postgres://cfmeet:your-secure-password@db:5432/cfmeet

# Redis
REDIS_URL=redis://redis:6379

# JWT Configuration
JWT_SECRET=your-very-long-and-secure-jwt-secret-key-at-least-64-characters
JWT_ACCESS_TOKEN_EXPIRE_MINUTES=15
JWT_REFRESH_TOKEN_EXPIRE_DAYS=7

# LiveKit
LIVEKIT_URL=wss://livekit.cfmeet.com
LIVEKIT_API_KEY=your-livekit-api-key
LIVEKIT_API_SECRET=your-livekit-api-secret

# Google OAuth
GOOGLE_CLIENT_ID=your-google-client-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-google-client-secret
GOOGLE_REDIRECT_URL=https://api.cfmeet.com/api/v1/auth/oauth/google/callback

# Email (SMTP)
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=noreply@cfmeet.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=noreply@cfmeet.com
SMTP_FROM_NAME=CF Meet

# Swagger
SWAGGER_PUBLIC_SERVER_URL=https://api.cfmeet.com

# Feature Flags
FEATURE_MEETING_V2_ENABLED=true
FEATURE_WAITING_ROOM_V2_ENABLED=true
FEATURE_RTC_CONTROL_ENABLED=true
```

### 3. Configure Nginx Reverse Proxy

```nginx
# nginx.conf
events {
    worker_connections 1024;
}

http {
    upstream meeting_service {
        server meeting-service:8080;
    }

    # Rate limiting
    limit_req_zone $binary_remote_addr zone=api:10m rate=100r/s;

    server {
        listen 80;
        server_name api.cfmeet.com;

        location /.well-known/acme-challenge/ {
            root /var/www/certbot;
        }

        location / {
            return 301 https://$host$request_uri;
        }
    }

    server {
        listen 443 ssl http2;
        server_name api.cfmeet.com;

        ssl_certificate /etc/nginx/ssl/fullchain.pem;
        ssl_certificate_key /etc/nginx/ssl/privkey.pem;

        # SSL settings
        ssl_protocols TLSv1.2 TLSv1.3;
        ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256;
        ssl_prefer_server_ciphers off;

        # Security headers
        add_header X-Frame-Options "SAMEORIGIN" always;
        add_header X-Content-Type-Options "nosniff" always;
        add_header X-XSS-Protection "1; mode=block" always;

        location / {
            limit_req zone=api burst=50 nodelay;

            proxy_pass http://meeting_service;
            proxy_http_version 1.1;
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
            proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
            proxy_set_header X-Forwarded-Proto $scheme;

            # Timeouts
            proxy_connect_timeout 60s;
            proxy_send_timeout 60s;
            proxy_read_timeout 60s;
        }

        # Health check (no rate limiting)
        location /health {
            proxy_pass http://meeting_service;
            proxy_http_version 1.1;
        }
    }
}
```

### 4. Run Database Migrations

```bash
# Run migrations before starting the service
docker run --rm \
  --network cfmeet_default \
  -e DATABASE_URL=postgres://cfmeet:password@db:5432/cfmeet \
  cfmeet-meeting-service:latest \
  /app/migration up
```

### 5. Deploy and Start Services

```bash
# Start all services
docker-compose -f docker-compose.prod.yml up -d

# Check logs
docker-compose -f docker-compose.prod.yml logs -f meeting-service

# Verify health
curl https://api.cfmeet.com/health
```

### 6. SSL Certificate with Let's Encrypt

```bash
# Install certbot
apt install certbot

# Get certificate
certbot certonly --webroot -w /var/www/certbot \
  -d api.cfmeet.com \
  --email admin@cfmeet.com \
  --agree-tos

# Auto-renewal cron
echo "0 0 * * * certbot renew --quiet" | crontab -
```

## Kubernetes Deployment

For Kubernetes deployments, see the example manifests:

```yaml
# deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: meeting-service
  namespace: cfmeet
spec:
  replicas: 3
  selector:
    matchLabels:
      app: meeting-service
  template:
    metadata:
      labels:
        app: meeting-service
    spec:
      containers:
        - name: meeting-service
          image: cfmeet-meeting-service:latest
          ports:
            - containerPort: 8080
          envFrom:
            - secretRef:
                name: meeting-service-secrets
            - configMapRef:
                name: meeting-service-config
          resources:
            requests:
              memory: "256Mi"
              cpu: "250m"
            limits:
              memory: "512Mi"
              cpu: "500m"
          readinessProbe:
            httpGet:
              path: /health
              port: 8080
            initialDelaySeconds: 5
            periodSeconds: 10
          livenessProbe:
            httpGet:
              path: /health
              port: 8080
            initialDelaySeconds: 15
            periodSeconds: 20
---
apiVersion: v1
kind: Service
metadata:
  name: meeting-service
  namespace: cfmeet
spec:
  selector:
    app: meeting-service
  ports:
    - port: 80
      targetPort: 8080
  type: ClusterIP
```

## Scaling Considerations

### Horizontal Scaling

The Meeting Service is stateless and can be horizontally scaled:

```bash
# Docker Compose
docker-compose -f docker-compose.prod.yml up -d --scale meeting-service=3

# Kubernetes
kubectl scale deployment meeting-service --replicas=5
```

### Database Scaling

- Use read replicas for read-heavy workloads
- Consider connection pooling (PgBouncer)
- Implement database indexing as documented

### Caching

- Redis is used for:
  - Session/token management
  - Rate limiting
  - OTP/verification codes
  - Temporary data caching

## Monitoring and Logging

### Health Endpoints

```bash
# Basic health
curl https://api.cfmeet.com/health

# Detailed info
curl https://api.cfmeet.com/
```

### Logging

```bash
# View logs
docker-compose logs -f meeting-service

# Log levels (set via RUST_LOG)
# error, warn, info, debug, trace
RUST_LOG=info,meeting_service=debug
```

### Metrics

The service exposes Prometheus metrics at `/internal/metrics` (internal access only).

## Backup and Recovery

### Database Backup

```bash
# Manual backup
docker exec cfmeet-db pg_dump -U cfmeet cfmeet > backup.sql

# Automated backup script
#!/bin/bash
DATE=$(date +%Y%m%d_%H%M%S)
docker exec cfmeet-db pg_dump -U cfmeet cfmeet | gzip > /backups/cfmeet_$DATE.sql.gz
find /backups -name "cfmeet_*.sql.gz" -mtime +7 -delete
```

### Recovery

```bash
# Restore from backup
gunzip < backup.sql.gz | docker exec -i cfmeet-db psql -U cfmeet cfmeet
```

## Troubleshooting

### Common Issues

1. **Connection refused to database**
   - Check DATABASE_URL is correct
   - Verify database container is running
   - Check network connectivity

2. **JWT token errors**
   - Ensure JWT_SECRET is the same across restarts
   - Check token expiration settings

3. **LiveKit connection issues**
   - Verify LIVEKIT_URL is accessible
   - Check API key/secret are correct
   - Ensure WebSocket connections are allowed

4. **CORS errors**
   - Update FRONTEND_URLS to include all frontend domains
   - Check Nginx headers configuration

### Debug Commands

```bash
# Check container status
docker-compose ps

# View container logs
docker-compose logs -f meeting-service

# Execute shell in container
docker exec -it cfmeet-api /bin/sh

# Test database connection
docker exec -it cfmeet-db psql -U cfmeet -c "SELECT 1"

# Test Redis connection
docker exec -it cfmeet-redis redis-cli ping
```
