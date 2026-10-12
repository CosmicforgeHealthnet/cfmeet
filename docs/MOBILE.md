# Mobile App Deployment Guide

This guide covers deploying the CF Meet mobile application to iOS App Store and Google Play Store using Expo/EAS.

## Overview

The CF Meet mobile app is built with:
- React Native + Expo SDK 52
- Expo Router for navigation
- NativeWind (Tailwind CSS)
- LiveKit React Native SDK
- EAS Build for production builds

## Prerequisites

- Expo account ([expo.dev](https://expo.dev))
- Apple Developer account ($99/year) for iOS
- Google Play Developer account ($25 one-time) for Android
- EAS CLI installed: `npm install -g eas-cli`

## Project Configuration

### app.json / app.config.js

```javascript
// apps/mobile/app.config.js
export default {
  expo: {
    name: "CF Meet",
    slug: "cfmeet",
    version: "1.0.0",
    orientation: "portrait",
    icon: "./assets/icon.png",
    scheme: "cfmeet",
    userInterfaceStyle: "automatic",
    splash: {
      image: "./assets/splash.png",
      resizeMode: "contain",
      backgroundColor: "#006BFF"
    },
    assetBundlePatterns: ["**/*"],
    ios: {
      supportsTablet: true,
      bundleIdentifier: "com.cosmicforge.cfmeet",
      buildNumber: "1",
      infoPlist: {
        NSCameraUsageDescription: "CF Meet needs camera access for video calls",
        NSMicrophoneUsageDescription: "CF Meet needs microphone access for audio calls",
        UIBackgroundModes: ["audio", "voip"]
      }
    },
    android: {
      adaptiveIcon: {
        foregroundImage: "./assets/adaptive-icon.png",
        backgroundColor: "#006BFF"
      },
      package: "com.cosmicforge.cfmeet",
      versionCode: 1,
      permissions: [
        "android.permission.CAMERA",
        "android.permission.RECORD_AUDIO",
        "android.permission.MODIFY_AUDIO_SETTINGS"
      ]
    },
    web: {
      bundler: "metro",
      output: "static",
      favicon: "./assets/favicon.png"
    },
    plugins: [
      "expo-router",
      "expo-secure-store",
      [
        "expo-camera",
        {
          cameraPermission: "Allow CF Meet to access your camera for video calls."
        }
      ],
      [
        "expo-av",
        {
          microphonePermission: "Allow CF Meet to access your microphone for audio calls."
        }
      ]
    ],
    extra: {
      eas: {
        projectId: "your-eas-project-id"
      },
      apiUrl: process.env.EXPO_PUBLIC_API_URL
    }
  }
};
```

### eas.json

```json
{
  "cli": {
    "version": ">= 5.0.0"
  },
  "build": {
    "development": {
      "developmentClient": true,
      "distribution": "internal",
      "ios": {
        "simulator": true
      }
    },
    "preview": {
      "distribution": "internal",
      "ios": {
        "simulator": false
      },
      "android": {
        "buildType": "apk"
      },
      "env": {
        "EXPO_PUBLIC_API_URL": "https://staging-api.cfmeet.com/api/v1"
      }
    },
    "production": {
      "distribution": "store",
      "ios": {
        "simulator": false
      },
      "android": {
        "buildType": "app-bundle"
      },
      "env": {
        "EXPO_PUBLIC_API_URL": "https://api.cfmeet.com/api/v1"
      }
    }
  },
  "submit": {
    "production": {
      "ios": {
        "appleId": "your-apple-id@email.com",
        "ascAppId": "1234567890",
        "appleTeamId": "ABCD1234"
      },
      "android": {
        "serviceAccountKeyPath": "./google-play-key.json",
        "track": "internal"
      }
    }
  }
}
```

## Build Process

### 1. Login to EAS

```bash
cd apps/mobile
eas login
```

### 2. Configure EAS Project

```bash
# Initialize EAS (first time only)
eas build:configure

# Update project ID in app.config.js
```

### 3. Build for Development

```bash
# iOS Simulator
eas build --platform ios --profile development

# Android APK
eas build --platform android --profile preview
```

### 4. Build for Production

```bash
# iOS (App Store)
eas build --platform ios --profile production

# Android (Play Store)
eas build --platform android --profile production

# Both platforms
eas build --platform all --profile production
```

## iOS Deployment

### App Store Connect Setup

1. Go to [App Store Connect](https://appstoreconnect.apple.com)
2. Create new app with bundle ID: `com.cosmicforge.cfmeet`
3. Fill in app information:
   - Name: CF Meet
   - Primary Language: English
   - Bundle ID: com.cosmicforge.cfmeet
   - SKU: cfmeet

### Required Assets

| Asset | Size | Notes |
|-------|------|-------|
| App Icon | 1024x1024 | No transparency, no rounded corners |
| Screenshots (iPhone 6.7") | 1290x2796 | At least 2 |
| Screenshots (iPhone 6.5") | 1284x2778 | At least 2 |
| Screenshots (iPad 12.9") | 2048x2732 | If supporting iPad |

### Submit to App Store

```bash
# Auto-submit after build
eas submit --platform ios --latest

# Or submit specific build
eas submit --platform ios --id <build-id>
```

### TestFlight

1. After submission, app goes to TestFlight automatically
2. Add internal testers (up to 100)
3. Add external testers (up to 10,000)
4. Collect feedback before App Store release

## Android Deployment

### Google Play Console Setup

1. Go to [Google Play Console](https://play.google.com/console)
2. Create new app: CF Meet
3. Complete app content declarations
4. Set up internal testing track

### Required Assets

| Asset | Size | Notes |
|-------|------|-------|
| App Icon | 512x512 | 32-bit PNG |
| Feature Graphic | 1024x500 | Required |
| Screenshots | Variable | At least 2 per device type |
| Privacy Policy URL | - | Required |

### Create Service Account

1. Go to Google Cloud Console
2. Create service account with Play Console access
3. Download JSON key file
4. Place in `apps/mobile/google-play-key.json`
5. Add to `.gitignore`

### Submit to Play Store

```bash
# Auto-submit after build
eas submit --platform android --latest

# Or submit specific build
eas submit --platform android --id <build-id>
```

### Release Tracks

| Track | Description |
|-------|-------------|
| Internal | Up to 100 testers, instant availability |
| Closed | Invite-only testing groups |
| Open | Public beta testing |
| Production | Full release |

## Environment Variables

### Development

```bash
# apps/mobile/.env
EXPO_PUBLIC_API_URL=http://localhost:8080/api/v1
```

### Staging

```bash
# apps/mobile/.env.staging
EXPO_PUBLIC_API_URL=https://staging-api.cfmeet.com/api/v1
```

### Production

```bash
# apps/mobile/.env.production
EXPO_PUBLIC_API_URL=https://api.cfmeet.com/api/v1
```

## Over-the-Air Updates

EAS Update allows instant updates without App Store review:

### Configure EAS Update

```bash
# Initialize updates
eas update:configure
```

### Push Update

```bash
# Update production
eas update --branch production --message "Bug fixes"

# Update preview/staging
eas update --branch preview --message "New feature"
```

### Update Policy

In `app.config.js`:

```javascript
updates: {
  enabled: true,
  checkAutomatically: "ON_LOAD",
  fallbackToCacheTimeout: 30000,
  url: "https://u.expo.dev/your-project-id"
}
```

## CI/CD Integration

### GitHub Actions

```yaml
# .github/workflows/mobile.yml
name: Mobile CI/CD

on:
  push:
    branches: [main]
    paths:
      - 'apps/mobile/**'
  pull_request:
    paths:
      - 'apps/mobile/**'

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: '22'

      - name: Setup Expo
        uses: expo/expo-github-action@v8
        with:
          eas-version: latest
          token: ${{ secrets.EXPO_TOKEN }}

      - name: Install dependencies
        run: cd apps/mobile && npm ci

      - name: Lint
        run: cd apps/mobile && npm run lint

      - name: Type check
        run: cd apps/mobile && npm run typecheck

  build-preview:
    needs: build
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Setup Expo
        uses: expo/expo-github-action@v8
        with:
          eas-version: latest
          token: ${{ secrets.EXPO_TOKEN }}

      - name: Build preview
        run: cd apps/mobile && eas build --platform all --profile preview --non-interactive

  deploy-production:
    needs: build
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Setup Expo
        uses: expo/expo-github-action@v8
        with:
          eas-version: latest
          token: ${{ secrets.EXPO_TOKEN }}

      - name: Build and submit
        run: |
          cd apps/mobile
          eas build --platform all --profile production --non-interactive
          eas submit --platform all --latest --non-interactive
```

## Troubleshooting

### Common Issues

1. **Build fails with native module errors**
   ```bash
   # Clear cache and rebuild
   cd apps/mobile
   npx expo prebuild --clean
   eas build --clear-cache
   ```

2. **iOS provisioning profile errors**
   - Check Apple Developer account membership
   - Ensure bundle ID matches
   - Regenerate profiles: `eas credentials`

3. **Android keystore issues**
   ```bash
   # Manage Android credentials
   eas credentials --platform android
   ```

4. **Camera/Microphone not working**
   - Check permissions in app.config.js
   - Verify native modules are linked
   - Test on real device (not simulator)

### Debug Commands

```bash
# View build logs
eas build:list
eas build:view <build-id>

# Check credentials
eas credentials

# View project info
eas project:info
```

## App Store Guidelines Compliance

### iOS

- [ ] Privacy policy URL provided
- [ ] All required permissions explained
- [ ] No private API usage
- [ ] Content rating completed
- [ ] Screenshots accurate and up-to-date

### Android

- [ ] Privacy policy URL provided
- [ ] Content rating questionnaire completed
- [ ] Data safety section filled out
- [ ] Target API level meets requirements
- [ ] 64-bit support included

## Version Management

### Semantic Versioning

```json
{
  "version": "1.2.3"  // Marketing version
}
```

### Build Numbers

```javascript
// app.config.js
ios: {
  buildNumber: "42"  // Increment each submission
},
android: {
  versionCode: 42    // Must increase each upload
}
```

### Auto-increment in CI

```bash
# Use EAS auto-versioning
eas build --auto-submit --platform all
```
