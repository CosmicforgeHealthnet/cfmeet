# CF Meet Complete Deployment Guide

Step-by-step guide to deploy the entire CF Meet platform.

## Overview

| Component | Platform | Domain | Order |
|-----------|----------|--------|-------|
| Backend + LiveKit + DB | Ubuntu VPS | `api.cfmeet.com`, `livekit.cfmeet.com` | 1st |
| Web App | Vercel | `cfmeet.com` | 2nd |
| Mobile App | Expo/EAS | App Stores | 3rd |

**Deploy in this order** - the backend must be running before frontend/mobile can connect.

---

## Prerequisites

Before starting, you need:

- [ ] Ubuntu VPS (minimum 2 vCPU, 4GB RAM, 40GB SSD)
- [ ] Domain name with DNS access
- [ ] Vercel account (free tier works)
- [ ] Expo account (free tier works)
- [ ] Apple Developer account ($99/year) - for iOS
- [ ] Google Play Developer account ($25 one-time) - for Android
- [ ] SMTP email credentials (Gmail, SendGrid, etc.)
- [ ] Google OAuth credentials (optional)

---

# Part 1: Backend Deployment (Ubuntu VPS)

## Step 1.1: VPS Initial Setup

```bash
# SSH into your VPS as root
ssh root@YOUR_VPS_IP

# Create non-root user
adduser cfmeet
usermod -aG sudo cfmeet

# Setup SSH keys for new user
mkdir -p /home/cfmeet/.ssh
cp ~/.ssh/authorized_keys /home/cfmeet/.ssh/
chown -R cfmeet:cfmeet /home/cfmeet/.ssh
chmod 700 /home/cfmeet/.ssh
chmod 600 /home/cfmeet/.ssh/authorized_keys

# Switch to new user
su - cfmeet
```

## Step 1.2: Install Dependencies

```bash
# Update system
sudo apt update && sudo apt upgrade -y

# Install required packages
sudo apt install -y curl wget git vim ufw fail2ban

# Install Docker
curl -fsSL https://get.docker.com | sudo sh
sudo usermod -aG docker cfmeet
sudo apt install -y docker-compose-plugin

# IMPORTANT: Logout and login again
exit
ssh cfmeet@YOUR_VPS_IP

# Verify Docker
docker --version
docker compose version
```

## Step 1.3: Configure Firewall

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw allow 7880/tcp
sudo ufw allow 7881/tcp
sudo ufw allow 7882/tcp
sudo ufw allow 3478/udp
sudo ufw allow 5349/tcp
sudo ufw allow 50000:60000/udp
sudo ufw enable
```

## Step 1.4: Configure DNS

At your domain registrar, add these DNS records:

| Type | Name | Value |
|------|------|-------|
| A | api | YOUR_VPS_IP |
| A | livekit | YOUR_VPS_IP |

Wait 5-30 minutes for propagation, then verify:

```bash
dig api.cfmeet.com +short
dig livekit.cfmeet.com +short
```

## Step 1.5: Clone Repository

```bash
cd ~
git clone https://github.com/YOUR_REPO/cfmeet.git
cd cfmeet
```

## Step 1.6: Create Production Directory

```bash
mkdir -p ~/cfmeet-production
cd ~/cfmeet-production

# Copy required files
cp ~/cfmeet/docker-compose.prod.yml ./docker-compose.yml
cp -r ~/cfmeet/services/meeting-service ./services/
```

## Step 1.7: Generate Secrets

```bash
# Generate and save these - you'll need them for .env
echo "JWT_SECRET: $(openssl rand -base64 64 | tr -d '\n')"
echo "LIVEKIT_API_SECRET: $(openssl rand -base64 32 | tr -d '\n')"
echo "POSTGRES_PASSWORD: $(openssl rand -base64 24 | tr -d '\n')"
```

## Step 1.8: Create Environment File

```bash
nano ~/cfmeet-production/.env
```

Paste and edit (replace ALL placeholder values):

```bash
# =============================================================================
# CF Meet Production Environment
# =============================================================================

