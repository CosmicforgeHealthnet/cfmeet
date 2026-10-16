# Infrastructure Setup Guide

This guide covers deploying CF Meet infrastructure on an Ubuntu VPS for self-hosting the backend (meeting-service), LiveKit media server, PostgreSQL, and Redis.

## Architecture Overview

```
                                    INTERNET
                                        │
                    ┌───────────────────┼───────────────────┐
                    │                   │                   │
                    ▼                   ▼                   ▼
             ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
             │   Vercel    │    │   Expo/     │    │  Ubuntu VPS │
             │  (Web App)  │    │  App Store  │    │             │
             │cfmeet.com   │    │  (Mobile)   │    │api.cfmeet.  │
             └──────┬──────┘    └──────┬──────┘    │    com      │
                    │                  │           └──────┬──────┘
                    │                  │                  │
                    └──────────────────┴──────────────────┘
                                       │
                           ┌───────────┴───────────┐
                           │     Ubuntu VPS        │
                           │  ┌─────────────────┐  │
                           │  │     Nginx       │  │
                           │  │  (Reverse Proxy)│  │
                           │  └────────┬────────┘  │
                           │           │           │
                           │  ┌────────┼────────┐  │
                           │  │        │        │  │
                           │  ▼        ▼        ▼  │
                           │ ┌───┐  ┌─────┐  ┌───┐ │
                           │ │API│  │Live │  │DB │ │
                           │ │   │  │Kit  │  │   │ │
                           │ └───┘  └─────┘  └───┘ │
                           └───────────────────────┘
```

## Deployment Strategy

| Component | Platform | Domain |
|-----------|----------|--------|
| Web App | Vercel | `cfmeet.com` |
| Mobile App | Expo / App Stores | N/A |
| Backend API | Ubuntu VPS (Docker) | `api.cfmeet.com` |
| LiveKit | Ubuntu VPS (Docker) | `livekit.cfmeet.com` |
| PostgreSQL | Ubuntu VPS (Docker) | Internal |
| Redis | Ubuntu VPS (Docker) | Internal |

## VPS Requirements

### Minimum Specifications

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| CPU | 2 vCPU | 4 vCPU |
| RAM | 4 GB | 8 GB |
| Storage | 40 GB SSD | 80 GB SSD |
| Bandwidth | 2 TB/month | Unlimited |
| OS | Ubuntu 22.04 LTS | Ubuntu 24.04 LTS |

### Recommended VPS Providers

- DigitalOcean (Droplets)
- Linode (Linodes)
- Vultr (Cloud Compute)
- Hetzner (Cloud Servers)
- AWS Lightsail

---

## Step 1: Initial VPS Setup

### 1.1 Connect to Your VPS

```bash
ssh root@YOUR_VPS_IP
```

### 1.2 Create a Non-Root User

```bash
# Create user
adduser cfmeet
usermod -aG sudo cfmeet

# Setup SSH for new user
mkdir -p /home/cfmeet/.ssh
cp ~/.ssh/authorized_keys /home/cfmeet/.ssh/
chown -R cfmeet:cfmeet /home/cfmeet/.ssh
chmod 700 /home/cfmeet/.ssh
chmod 600 /home/cfmeet/.ssh/authorized_keys

# Switch to new user
su - cfmeet
```

### 1.3 Update System

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y curl wget git vim ufw fail2ban
```

### 1.4 Configure Firewall

```bash
# Allow SSH
sudo ufw allow OpenSSH

# Allow HTTP/HTTPS
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp

# Allow LiveKit ports
sudo ufw allow 7880/tcp    # LiveKit HTTP API
sudo ufw allow 7881/tcp    # LiveKit RTC over TCP
sudo ufw allow 7882/tcp    # LiveKit TURN/TLS
sudo ufw allow 3478/udp    # TURN UDP
sudo ufw allow 5349/tcp    # TURN TLS
sudo ufw allow 50000:60000/udp  # WebRTC UDP range

# Enable firewall
sudo ufw enable
sudo ufw status
```

### 1.5 Install Docker

```bash
# Install Docker
curl -fsSL https://get.docker.com | sudo sh

