# Troubleshooting Guide

This guide covers common issues and their solutions when deploying and running CF Meet.

## Quick Diagnostics

### Health Check Commands

```bash
# Backend API health
curl -s https://api.cfmeet.com/health | jq

# Check API version
curl -s https://api.cfmeet.com/ | jq

# Database connectivity (from backend container)
docker exec cfmeet-api /bin/sh -c 'echo "SELECT 1" | psql $DATABASE_URL'

# Redis connectivity
docker exec cfmeet-redis redis-cli ping

# LiveKit connectivity
livekit-cli room list --url $LIVEKIT_URL --api-key $LIVEKIT_API_KEY --api-secret $LIVEKIT_API_SECRET
```

---

## Backend Issues

### 1. Service Won't Start

**Symptoms:**
- Container exits immediately
- "Error: environment variable not set" messages

**Solutions:**

```bash
# Check container logs
docker logs cfmeet-api

# Verify environment variables
docker exec cfmeet-api env | grep -E "(DATABASE|REDIS|JWT|LIVEKIT)"

# Common fixes:
# 1. Ensure .env file exists and is mounted
# 2. Check for typos in variable names
# 3. Ensure DATABASE_URL includes full connection string
```

### 2. Database Connection Failed

**Symptoms:**
- "connection refused" errors
- "FATAL: password authentication failed"
- Service starts but crashes on first request

**Solutions:**

```bash
# Test database connectivity
psql $DATABASE_URL -c "SELECT 1"

# Check if database exists
psql $DATABASE_URL -c "\l"

# Common issues:
# 1. Wrong password in DATABASE_URL
# 2. Database server not running
# 3. Firewall blocking port 5432
# 4. Docker network issues (use service name, not localhost)

# If using Docker Compose, ensure correct hostname:
DATABASE_URL=postgres://user:pass@db:5432/cfmeet  # ✓ Use service name
DATABASE_URL=postgres://user:pass@localhost:5432/cfmeet  # ✗ Won't work inside container
```

### 3. Redis Connection Issues

**Symptoms:**
- "Connection refused" to Redis
- Session/token operations failing
- Rate limiting not working

**Solutions:**

```bash
# Test Redis connectivity
redis-cli -h $REDIS_HOST ping

# Check Redis logs
docker logs cfmeet-redis

# Common fixes:
# 1. Ensure Redis container is running
# 2. Check REDIS_URL format: redis://host:6379
# 3. If using password: redis://:password@host:6379
```

### 4. JWT Token Errors

**Symptoms:**
- "Invalid token" errors
- Users logged out unexpectedly
- "Token expired" when token should be valid

**Solutions:**

```bash
# Ensure JWT_SECRET is consistent across deployments
# If you've changed JWT_SECRET, all existing tokens become invalid

# Check token expiration settings
JWT_ACCESS_TOKEN_EXPIRE_MINUTES=15
JWT_REFRESH_TOKEN_EXPIRE_DAYS=7

# Debug token (using jwt-cli or online decoder)
# Never log or expose tokens in production!
```

### 5. CORS Errors

**Symptoms:**
- "Access-Control-Allow-Origin" errors in browser console
- API calls fail from frontend but work in Postman

**Solutions:**

```bash
# Check FRONTEND_URLS environment variable
# Must include ALL frontend domains

# Example:
FRONTEND_URLS=https://cfmeet.com,https://www.cfmeet.com,https://staging.cfmeet.com

# For development:
FRONTEND_URLS=http://localhost:3000,http://localhost:5173

# Common mistakes:
# - Missing https:// prefix
# - Trailing slash (don't include: https://cfmeet.com/)
# - Missing www variant
```

### 6. Email Sending Failures

**Symptoms:**
- Registration emails not received
- Password reset emails not sent
- "SMTP connection failed" errors

**Solutions:**

```bash
# Test SMTP connectivity
openssl s_client -starttls smtp -connect smtp.gmail.com:587

# Check SMTP configuration
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password  # Use App Password, not regular password

# Gmail-specific:
# 1. Enable 2FA
# 2. Generate App Password at https://myaccount.google.com/apppasswords
# 3. Use the 16-character app password

# Check spam folder
# Add SPF/DKIM records to your domain
```

### 7. LiveKit Connection Issues

**Symptoms:**
- "Failed to connect to LiveKit" errors
- Video/audio not working in meetings
- WebSocket connection errors

**Solutions:**

```bash
# Verify LiveKit URL format
LIVEKIT_URL=wss://your-livekit-server:7880  # Must use wss:// for secure

# Test LiveKit API
curl https://your-livekit-server:7880/health

# Check credentials
livekit-cli room list --url $LIVEKIT_URL --api-key $LIVEKIT_API_KEY --api-secret $LIVEKIT_API_SECRET

# Common issues:
# 1. Wrong protocol (use wss:// not https://)
# 2. Firewall blocking WebSocket ports
# 3. API key/secret mismatch
# 4. SSL certificate issues
```

---

## Frontend Issues (Web App)

### 1. Build Failures

