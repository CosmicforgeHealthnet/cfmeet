# Google OAuth Configuration

## Overview

Google OAuth allows users to sign in with their Google account. The flow works across both the backend (Rust/Axum) and frontend (Next.js):

1. Frontend redirects the browser to the backend OAuth init endpoint
2. Backend generates a CSRF state token, stores it in the `oauth_states` table, and redirects to Google
3. Google authenticates the user and redirects back to the backend callback
4. Backend validates the state, exchanges the code for tokens, creates/finds the user, generates JWTs
5. Backend redirects to the frontend callback page with tokens as query params
6. Frontend stores tokens in cookies + localStorage and redirects to the dashboard

## Environment Variables

### Backend (`backend/.env`)

```env
# Google OAuth
GOOGLE_CLIENT_ID=your-app-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-client-secret
GOOGLE_REDIRECT_URL=http://localhost:8080/api/v1/auth/oauth/google/callback

# Frontend URL (used for redirecting after OAuth callback)
APP_URL=http://localhost:3000
```

### Frontend (`frontend/.env.local`)

```env
# Must point to the backend, NOT the frontend
NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1
```

## Setup Instructions

### 1. Create Google OAuth App

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Create a new project or select an existing one
3. Navigate to **APIs & Services** > **OAuth consent screen**
   - Choose "External" user type
   - Fill in app name, support email, and developer contact
   - Add scopes: `openid`, `email`, `profile`
4. Navigate to **APIs & Services** > **Credentials**
5. Click **Create Credentials** > **OAuth client ID**
   - Application type: **Web application**
   - Authorized JavaScript origins: `http://localhost:3000`
   - Authorized redirect URIs: `http://localhost:8080/api/v1/auth/oauth/google/callback`
6. Copy the **Client ID** and **Client Secret**

### 2. Configure Environment Variables

Add the credentials to `backend/.env`:

```env
GOOGLE_CLIENT_ID=466386896934-xxxxx.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=GOCSPX-xxxxx
GOOGLE_REDIRECT_URL=http://localhost:8080/api/v1/auth/oauth/google/callback
APP_URL=http://localhost:3000
```

Ensure `frontend/.env.local` points to the backend:

```env
NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1
```

### 3. Run Database Migration

The `oauth_states` table must exist. Run the migration:

```powershell
cd backend/migration
cargo run -- up
```

This applies `m20260127_000001_create_oauth_states` which creates:
- `oauth_states` table with columns: `id`, `state`, `provider`, `created_at`, `expires_at`
- Indexes on `state` and `expires_at`

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/auth/oauth/google` | Initiates OAuth flow (redirects browser to Google) |
| GET | `/api/v1/auth/oauth/google/callback` | Handles Google callback, redirects to frontend with tokens |

### Callback Redirect

On **success**, the backend redirects to:
```
{APP_URL}/auth/google/callback?access_token=...&refresh_token=...&user_id=...&username=...&role=...
```

On **error**, the backend redirects to:
```
{APP_URL}/auth/google/callback?error=...
```

## Frontend Integration

### Google Button (LoginModal / SignupModal)

Both modals have a "Continue with Google" button that navigates to the backend OAuth init endpoint:

```tsx
const API_URL = process.env.NEXT_PUBLIC_API_URL || "http://localhost:3001/api/v1";

const handleGoogleLogin = () => {
  window.location.href = `${API_URL}/auth/oauth/google`;
};
```

### Callback Page (`/auth/google/callback`)

Located at `frontend/src/app/auth/google/callback/page.tsx`. This page:

1. Reads `access_token`, `refresh_token`, `user_id`, `username`, `role` from URL query params
2. Stores tokens via `cookieStore.setTokens()` and user info via `storageStore.setUser()`
3. Redirects to `/dashboard`
4. On error, displays the error message with a "Back to Login" button

## Testing the Flow

1. Start the backend:
   ```powershell
   cd backend
   cargo run
   ```

2. Start the frontend:
   ```powershell
   cd frontend
   npm run dev
   ```

3. Navigate to `http://localhost:3000/login`

4. Click **"Continue with Google"**

5. Expected flow:
   - Browser navigates to `http://localhost:8080/api/v1/auth/oauth/google`
   - Backend redirects to Google consent page
   - User selects Google account and grants permissions
   - Google redirects to backend callback with `code` and `state`
   - Backend validates state, exchanges code for Google tokens
   - Backend decodes Google ID token, creates/finds user
   - Backend generates JWT access + refresh tokens
   - Backend redirects to `http://localhost:3000/auth/google/callback?access_token=...`
   - Frontend stores tokens and redirects to `/dashboard`

## User Handling

| Scenario | Behavior |
|----------|----------|
| New user (email not in DB) | Created with `auth_type=Google`, `status=Active`, no password |
| Existing user (pending verification) | Upgraded to `status=Active` (Google verified the email) |
| Existing user (active) | Logged in normally |
| Existing user (inactive/deactivated) | Blocked with "Account has been deactivated" error |

## Security Notes

- **CSRF protection**: Random 32-char state token stored in DB, validated on callback
- **State expiry**: Tokens expire after 10 minutes
- **Single-use state**: Deleted from DB immediately after validation
- **ID token validation**: Issuer (`iss`) is verified against `accounts.google.com`
- **Session cookies**: Google OAuth users get persistent cookies by default
- **No password**: OAuth users have `password_hash = NULL`, cannot use local login

## Troubleshooting

| Issue | Cause | Fix |
|-------|-------|-----|
| `relation "oauth_states" does not exist` | Migration not run | Run `cd backend/migration && cargo run -- up` |
| Google button goes to `localhost:3000` | Wrong `NEXT_PUBLIC_API_URL` | Set to `http://localhost:8080/api/v1` (no trailing slash), restart Next.js |
| `redirect_uri_mismatch` from Google | `GOOGLE_REDIRECT_URL` doesn't match Google Console | Ensure both use exactly `http://localhost:8080/api/v1/auth/oauth/google/callback` |
| `Invalid or expired state` | State expired or was already used | Try again; state tokens are single-use and expire in 10 min |
| Blank page after Google consent | Backend callback returning JSON | Ensure backend `oauth_google_callback` returns `Redirect`, not `Json` |
| `Token exchange failed` | Wrong client secret or redirect URL | Verify `GOOGLE_CLIENT_SECRET` and `GOOGLE_REDIRECT_URL` in `.env` |