# Add user to docker group
sudo usermod -aG docker cfmeet

# Install Docker Compose plugin
sudo apt install -y docker-compose-plugin

# Verify installation
docker --version
docker compose version

# Logout and login again for group changes to take effect
exit
ssh cfmeet@YOUR_VPS_IP
```

---

## Step 2: DNS Configuration

Configure DNS records at your domain registrar:

```
Type    Name        Value               TTL
A       @           YOUR_VPS_IP         300
A       api         YOUR_VPS_IP         300
A       livekit     YOUR_VPS_IP         300
CNAME   www         cfmeet.com          300
```

Wait for DNS propagation (5-30 minutes).

```bash
# Verify DNS
dig api.cfmeet.com +short
dig livekit.cfmeet.com +short
```

---

## Step 3: Project Setup

### 3.1 Clone Repository

```bash
cd ~
git clone https://github.com/CosmicforgeHealthnet/cfmeet.git
cd cfmeet
```

### 3.2 Create Production Environment File

```bash
# Create production directory
mkdir -p ~/cfmeet-production
cd ~/cfmeet-production

# Create environment file
cat > .env << 'EOF'
# =============================================================================
# CF Meet Production Environment
# =============================================================================

# Application
APP_ENV=production
RUST_LOG=info,backend=info
HOST=0.0.0.0
PORT=8080

# URLs
APP_URL=https://cfmeet.com
FRONTEND_URLS=https://cfmeet.com,https://www.cfmeet.com
SWAGGER_SERVER_URL=https://api.cfmeet.com

# Database
POSTGRES_USER=cfmeet
POSTGRES_PASSWORD=CHANGE_THIS_SECURE_PASSWORD
POSTGRES_DB=cfmeet
DATABASE_URL=postgres://cfmeet:CHANGE_THIS_SECURE_PASSWORD@postgres:5432/cfmeet

# Redis
REDIS_URL=redis://redis:6379

# JWT (generate with: openssl rand -base64 64)
JWT_SECRET=<generated-jwt-secret>

# LiveKit
LIVEKIT_URL=wss://livekit.cfmeet.com
LIVEKIT_API_KEY=cfmeet_api_key
LIVEKIT_API_SECRET=CHANGE_THIS_GENERATE_SECURE_SECRET

# Google OAuth (optional)
GOOGLE_CLIENT_ID=your-google-client-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-google-client-secret
GOOGLE_REDIRECT_URL=https://api.cfmeet.com/api/v1/auth/oauth/google/callback

# Email (SMTP)
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=noreply@cfmeet.com
SMTP_FROM_NAME=CF Meet

# Email Verification
EMAIL_VERIFICATION_REQUIRED=true

# Feature Flags
FEATURE_MEETING_V2_ENABLED=true
FEATURE_WAITING_ROOM_V2_ENABLED=true
EOF
```

### 3.3 Generate Secure Secrets

```bash
# Generate JWT secret
echo "JWT_SECRET=$(openssl rand -base64 64 | tr -d '\n')"

# Generate LiveKit API secret
echo "LIVEKIT_API_SECRET=$(openssl rand -base64 32 | tr -d '\n')"

# Generate database password
echo "POSTGRES_PASSWORD=$(openssl rand -base64 24 | tr -d '\n')"
```

Update the `.env` file with the generated secrets.

---

## Step 4: Create Production Docker Compose

```bash
cat > docker-compose.yml << 'EOF'
# =============================================================================
# CF Meet Production Stack
# =============================================================================
name: cfmeet-production

