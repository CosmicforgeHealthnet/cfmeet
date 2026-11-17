# Passwordless sign-in: backend handover to the frontend team

**Status:** the backend is passwordless (email + one-time code) and is deployed to production. The
password endpoints are gone. The web and mobile apps still call them, so their sign-in, sign-up,
password-reset and change-password screens fail until they move to the endpoints below. The
frontend team owns all web and mobile authentication changes. The backend team made no change to
web or mobile source, UI or frontend tests.

The authoritative contract is `packages/contracts/openapi/meeting-service.json` (tag
*Authentication* and *Account security (v2)*). Regenerate `apps/web/src/types/generated` from it.

## 1. What was removed (all now `404`)

| Removed endpoint | Replacement |
|---|---|
| `POST /api/v1/auth/register` | `POST /api/v1/auth/otp/request` then `/otp/verify` (the first verification creates the account) |
| `POST /api/v1/auth/login` | same |
| `POST /api/v1/auth/verify-email`, `/resend-verification` | not needed: a verified code proves the address |
| `POST /api/v1/auth/forgot-password`, `/verify-reset-token`, `/reset-password` | not needed: there is no password |
| `POST /api/v1/auth/change-password` | not needed |
| `POST /api/v1/users/me/deactivate` body `{current_password}` | body `{step_up_token}` (see §4) |

`/api/v1/auth/refresh`, `/api/v1/auth/logout` and the Google OAuth routes are unchanged.

## 2. Sign-in and sign-up (one flow)

1. **Ask for a code.** `POST /api/v1/auth/otp/request` with body `{ "email": "ada@example.com" }`.
   - Always `200 { "message": "…" }`: the same answer for every address (new, existing or
     deactivated). Never infer anything from it.
   - The email holds a 6-digit code and a one-click link, both valid **10 minutes**. A new request
     replaces the previous code and link.
   - Limit: 5 codes per address per 15 minutes. Past it the answer is the same and no email is sent.
     Suggest a "resend" button with a cooldown of about 60 seconds.
2. **Verify.** Either way works:
   - Code: `POST /api/v1/auth/otp/verify` with body `{ "email", "code": "048213" }`. The code is
     exactly 6 ASCII digits and may start with `0`, so keep it a string.
   - Link: the email links to `{APP_URL}/auth/magic?email=<urlencoded>&token=<64 hex>`. **The web
     app must add this page.** It must not sign in on GET: it POSTs to
     `POST /api/v1/auth/otp/verify-link` with body `{ "email", "token" }`. This keeps email
     scanners that open links from using up the token.
3. **Response `200`.** `{ access_token, refresh_token, user: { id, username, role, status }, is_new_account }`.
   - `is_new_account: true` means this sign-in created the account. Its username is random
     (`user_xxxxxxxxxx`), so prompt the user to pick one with `PATCH /api/v2/me/profile`.
   - Store the tokens exactly as for the former login: the web app's `POST /api/session` with
     `{ refresh_token }`; mobile secure storage.
4. **Errors.**
   - `400 "Invalid or expired code"`: a wrong, expired, used or replaced code. It looks the same
     whether or not an account exists.
   - `429`: 5 checks within 15 minutes. The pending code is destroyed and the user requests a new
     one after the window. Requesting a new code does not reset the limit.
   - `403 "Account has been deactivated"`: only after a correct code.
   - `400 VALIDATION_ERROR` with `error.details[]`: fields `email` and `code`.

A pending (unverified) legacy account becomes active and verified on its first correct code.
Google sign-in still works, and a Google account may also sign in by code.

## 3. Security emails the user may receive

- **The code email** says when (UTC) and from which browser (User-Agent) the code was requested.
- **"New sign-in to your CF Meet account"** is sent when a device not seen on the account in 90
  days signs in. It links to `{APP_URL}/settings/sessions`, so **that page should exist**; it lists
  `GET /api/v2/me/sessions` and offers `POST /api/v2/me/sessions/revoke-all`.

## 4. Step-up confirmation (sensitive actions)

Before deactivating the account or changing its email, the signed-in user confirms with a code
sent to the account's **current** address:

1. `POST /api/v2/me/step-up/request` with body `{ "purpose": "DEACTIVATE_ACCOUNT" | "CHANGE_EMAIL" }`
   returns `200 { message, expiresInSecs: 600 }`.
2. `POST /api/v2/me/step-up/verify` with body `{ purpose, code }` returns
   `200 { stepUpToken, expiresInSecs: 300 }`. The token is single use and bound to this session
   and purpose.
3. Use the token:
   - **Deactivate:** `POST /api/v1/users/me/deactivate` with body `{ "step_up_token": "…" }`.
   - **Change email:** see §5.

A missing, used or foreign token gets `403 REAUTHENTICATION_REQUIRED` (the existing code). A token
from before account sessions existed cannot step up: the user signs in again.

## 5. Change email (new screens)

1. Step-up with purpose `CHANGE_EMAIL` (§4).
2. `POST /api/v2/me/email/change/request` with body `{ newEmail, stepUpToken }` returns
   `200 { message, expiresInSecs: 600 }`.
   - A code goes to the new address. If that address already has an account it gets a notice
     instead, and the answer is the same.
   - `429`: one change per 7 days.
3. `POST /api/v2/me/email/change/verify` with body `{ newEmail, code }` returns
   `200 { email, otherSessionsEnded: true }`. Every other session is signed out; this one stays.
4. The **previous** address receives an undo link `{APP_URL}/auth/email-change/undo?token=<64 hex>`,
   valid 7 days. **Add this page.** It POSTs `POST /api/v1/auth/email-change/revert` with body
   `{ "token" }`:
   - `200`: the old address is restored and every session ends; show "sign in with your previous
     address".
   - `400`: invalid, used or expired.
   - `409`: the old address was taken in the meantime; contact support.

Users who lost their old mailbox are helped by support: `PATCH /api/v1/admin/users/{id}/email`
(administrators only).

## 6. CI and test dependencies handed over

These frontend-owned test scripts still sign in with a password. The backend side is ready.

| Script | Run by | What the backend now provides | Frontend change needed |
|---|---|---|---|
| `apps/web/tests/messaging-e2e.browser.mjs` (`signIn`) | `unified-messaging-e2e.yml` (web journey, a required release gate) | `E2E_USERS` entries are `{id, email, code, name}`; the three users are fixed-code test accounts (`AUTH_TEST_ACCOUNTS`) | POST `{email, code}` to `${API}/auth/otp/verify` instead of `{email, password}` to `${API}/auth/login` (about 4 lines) |
| `apps/web/tests/messaging-production.browser.mjs` (`signIn` through the login page) | `cfmeet-calling-production.yml` (production web journey) | `PROD_ACCOUNTS` entries are `{email, refreshToken, cfMeetId, labels}`: one fresh session per account, because the browser cannot read a code | Either store the session the way `messaging-e2e.browser.mjs` does (`POST /api/session` with `{refresh_token}`), or drive the new code screen with a code supplied by the backend team |
| `apps/mobile/tests/logic.test.ts`, `apps/mobile/lib/api/routes.ts` and the other mobile auth files | mobile CI | — | Move to the endpoints above |

Web files that still use removed endpoints:
- `src/services/auth.service.ts`, `src/services/account.service.ts`, `src/components/auth/LoginModal.tsx`;
- `src/app/(auth)/forgot-password/**`;
- `src/app/api/session/**` (its refresh is unchanged);
- the regenerated `src/types/generated/meeting-service.ts`.

Mobile files: `app/(auth)/*`, `app/(settings)/change-password.tsx`, `security.tsx`,
`features/auth/components/*`, `lib/api/routes.ts`, `lib/api/errors.ts`, `lib/auth/account-status.ts`.

## 7. Test accounts (store review, automated tests)

The backend accepts fixed codes for listed addresses: `AUTH_TEST_ACCOUNTS=email:123456,...`.
- No email is sent to these addresses, and they can never be administrators.
- The backend refuses to start with this setting outside a development `APP_ENV` unless
  `AUTH_TEST_ACCOUNTS_ALLOW_IN_PRODUCTION=true`.
- Ask the backend team for a production App Store / Play review account when one is needed.