# Application
APP_ENV=production
RUST_LOG=info,backend=info
HOST=0.0.0.0
PORT=8080

# URLs (CHANGE THESE to your domain)
APP_URL=https://cfmeet.com
FRONTEND_URLS=https://cfmeet.com,https://www.cfmeet.com
SWAGGER_SERVER_URL=https://api.cfmeet.com

# Database (CHANGE password)
POSTGRES_USER=cfmeet
POSTGRES_PASSWORD=YOUR_GENERATED_PASSWORD
POSTGRES_DB=cfmeet
DATABASE_URL=postgres://cfmeet:YOUR_GENERATED_PASSWORD@postgres:5432/cfmeet

# Redis
REDIS_URL=redis://redis:6379

# JWT (CHANGE to your generated secret)
JWT_SECRET=YOUR_GENERATED_JWT_SECRET

# LiveKit (CHANGE secret)
LIVEKIT_URL=wss://livekit.cfmeet.com
LIVEKIT_API_KEY=cfmeet_api_key
LIVEKIT_API_SECRET=YOUR_GENERATED_LIVEKIT_SECRET

# Google OAuth (optional - remove if not using)
GOOGLE_CLIENT_ID=your-google-client-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-google-client-secret
GOOGLE_REDIRECT_URL=https://api.cfmeet.com/api/v1/auth/oauth/google/callback

# Email SMTP (CHANGE these)
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
```

## Step 1.9: Create LiveKit Configuration

```bash
nano ~/cfmeet-production/livekit.yaml
```

Paste (replace YOUR_VPS_IP and YOUR_LIVEKIT_SECRET):

```yaml
port: 7880

rtc:
  tcp_port: 7881
  port_range_start: 50000
  port_range_end: 60000
  use_external_ip: true
  node_ip: YOUR_VPS_IP

keys:
  cfmeet_api_key: YOUR_LIVEKIT_SECRET

logging:
  level: info
  json: true

room:
  auto_create: true
  empty_timeout: 300
  max_participants: 100

turn:
  enabled: true
  domain: livekit.cfmeet.com
  tls_port: 5349
  udp_port: 3478
  cert_file: /etc/livekit/ssl/live/livekit.cfmeet.com/fullchain.pem
  key_file: /etc/livekit/ssl/live/livekit.cfmeet.com/privkey.pem
```

## Step 1.10: Create Nginx Configuration

```bash
mkdir -p ~/cfmeet-production/nginx/conf.d
mkdir -p ~/cfmeet-production/ssl
mkdir -p ~/cfmeet-production/certbot/www
```

**Main nginx.conf:**

```bash
cat > ~/cfmeet-production/nginx/nginx.conf << 'EOF'
user nginx;
worker_processes auto;
error_log /var/log/nginx/error.log warn;
pid /var/run/nginx.pid;