services:
  # ---------------------------------------------------------------------------
  # PostgreSQL Database
  # ---------------------------------------------------------------------------
  postgres:
    image: postgres:16-alpine
    container_name: cfmeet-postgres
    restart: unless-stopped
    environment:
      POSTGRES_USER: ${POSTGRES_USER}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
      POSTGRES_DB: ${POSTGRES_DB}
    volumes:
      - postgres_data:/var/lib/postgresql/data
    networks:
      - cfmeet-internal
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER}"]
      interval: 10s
      timeout: 5s
      retries: 5

  # ---------------------------------------------------------------------------
  # Redis Cache
  # ---------------------------------------------------------------------------
  redis:
    image: redis:7-alpine
    container_name: cfmeet-redis
    restart: unless-stopped
    command: redis-server --appendonly yes --maxmemory 256mb --maxmemory-policy allkeys-lru
    volumes:
      - redis_data:/data
    networks:
      - cfmeet-internal
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 5s
      retries: 5

  # ---------------------------------------------------------------------------
  # LiveKit Media Server
  # ---------------------------------------------------------------------------
  livekit:
    image: livekit/livekit-server:latest
    container_name: cfmeet-livekit
    restart: unless-stopped
    command: --config /etc/livekit.yaml
    ports:
      - "7880:7880"       # HTTP API (will be behind nginx)
      - "7881:7881"       # RTC over TCP
      - "7882:7882"       # TURN/TLS
      - "3478:3478/udp"   # TURN UDP
      - "5349:5349/tcp"   # TURN TLS
      - "50000-60000:50000-60000/udp"  # WebRTC UDP
    volumes:
      - ./livekit.yaml:/etc/livekit.yaml:ro
      - ./ssl:/etc/livekit/ssl:ro
    networks:
      - cfmeet-internal
      - cfmeet-external

  # ---------------------------------------------------------------------------
  # Meeting Service (Backend API)
  # ---------------------------------------------------------------------------
  meeting-service:
    build:
      context: ./services/meeting-service
      dockerfile: Dockerfile
    image: cfmeet-meeting-service:latest
    container_name: cfmeet-api
    restart: unless-stopped
    environment:
      - DATABASE_URL=${DATABASE_URL}
      - REDIS_URL=${REDIS_URL}
      - JWT_SECRET=${JWT_SECRET}
      - LIVEKIT_URL=${LIVEKIT_URL}
      - LIVEKIT_API_KEY=${LIVEKIT_API_KEY}
      - LIVEKIT_API_SECRET=${LIVEKIT_API_SECRET}
      - APP_URL=${APP_URL}
      - FRONTEND_URLS=${FRONTEND_URLS}
      - SWAGGER_SERVER_URL=${SWAGGER_SERVER_URL}
      - GOOGLE_CLIENT_ID=${GOOGLE_CLIENT_ID}
      - GOOGLE_CLIENT_SECRET=${GOOGLE_CLIENT_SECRET}
      - GOOGLE_REDIRECT_URL=${GOOGLE_REDIRECT_URL}
      - SMTP_HOST=${SMTP_HOST}
      - SMTP_PORT=${SMTP_PORT}
      - SMTP_USERNAME=${SMTP_USERNAME}
      - SMTP_PASSWORD=${SMTP_PASSWORD}
      - SMTP_FROM_EMAIL=${SMTP_FROM_EMAIL}
      - SMTP_FROM_NAME=${SMTP_FROM_NAME}
      - EMAIL_VERIFICATION_REQUIRED=${EMAIL_VERIFICATION_REQUIRED}
      - RUST_LOG=${RUST_LOG}
      - HOST=${HOST}
      - PORT=${PORT}
      - FEATURE_MEETING_V2_ENABLED=${FEATURE_MEETING_V2_ENABLED}
      - FEATURE_WAITING_ROOM_V2_ENABLED=${FEATURE_WAITING_ROOM_V2_ENABLED}
    command: ["sh", "-c", "./migration up && ./server"]
    networks:
      - cfmeet-internal
      - cfmeet-external
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_healthy
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 40s

  # ---------------------------------------------------------------------------
  # Nginx Reverse Proxy
  # ---------------------------------------------------------------------------
  nginx:
    image: nginx:alpine
    container_name: cfmeet-nginx
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./nginx/nginx.conf:/etc/nginx/nginx.conf:ro
      - ./nginx/conf.d:/etc/nginx/conf.d:ro
      - ./ssl:/etc/nginx/ssl:ro
      - ./certbot/www:/var/www/certbot:ro
    networks:
      - cfmeet-external
    depends_on:
      - meeting-service
      - livekit

  # ---------------------------------------------------------------------------
  # Certbot for SSL
  # ---------------------------------------------------------------------------
  certbot:
    image: certbot/certbot
    container_name: cfmeet-certbot
    volumes:
      - ./ssl:/etc/letsencrypt
      - ./certbot/www:/var/www/certbot

