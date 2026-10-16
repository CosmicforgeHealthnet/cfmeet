# Vercel Deployment Guide (Web App)

This guide covers deploying the CF Meet web application to Vercel.

## Overview

The CF Meet web app is a Next.js 14 application with:
- App Router architecture
- Server and Client components
- Tailwind CSS + shadcn/ui
- React Query for data fetching
- Real-time features via LiveKit React SDK

## Prerequisites

- Vercel account (free tier works for getting started)
- GitHub repository connected to Vercel
- Backend (Meeting Service) deployed and accessible
- Domain name (optional, Vercel provides free `.vercel.app` subdomain)

## Deployment Steps

### 1. Connect Repository to Vercel

1. Go to [vercel.com](https://vercel.com) and sign in
2. Click "Add New Project"
3. Import your GitHub repository (`CosmicforgeHealthnet/cfmeet`)
4. Configure the project:

| Setting | Value |
|---------|-------|
| Framework Preset | Next.js |
| Root Directory | `apps/web` |
| Build Command | `npm run build` (auto-detected) |
| Output Directory | `.next` (auto-detected) |
| Install Command | `npm install` (auto-detected) |

### 2. Configure Environment Variables

In Vercel Dashboard > Project > Settings > Environment Variables:

| Variable | Value | Environment |
|----------|-------|-------------|
| `NEXT_PUBLIC_API_URL` | `https://api.cfmeet.com/api/v1` | Production |
| `NEXT_PUBLIC_API_URL` | `https://staging-api.cfmeet.com/api/v1` | Preview |
| `NEXT_PUBLIC_API_URL` | `http://localhost:8080/api/v1` | Development |

### 3. Configure Domain

1. Go to Project > Settings > Domains
2. Add your custom domain (e.g., `cfmeet.com`, `www.cfmeet.com`)
3. Configure DNS at your registrar:

```
# DNS Records
Type    Name    Value
A       @       76.76.21.21
CNAME   www     cname.vercel-dns.com
```

### 4. Deploy

```bash
# Push to main branch triggers automatic deployment
git push origin main

# Or deploy manually via Vercel CLI
npm i -g vercel
cd apps/web
vercel --prod
```

## Vercel Configuration File

Create `apps/web/vercel.json` for advanced configuration:

```json
{
  "framework": "nextjs",
  "buildCommand": "npm run build",
  "installCommand": "npm install",
  "regions": ["iad1"],
  "headers": [
    {
      "source": "/(.*)",
      "headers": [
        {
          "key": "X-Frame-Options",
          "value": "DENY"
        },
        {
          "key": "X-Content-Type-Options",
          "value": "nosniff"
        },
        {
          "key": "Referrer-Policy",
          "value": "strict-origin-when-cross-origin"
        }
      ]
    }
  ],
  "rewrites": [
    {
      "source": "/api/:path*",
      "destination": "https://api.cfmeet.com/api/:path*"
    }
  ]
}
```

## Environment-Specific Builds

### Production (main branch)
- Domain: `cfmeet.com`
- API: `https://api.cfmeet.com/api/v1`
- Automatic deployment on push to `main`

### Preview (Pull Requests)
- Domain: `cfmeet-git-<branch>-<team>.vercel.app`
- API: Staging or production (configurable)
- Automatic deployment on PR creation

### Development (Local)
- Domain: `localhost:3000`
- API: `http://localhost:8080/api/v1`

## Monorepo Setup

Since CF Meet is a monorepo, configure Vercel to only deploy when `apps/web` changes:

### Ignored Build Step

In `apps/web/package.json`, add:

```json
{
  "scripts": {
    "vercel-build": "npm run build"
  }
}
```

Or use Vercel's "Ignored Build Step" feature:
1. Go to Project > Settings > Git
2. Set "Ignored Build Step" to:
   ```bash
   git diff HEAD^ HEAD --quiet -- apps/web
   ```

This skips deployment if only backend or other files changed.

## Performance Optimization

### Edge Runtime

For API routes that need low latency:

```typescript
// apps/web/src/app/api/example/route.ts
export const runtime = 'edge';

export async function GET() {
  return Response.json({ message: 'Hello from the edge!' });
}
```

### Image Optimization

Configure Next.js image domains in `next.config.js`:

```javascript
/** @type {import('next').NextConfig} */
const nextConfig = {
  images: {
    remotePatterns: [
      {
        protocol: 'https',
        hostname: 'api.cfmeet.com',
        pathname: '/uploads/**',
      },
      {
        protocol: 'https',
        hostname: 'lh3.googleusercontent.com',
        pathname: '/**',
      },
    ],
  },
};

module.exports = nextConfig;
```

### Caching

Vercel automatically caches static assets. For dynamic data, configure cache headers:

```typescript
// In API routes or server components
export const revalidate = 60; // Revalidate every 60 seconds
```

## Monitoring and Analytics

### Vercel Analytics

1. Go to Project > Analytics
2. Enable Web Analytics (free tier available)

### Speed Insights

1. Go to Project > Speed Insights
2. Enable to track Core Web Vitals

### Logs

1. Go to Project > Logs
2. Filter by:
   - Function logs (API routes)
   - Build logs
   - Edge function logs

## Troubleshooting

### Common Issues

1. **Build fails with TypeScript errors**
   ```bash
   # Check types locally first
   cd apps/web
   npm run typecheck
   ```

2. **Environment variables not working**
   - Ensure variables are set for correct environment (Production/Preview/Development)
   - Variables starting with `NEXT_PUBLIC_` are exposed to browser
   - Redeploy after adding/changing variables

3. **API calls failing**
   - Check CORS configuration on backend
   - Verify `NEXT_PUBLIC_API_URL` is correct
   - Check network tab for actual request URL

4. **500 errors on API routes**
   - Check Vercel function logs
   - Verify environment variables are set
   - Check for runtime errors in server components

### Debug Locally

```bash
# Build production locally
cd apps/web
npm run build

# Run production build locally
npm start

# Or use Vercel CLI
vercel dev
```

### Check Deployment Status

```bash
# Via Vercel CLI
vercel ls

# Check deployment details
vercel inspect <deployment-url>
```

## CI/CD Integration

### GitHub Actions (Optional)

If you want more control over the build process:

```yaml
# .github/workflows/frontend.yml
name: Frontend CI/CD

on:
  push:
    branches: [main]
    paths:
      - 'apps/web/**'
  pull_request:
    paths:
      - 'apps/web/**'

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: '22'
          cache: 'npm'
          cache-dependency-path: apps/web/package-lock.json

      - name: Install dependencies
        run: cd apps/web && npm ci

      - name: Lint
        run: cd apps/web && npm run lint

      - name: Type check
        run: cd apps/web && npm run typecheck

      - name: Build
        run: cd apps/web && npm run build
        env:
          NEXT_PUBLIC_API_URL: https://api.cfmeet.com/api/v1

  deploy:
    needs: build
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Deploy to Vercel
        uses: amondnet/vercel-action@v25
        with:
          vercel-token: ${{ secrets.VERCEL_TOKEN }}
          vercel-org-id: ${{ secrets.VERCEL_ORG_ID }}
          vercel-project-id: ${{ secrets.VERCEL_PROJECT_ID }}
          working-directory: apps/web
          vercel-args: '--prod'
```

## Rollback

### Via Dashboard
1. Go to Project > Deployments
2. Find the previous working deployment
3. Click "..." > "Promote to Production"

### Via CLI
```bash
# List deployments
vercel ls

# Rollback to specific deployment
vercel rollback <deployment-url>
```

## Cost Optimization

### Free Tier Limits (Hobby)
- 100 GB bandwidth/month
- Serverless function execution: 100 GB-hours
- Edge function execution: 500,000 invocations

### Pro Tier ($20/month per member)
- 1 TB bandwidth/month
- Serverless: 1000 GB-hours
- Edge: 1,000,000 invocations
- Team features

### Optimization Tips
1. Use static pages where possible (ISR)
2. Optimize images with Next.js Image component
3. Use edge functions for simple operations
4. Cache API responses appropriately