**Symptoms:**
- TypeScript errors during build
- "Module not found" errors
- Build timeout on Vercel

**Solutions:**

```bash
# Run build locally first
cd apps/web
npm run build

# Check for TypeScript errors
npm run typecheck

# Clear caches
rm -rf .next node_modules
npm install
npm run build

# Common issues:
# 1. Missing dependencies in package.json
# 2. TypeScript strict mode errors
# 3. Import path issues
```

### 2. Environment Variables Not Working

**Symptoms:**
- API calls going to wrong URL
- `undefined` values in console

**Solutions:**

```bash
# Ensure variables start with NEXT_PUBLIC_
NEXT_PUBLIC_API_URL=https://api.cfmeet.com/api/v1  # ✓ Accessible in browser
API_URL=https://api.cfmeet.com/api/v1  # ✗ Server-side only

# Vercel-specific:
# 1. Add variables in Vercel Dashboard > Settings > Environment Variables
# 2. Select correct environment (Production/Preview/Development)
# 3. Redeploy after adding variables

# Check current value
console.log(process.env.NEXT_PUBLIC_API_URL)
```

### 3. API Calls Failing

**Symptoms:**
- 404 errors on API requests
- Network errors in browser
- CORS errors

**Solutions:**

```bash
# Check API URL configuration
# Browser Console:
console.log(process.env.NEXT_PUBLIC_API_URL)

# Verify API is accessible
curl https://api.cfmeet.com/api/v1/health

# Common issues:
# 1. Missing /api/v1 in URL
# 2. CORS not configured on backend
# 3. SSL certificate issues
# 4. API server not running
```

### 4. Authentication Loop

**Symptoms:**
- Redirected to login after logging in
- Token refresh failing
- "Unauthorized" errors

**Solutions:**

```bash
# Clear cookies and local storage
# Browser DevTools > Application > Clear Storage

# Check token storage
localStorage.getItem('accessToken')
localStorage.getItem('refreshToken')

# Verify API base URL is correct
# Tokens issued for one domain won't work on another
```

---

## Mobile App Issues

### 1. EAS Build Failures

**Symptoms:**
- Build fails with native module errors
- Dependency resolution errors

**Solutions:**

```bash
# Clear Expo cache
cd apps/mobile
npx expo prebuild --clean
eas build --clear-cache --platform ios

# Check EAS CLI version
eas --version
npm install -g eas-cli@latest

# Verify app.json/app.config.js
npx expo config --type public
```

### 2. API Connection on Device

**Symptoms:**
- Works in simulator but not on device
- Network request failed
- Connection timeout

**Solutions:**

```bash
# For local development on physical device:
# 1. Use your computer's local IP, not localhost
EXPO_PUBLIC_API_URL=http://192.168.1.100:8080/api/v1

# 2. Ensure device and computer are on same network

# 3. Check firewall isn't blocking connections
# On macOS:
sudo pfctl -d  # Temporarily disable firewall

# For production:
# Ensure using HTTPS URL
EXPO_PUBLIC_API_URL=https://api.cfmeet.com/api/v1
```

### 3. Camera/Microphone Not Working

**Symptoms:**
- Black screen in video
- No audio in calls
- Permission denied errors

**Solutions:**

```bash
# Check permissions in app.json
{
  "ios": {
    "infoPlist": {
      "NSCameraUsageDescription": "CF Meet needs camera access for video calls",
      "NSMicrophoneUsageDescription": "CF Meet needs microphone access for audio calls"
    }
  }
}

# Test on real device (not simulator)
# Simulators have limited media support

# Rebuild after changing permissions
npx expo prebuild --clean
eas build --platform ios
```

### 4. App Store Rejection

**Symptoms:**
- Rejected for privacy reasons
- Missing permissions explanation
- Incomplete app metadata

**Solutions:**

```markdown
Common rejection reasons and fixes:

1. **Privacy Policy Missing**
   - Add privacy policy URL in App Store Connect
   - Host policy at https://cfmeet.com/privacy

2. **Permission Explanations**
   - Ensure all NSUsageDescription strings are clear
   - Explain why each permission is needed

3. **Login Issues**
   - Provide demo account if login is required
   - Ensure registration works during review

4. **Crashes**
   - Test thoroughly on older devices
   - Check crash logs in App Store Connect
```

---

## Infrastructure Issues

### 1. SSL Certificate Errors

**Symptoms:**
- "Certificate expired" warnings
- "Unable to verify certificate" errors
- HTTPS not working

**Solutions:**

```bash
# Check certificate expiry
openssl s_client -connect api.cfmeet.com:443 2>/dev/null | openssl x509 -noout -dates

# Renew Let's Encrypt certificate
certbot renew

# Auto-renewal cron
0 0 * * * certbot renew --quiet --post-hook "systemctl reload nginx"

# Check certificate chain
curl -vI https://api.cfmeet.com 2>&1 | grep -A5 "Server certificate"
```

### 2. Memory Issues

**Symptoms:**
- Container OOM killed
- Slow performance
- Swap usage high