networks:
  cfmeet-internal:
    driver: bridge
  cfmeet-external:
    driver: bridge

volumes:
  postgres_data:
  redis_data:
EOF
```

---

## Step 5: Create LiveKit Configuration

```bash
cat > livekit.yaml << 'EOF'
# =============================================================================
# LiveKit Production Configuration
# =============================================================================
port: 7880

rtc:
  tcp_port: 7881
  port_range_start: 50000
  port_range_end: 60000
  use_external_ip: true
  # IMPORTANT: Replace with your VPS public IP
  node_ip: YOUR_VPS_PUBLIC_IP

# API Keys (must match .env LIVEKIT_API_KEY and LIVEKIT_API_SECRET)
keys:
  cfmeet_api_key: CHANGE_THIS_GENERATE_SECURE_SECRET

logging:
  level: info
  json: true

room:
  auto_create: true
  empty_timeout: 300
  max_participants: 100

# TURN configuration for NAT traversal
turn:
  enabled: true
  domain: livekit.cfmeet.com
  tls_port: 5349
  udp_port: 3478
  # SSL certificates (will be created by certbot)
  cert_file: /etc/livekit/ssl/live/livekit.cfmeet.com/fullchain.pem
  key_file: /etc/livekit/ssl/live/livekit.cfmeet.com/privkey.pem

# Webhook configuration (optional)
# webhook:
#   urls:
#     - https://api.cfmeet.com/internal/rtc/livekit/webhook
#   api_key: cfmeet_api_key
EOF
```

**IMPORTANT:** Replace `YOUR_VPS_PUBLIC_IP` with your actual VPS IP address.

---

## Step 6: Create Nginx Configuration

### 6.1 Create Nginx Directories

```bash
mkdir -p nginx/conf.d ssl certbot/www
```

### 6.2 Create Main Nginx Config

```bash
cat > nginx/nginx.conf << 'EOF'
user nginx;
worker_processes auto;
error_log /var/log/nginx/error.log warn;
pid /var/run/nginx.pid;