events {
    worker_connections 1024;
}

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    sendfile on;
    keepalive_timeout 65;
    gzip on;

    limit_req_zone $binary_remote_addr zone=api:10m rate=100r/s;

    include /etc/nginx/conf.d/*.conf;
}
EOF
```

**API config:**

```bash
cat > ~/cfmeet-production/nginx/conf.d/api.conf << 'EOF'
upstream meeting_service {
    server meeting-service:8080;
}

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

    ssl_certificate /etc/nginx/ssl/live/api.cfmeet.com/fullchain.pem;
    ssl_certificate_key /etc/nginx/ssl/live/api.cfmeet.com/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;

    location / {
        limit_req zone=api burst=50 nodelay;
        proxy_pass http://meeting_service;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    location /health {
        proxy_pass http://meeting_service;
    }
}
EOF
```

**LiveKit config:**

```bash
cat > ~/cfmeet-production/nginx/conf.d/livekit.conf << 'EOF'
upstream livekit_server {
    server livekit:7880;
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

    location / {
        proxy_pass http://livekit_server;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 86400;
    }
}
EOF
```

## Step 1.11: Create Temporary SSL Certificates

```bash
mkdir -p ~/cfmeet-production/ssl/live/api.cfmeet.com
mkdir -p ~/cfmeet-production/ssl/live/livekit.cfmeet.com

# Temporary self-signed (for nginx to start)
openssl req -x509 -nodes -days 1 -newkey rsa:2048 \
  -keyout ~/cfmeet-production/ssl/live/api.cfmeet.com/privkey.pem \
  -out ~/cfmeet-production/ssl/live/api.cfmeet.com/fullchain.pem \
  -subj "/CN=api.cfmeet.com"

openssl req -x509 -nodes -days 1 -newkey rsa:2048 \
  -keyout ~/cfmeet-production/ssl/live/livekit.cfmeet.com/privkey.pem \
  -out ~/cfmeet-production/ssl/live/livekit.cfmeet.com/fullchain.pem \
  -subj "/CN=livekit.cfmeet.com"
```

## Step 1.12: Start Services

```bash
cd ~/cfmeet-production

# Build and start
docker compose up -d --build

# Check status
docker compose ps

# View logs
docker compose logs -f
```

## Step 1.13: Get Real SSL Certificates

```bash
# Stop nginx temporarily
docker compose stop nginx

# Get certificates (CHANGE email and domains)
docker compose run --rm certbot certonly \
  --standalone \
  --email admin@cfmeet.com \
  --agree-tos \
  --no-eff-email \
  -d api.cfmeet.com

docker compose run --rm certbot certonly \
  --standalone \
  --email admin@cfmeet.com \
  --agree-tos \
  --no-eff-email \
  -d livekit.cfmeet.com

# Start nginx
docker compose start nginx
```

## Step 1.14: Verify Backend

```bash
# Test API
curl https://api.cfmeet.com/health

# Test Swagger
curl https://api.cfmeet.com/swagger-ui/
```

**Backend is now running!**

---

# Part 2: Web App Deployment (Vercel)

## Step 2.1: Connect to Vercel

1. Go to [vercel.com](https://vercel.com) and sign in
2. Click "Add New Project"
3. Import your GitHub repository

## Step 2.2: Configure Project

| Setting | Value |
|---------|-------|
| Framework Preset | Next.js |
| Root Directory | `apps/web` |
| Build Command | `npm run build` |
| Output Directory | `.next` |

## Step 2.3: Add Environment Variables

In Vercel Dashboard → Project → Settings → Environment Variables:

| Variable | Value | Environment |
|----------|-------|-------------|
| `NEXT_PUBLIC_API_URL` | `https://api.cfmeet.com/api/v1` | Production |

## Step 2.4: Configure Domain

1. Go to Project → Settings → Domains
2. Add `cfmeet.com` and `www.cfmeet.com`
3. Configure DNS at your registrar:

| Type | Name | Value |
|------|------|-------|
| A | @ | 76.76.21.21 |
| CNAME | www | cname.vercel-dns.com |

## Step 2.5: Deploy

Push to main branch or click "Deploy" in Vercel dashboard.

## Step 2.6: Verify Web App

Visit `https://cfmeet.com` - should load and connect to your API.

**Web app is now live!**

---

# Part 3: Mobile App Deployment (Expo/EAS)

## Step 3.1: Install EAS CLI

```bash
npm install -g eas-cli
eas login
```

## Step 3.2: Configure Project

```bash
cd apps/mobile

# Initialize EAS (first time only)
eas build:configure
```

## Step 3.3: Create eas.json

```bash
cat > eas.json << 'EOF'
{
  "cli": {
    "version": ">= 5.0.0"
  },
  "build": {
    "development": {
      "developmentClient": true,
      "distribution": "internal"
    },
    "preview": {
      "distribution": "internal",
      "android": {
        "buildType": "apk"
      },
      "env": {
        "EXPO_PUBLIC_API_URL": "https://api.cfmeet.com/api/v1"
      }
    },
    "production": {
      "distribution": "store",
      "env": {
        "EXPO_PUBLIC_API_URL": "https://api.cfmeet.com/api/v1"
      }
    }
  },
  "submit": {
    "production": {
      "ios": {
        "appleId": "your-apple-id@email.com",
        "ascAppId": "your-app-store-connect-app-id",
        "appleTeamId": "YOUR_TEAM_ID"
      },
      "android": {
        "serviceAccountKeyPath": "./google-play-key.json",
        "track": "internal"
      }
    }
  }
}
EOF
```

## Step 3.4: Build for Production

```bash
# Build both platforms
eas build --platform all --profile production

# Or build separately
eas build --platform ios --profile production
eas build --platform android --profile production
```

## Step 3.5: Submit to Stores

```bash
# Submit to App Store and Play Store
eas submit --platform all --latest

# Or submit separately
eas submit --platform ios --latest
eas submit --platform android --latest
```

**Mobile app submitted to stores!**

---

# Part 4: Post-Deployment

## Update Backend CORS

After deploying frontend, update `.env` on VPS:

```bash
FRONTEND_URLS=https://cfmeet.com,https://www.cfmeet.com
```

Then restart:

```bash
cd ~/cfmeet-production
docker compose restart meeting-service
```

## Setup SSL Auto-Renewal

```bash
# Create renewal script
cat > ~/cfmeet-production/renew-certs.sh << 'EOF'
#!/bin/bash
cd /home/cfmeet/cfmeet-production
docker compose run --rm certbot renew
docker compose exec nginx nginx -s reload
EOF

chmod +x ~/cfmeet-production/renew-certs.sh

# Add cron job (runs daily at 3am)
(crontab -l 2>/dev/null; echo "0 3 * * * /home/cfmeet/cfmeet-production/renew-certs.sh") | crontab -
```

## Setup Database Backups

```bash
# Create backup script
cat > ~/cfmeet-production/backup.sh << 'EOF'
#!/bin/bash
cd /home/cfmeet/cfmeet-production
DATE=$(date +%Y%m%d_%H%M%S)
docker exec cfmeet-postgres pg_dump -U cfmeet cfmeet | gzip > ~/backups/cfmeet_$DATE.sql.gz
find ~/backups -name "cfmeet_*.sql.gz" -mtime +7 -delete
EOF

mkdir -p ~/backups
chmod +x ~/cfmeet-production/backup.sh

# Add cron job (runs daily at 2am)
(crontab -l 2>/dev/null; echo "0 2 * * * /home/cfmeet/cfmeet-production/backup.sh") | crontab -
```

---

# Quick Reference

## Useful Commands

```bash
# View all logs
docker compose logs -f

# View specific service logs
docker compose logs -f meeting-service
docker compose logs -f livekit

# Restart services
docker compose restart
docker compose restart meeting-service

# Update backend code
cd ~/cfmeet && git pull
cp -r ~/cfmeet/services/meeting-service ~/cfmeet-production/services/
cd ~/cfmeet-production && docker compose up -d --build meeting-service

# Run database migration
docker compose exec meeting-service ./migration up

# Create database backup
docker exec cfmeet-postgres pg_dump -U cfmeet cfmeet | gzip > backup.sql.gz
```

## Health Check URLs

| Service | URL |
|---------|-----|
| API Health | `https://api.cfmeet.com/health` |
| API Swagger | `https://api.cfmeet.com/swagger-ui/` |
| Web App | `https://cfmeet.com` |

## Troubleshooting

| Issue | Solution |
|-------|----------|
| API not accessible | Check `docker compose ps`, verify firewall, check nginx logs |
| CORS errors | Verify `FRONTEND_URLS` in .env includes your frontend domain |
| SSL errors | Run certbot again, check certificate paths |
| LiveKit not connecting | Verify `node_ip` in livekit.yaml, check TURN ports are open |
| Mobile can't connect | Ensure using HTTPS URL, check API is accessible |

See [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) for more details.