**Solutions:**

```bash
# Check container memory usage
docker stats

# Increase container memory limits
# docker-compose.yml:
services:
  meeting-service:
    deploy:
      resources:
        limits:
          memory: 512M
        reservations:
          memory: 256M

# PostgreSQL memory tuning
# Reduce shared_buffers if memory is limited
shared_buffers = 128MB
```

### 3. Disk Space Issues

**Symptoms:**
- "No space left on device" errors
- Database operations failing
- Docker builds failing

**Solutions:**

```bash
# Check disk usage
df -h

# Clean Docker resources
docker system prune -a --volumes

# Clean old logs
find /var/log -name "*.gz" -mtime +7 -delete

# PostgreSQL cleanup
# Remove old WAL files
psql -c "SELECT pg_switch_wal();"
```

### 4. High CPU Usage

**Symptoms:**
- Service responding slowly
- High load average
- Container throttled

**Solutions:**

```bash
# Identify CPU-heavy processes
top -c

# Check container CPU usage
docker stats

# Common causes:
# 1. Database queries without indexes
# 2. Infinite loops in code
# 3. Too many concurrent connections

# Add database indexes
psql -c "CREATE INDEX CONCURRENTLY idx_users_email ON users(email);"
```

---

## Debugging Commands

### Backend Service

```bash
# View real-time logs
docker logs -f cfmeet-api

# Filter for errors
docker logs cfmeet-api 2>&1 | grep -i error

# Enter container shell
docker exec -it cfmeet-api /bin/sh

# Check environment
docker exec cfmeet-api env

# Test database migration
docker exec cfmeet-api /app/migration status
```

### Database

```bash
# Connect to PostgreSQL
docker exec -it cfmeet-db psql -U cfmeet

# Check active connections
SELECT * FROM pg_stat_activity WHERE datname = 'cfmeet';

# Check table sizes
SELECT relname, pg_size_pretty(pg_total_relation_size(relid))
FROM pg_catalog.pg_statio_user_tables
ORDER BY pg_total_relation_size(relid) DESC;

# Check slow queries
SELECT query, calls, mean_time, total_time
FROM pg_stat_statements
ORDER BY mean_time DESC
LIMIT 10;
```

### Redis

```bash
# Connect to Redis CLI
docker exec -it cfmeet-redis redis-cli

# Check memory usage
INFO memory

# List all keys (use with caution in production)
KEYS *

# Check key TTL
TTL your_key

# Monitor real-time commands
MONITOR
```

### Network

```bash
# Check if port is listening
netstat -tlnp | grep 8080

# Test internal connectivity
docker exec cfmeet-api ping db

# Check DNS resolution
docker exec cfmeet-api nslookup db

# Trace network issues
docker exec cfmeet-api traceroute api.cfmeet.com
```

---

## Error Code Reference

### HTTP Status Codes

| Code | Meaning | Common Cause |
|------|---------|--------------|
| 400 | Bad Request | Invalid request body or parameters |
| 401 | Unauthorized | Missing or invalid auth token |
| 403 | Forbidden | Valid token but insufficient permissions |
| 404 | Not Found | Resource doesn't exist |
| 409 | Conflict | Duplicate entry (email, username) |
| 422 | Unprocessable Entity | Validation error |
| 429 | Too Many Requests | Rate limit exceeded |
| 500 | Internal Server Error | Server-side bug |
| 502 | Bad Gateway | Backend unreachable |
| 503 | Service Unavailable | Service overloaded or down |

### API Error Codes

| Code | Description |
|------|-------------|
| `AUTH001` | Invalid credentials |
| `AUTH002` | Token expired |
| `AUTH003` | Token invalid |
| `AUTH004` | Email not verified |
| `USER001` | User not found |
| `USER002` | Email already exists |
| `USER003` | Username already exists |
| `MEET001` | Meeting not found |
| `MEET002` | Meeting ended |
| `MEET003` | Meeting capacity reached |

---

## Getting Help

### Log Collection

When reporting issues, collect:

```bash
# Backend logs (last 1000 lines)
docker logs --tail 1000 cfmeet-api > backend-logs.txt

# System info
docker info > docker-info.txt
docker-compose ps > container-status.txt

# Configuration (remove secrets!)
env | grep -v PASSWORD | grep -v SECRET > env-vars.txt
```

### Support Channels

1. **Documentation**: Check [Meeting Service Docs](../../services/meeting-service/docs/README.md)
2. **GitHub Issues**: Report bugs at the project repository
3. **Health Endpoints**: Check `/health` for service status

### Common Questions

**Q: How do I reset a user's password?**
```sql
-- Generate a password reset token via API, or manually:
UPDATE users SET password_hash = 'NEW_HASH' WHERE email = 'user@example.com';
```

**Q: How do I clear all sessions for a user?**
```bash
# Delete user's tokens from Redis
redis-cli KEYS "refresh_token:USER_ID:*" | xargs redis-cli DEL
```

**Q: How do I check database migrations status?**
```bash
docker exec cfmeet-api /app/migration status
```