events {
    worker_connections 1024;
    use epoll;
    multi_accept on;
}

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;

    log_format main '$remote_addr - $remote_user [$time_local] "$request" '
                    '$status $body_bytes_sent "$http_referer" '
                    '"$http_user_agent" "$http_x_forwarded_for"';

    access_log /var/log/nginx/access.log main;

    sendfile on;
    tcp_nopush on;
    tcp_nodelay on;
    keepalive_timeout 65;
    types_hash_max_size 2048;

    # Gzip compression
    gzip on;
    gzip_vary on;
    gzip_proxied any;
    gzip_comp_level 6;
    gzip_types text/plain text/css text/xml application/json application/javascript
               application/xml application/xml+rss text/javascript;

    # Rate limiting
    limit_req_zone $binary_remote_addr zone=api:10m rate=100r/s;
    limit_conn_zone $binary_remote_addr zone=addr:10m;

    # Include server configs
    include /etc/nginx/conf.d/*.conf;
}
EOF
```

### 6.3 Create API Server Config

```bash
cat > nginx/conf.d/api.conf << 'EOF'
# API Server (api.cfmeet.com)
upstream meeting_service {
    server meeting-service:8080;
    keepalive 32;
}

server {
    listen 80;
    server_name api.cfmeet.com;

    # ACME challenge for Let's Encrypt
    location /.well-known/acme-challenge/ {
        root /var/www/certbot;
    }

    # Redirect HTTP to HTTPS
    location / {
        return 301 https://$host$request_uri;
    }
}

server {
    listen 443 ssl http2;
    server_name api.cfmeet.com;

    # SSL certificates (will be created by certbot)
    ssl_certificate /etc/nginx/ssl/live/api.cfmeet.com/fullchain.pem;
    ssl_certificate_key /etc/nginx/ssl/live/api.cfmeet.com/privkey.pem;

    # SSL settings
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384;
    ssl_prefer_server_ciphers off;
    ssl_session_cache shared:SSL:10m;
    ssl_session_timeout 1d;
    ssl_session_tickets off;

    # Security headers
    add_header X-Frame-Options "SAMEORIGIN" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header X-XSS-Protection "1; mode=block" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;

    # CORS headers (handled by backend, but can add here too)

    # API proxy
    location / {
        limit_req zone=api burst=50 nodelay;
        limit_conn addr 100;

        proxy_pass http://meeting_service;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Connection "";

        # Timeouts
        proxy_connect_timeout 60s;
        proxy_send_timeout 60s;
        proxy_read_timeout 60s;

        # Buffer settings
        proxy_buffering on;
        proxy_buffer_size 4k;
        proxy_buffers 8 4k;
    }

    # WebSocket support (for future real-time features)
    location /ws {
        proxy_pass http://meeting_service;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 86400;
    }

    # Health check (no rate limiting)
    location /health {
        proxy_pass http://meeting_service;
        proxy_http_version 1.1;
    }
}
EOF
```

### 6.4 Create LiveKit Server Config

```bash
cat > nginx/conf.d/livekit.conf << 'EOF'
# LiveKit Server (livekit.cfmeet.com)
upstream livekit_server {
    server livekit:7880;
    keepalive 32;
}

server {
    listen 80;
    server_name livekit.cfmeet.com;

    location /.well-known/acme-challenge/ {
        root /var/www/certbot;
    }

    location / {
        return 301 https://$host$request_uri;
    }
}

server {
    listen 443 ssl http2;
    server_name livekit.cfmeet.com;

    ssl_certificate /etc/nginx/ssl/live/livekit.cfmeet.com/fullchain.pem;
    ssl_certificate_key /etc/nginx/ssl/live/livekit.cfmeet.com/privkey.pem;

    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384;
    ssl_prefer_server_ciphers off;

    # WebSocket proxy for LiveKit
    location / {
        proxy_pass http://livekit_server;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 86400;
        proxy_send_timeout 86400;
    }
}
EOF
```

---

## Step 7: SSL Certificates with Let's Encrypt

### 7.1 Create Temporary Self-Signed Certificates (for initial startup)

```bash
# Create temporary certificates for nginx to start
mkdir -p ssl/live/api.cfmeet.com ssl/live/livekit.cfmeet.com

# Generate self-signed certs (temporary)
openssl req -x509 -nodes -days 1 -newkey rsa:2048 \
  -keyout ssl/live/api.cfmeet.com/privkey.pem \
  -out ssl/live/api.cfmeet.com/fullchain.pem \
  -subj "/CN=api.cfmeet.com"

openssl req -x509 -nodes -days 1 -newkey rsa:2048 \
  -keyout ssl/live/livekit.cfmeet.com/privkey.pem \
  -out ssl/live/livekit.cfmeet.com/fullchain.pem \
  -subj "/CN=livekit.cfmeet.com"
```

### 7.2 Start Nginx for ACME Challenge

```bash
# Start only nginx first (for certbot validation)
docker compose up -d nginx
```

### 7.3 Obtain Real Certificates

```bash
# Get certificate for API domain
docker compose run --rm certbot certonly \
  --webroot \
  --webroot-path=/var/www/certbot \
  --email admin@cfmeet.com \
  --agree-tos \
  --no-eff-email \
  -d api.cfmeet.com

# Get certificate for LiveKit domain
docker compose run --rm certbot certonly \
  --webroot \
  --webroot-path=/var/www/certbot \
  --email admin@cfmeet.com \
  --agree-tos \
  --no-eff-email \
  -d livekit.cfmeet.com
```

### 7.4 Setup Auto-Renewal

```bash
# Create renewal script
cat > /home/cfmeet/cfmeet-production/renew-certs.sh << 'EOF'
#!/bin/bash
cd /home/cfmeet/cfmeet-production
docker compose run --rm certbot renew
docker compose exec nginx nginx -s reload
EOF

chmod +x /home/cfmeet/cfmeet-production/renew-certs.sh

# Add to crontab (runs daily at 3am)
(crontab -l 2>/dev/null; echo "0 3 * * * /home/cfmeet/cfmeet-production/renew-certs.sh >> /var/log/certbot-renewal.log 2>&1") | crontab -
```

---

## Step 8: Copy Backend Source and Build

```bash
# Copy meeting-service source to production directory
cp -r ~/cfmeet/services/meeting-service ~/cfmeet-production/services/

# Update livekit.yaml with your VPS IP
nano ~/cfmeet-production/livekit.yaml
# Replace YOUR_VPS_PUBLIC_IP with actual IP
```

---

## Step 9: Start All Services

```bash
cd ~/cfmeet-production

# Build and start all services
docker compose up -d --build

# Check status
docker compose ps

# View logs
docker compose logs -f
```

---

## Step 10: Verify Deployment

### 10.1 Health Checks

```bash
# API health
curl https://api.cfmeet.com/health

# API info
curl https://api.cfmeet.com/

# LiveKit health
curl https://livekit.cfmeet.com/
```

### 10.2 Test LiveKit Connection

```bash
# Install LiveKit CLI
curl -sSL https://get.livekit.io | bash

# Test room creation
livekit-cli room list \
  --url wss://livekit.cfmeet.com \
  --api-key cfmeet_api_key \
  --api-secret YOUR_LIVEKIT_API_SECRET
```

---

## Maintenance Commands

### View Logs

```bash
# All services
docker compose logs -f

# Specific service
docker compose logs -f meeting-service
docker compose logs -f livekit
docker compose logs -f postgres
```

### Restart Services

```bash
# Restart all
docker compose restart

# Restart specific service
docker compose restart meeting-service
```

### Update Application

```bash
cd ~/cfmeet-production

# Pull latest code
cd ~/cfmeet
git pull origin main

# Copy updated source
cp -r ~/cfmeet/services/meeting-service ~/cfmeet-production/services/

# Rebuild and restart
cd ~/cfmeet-production
docker compose up -d --build meeting-service
```

### Database Backup

```bash
# Create backup
docker exec cfmeet-postgres pg_dump -U cfmeet cfmeet | gzip > backup_$(date +%Y%m%d_%H%M%S).sql.gz

# Restore backup
gunzip < backup_YYYYMMDD_HHMMSS.sql.gz | docker exec -i cfmeet-postgres psql -U cfmeet cfmeet
```

### Database Migration

```bash
docker compose exec meeting-service ./migration up
docker compose exec meeting-service ./migration status
```

---

## Monitoring

### Basic Monitoring Script

```bash
cat > /home/cfmeet/cfmeet-production/health-check.sh << 'EOF'
#!/bin/bash
API_URL="https://api.cfmeet.com/health"
LIVEKIT_URL="https://livekit.cfmeet.com/"

if ! curl -sf "$API_URL" > /dev/null; then
    echo "$(date): API is DOWN!" | tee -a /var/log/cfmeet-health.log
    docker compose restart meeting-service
fi

if ! curl -sf "$LIVEKIT_URL" > /dev/null; then
    echo "$(date): LiveKit is DOWN!" | tee -a /var/log/cfmeet-health.log
    docker compose restart livekit
fi
EOF

chmod +x /home/cfmeet/cfmeet-production/health-check.sh

# Add to crontab (runs every 5 minutes)
(crontab -l 2>/dev/null; echo "*/5 * * * * /home/cfmeet/cfmeet-production/health-check.sh") | crontab -
```

---

## Security Checklist

- [ ] Firewall configured with UFW
- [ ] SSH key authentication enabled (disable password auth)
- [ ] Fail2ban installed and configured
- [ ] All secrets are strong and unique
- [ ] SSL certificates installed and auto-renewal configured
- [ ] Database not exposed to public internet
- [ ] Redis not exposed to public internet
- [ ] Regular backups scheduled
- [ ] Monitoring/health checks configured
- [ ] Log rotation configured

---

## Troubleshooting

See [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) for common issues and solutions.
