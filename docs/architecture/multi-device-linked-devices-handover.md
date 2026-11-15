# Linked devices and multi-device personal E2EE — web and mobile integration handover

The backend is live behind the Meeting Service at `https://api.cfmeet.com`. Decision: [ADR 0060](../decisions/0060-account-linked-devices-and-production-personal-e2ee.md). Gaps and evidence: [gap register](multi-device-gap-register.md). The OpenAPI contract is `packages/contracts/openapi/meeting-service.json` (tag **Account devices (v2)**). The generated types are `apps/web/src/types/generated/meeting-service.ts` and `apps/mobile/lib/api/generated/meeting-service.ts`.

Nothing here requires a phone number, SMS or a phone scanning a QR code. Meetings and Calling never require device declaration or E2EE.

## 0. What the web client already does

The shipped web client (`apps/web`) implements the browser side:

| Piece | Where |
|---|---|
| Per-account, non-extractable Ed25519 device key and installation id in IndexedDB (`cfmeet-account-device`) | `src/lib/auth/account-device-core.ts`, `src/lib/auth/account-device.ts` |
| Device declaration before E2EE enrollment | `MessagingEngine.registerDevice` → `ensureTrustedAccountDevice` |
| Link request with the 6-digit code shown in the existing setup step | `DevicePanel`, `data-testid=account-link-code` |
| Approval banner on trusted browsers | `IncomingLinks` |
| `REFRESH` proofs, sent through `/api/session/refresh` | `session-core.ts` `refreshProof`; `deviceProofOf` in the route |

Not yet in the web UI:

* the email step-up screen;
* the device list and revoke screen.

Mobile (`apps/mobile`) still has to adopt the whole of §3.

## 1. Concepts

| Term | Meaning |
|---|---|
| Device | One browser profile (all its tabs) or one mobile app installation. Identified by a random `installationId` you generate once and an Ed25519 key pair generated on the device. |
| Sign-in | One `account_sessions` row (`sid` in the JWT). It belongs to at most one device. |
| Primary mobile | The account's single `ACTIVE` mobile. A new phone replaces it. |
| Trusted device | Bootstrap (the first device of the account), approved by a trusted device, or confirmed by an email code. Only trusted devices can approve other devices or enroll personal E2EE. |
| Unclassified sign-in | A sign-in that never declared a device (older clients). It works as before, but cannot enroll E2EE. |

### Keys and storage

| Platform | Key | Where to keep it |
|---|---|---|
| Web | Web Crypto Ed25519, `extractable: false` | IndexedDB, next to the installation id. One per browser profile; never per tab. |
| Mobile | Ed25519 from the platform keystore (Secure Enclave or StrongBox where available, else Keychain or Keystore-wrapped) | The keystore; the installation id in secure storage. |

The private key never leaves the device. If local storage is cleared, the browser is a new device: generate a new installation id and key.

## 2. Proof of possession

Sign the UTF-8 statement with the device key. The base64 below is base64url without padding.

```
cfmeet-account-device-proof:v1
{PURPOSE}
{keyThumbprint}        lowercase hex SHA-256 of the 32-byte public key
{context}
{issuedAt}             Unix seconds (accepted within ±300 s)
{nonce}                base64url of 16-64 random bytes; NEVER reuse
```

Send it as `{"issuedAt": <int>, "nonce": "<b64url>", "signature": "<b64url 64 bytes>"}`.

| Purpose | Signed by | Context |
|---|---|---|
| `DECLARE` | the declaring device | its `sessionId` (JWT `sid`) |
| `REFRESH` | the device of the sign-in | its `sessionId` |
| `LINK_REQUEST` | the new device | its `deviceId` |
| `LINK_APPROVE` | the approving (trusted) device | `{linkId}\|{keyThumbprint of the new device}` |
| `LINK_COMPLETE` | the new device | `{linkId}` |
| `REVOKE` (another device) | the calling trusted device | the target `deviceId` |

## 3. Flows

### 3.1 Every sign-in (password or Google)

1. Sign in as today: `POST /api/v1/auth/login`, or the Google flow and `oauth/google/exchange`.
2. Immediately, within 10 minutes of signing in, call `POST /api/v2/me/devices/current`:
   ```json
   {"installationId": "...", "deviceClass": "WEB|MOBILE", "platform": "WEB|IOS|ANDROID",
    "label": "Chrome on Mac", "publicKey": "<b64url 32 bytes>", "proof": {DECLARE proof}}
   ```
   The response is `{device, replacedDeviceId, endedSessions}`. Check `device.state` and `device.trust`:

   | Result | Meaning and next step |
   |---|---|
   | `ACTIVE` + `TRUSTED` | Done. A first mobile is now primary. |
   | `ACTIVE` + `UNTRUSTED` (web) | Usable for Meetings and Calling. Offer "Link this browser" (§3.2) before Messaging. |
   | `REPLACEMENT_PENDING` (mobile) | Every other endpoint answers `403 DEVICE_REPLACEMENT_PENDING` until §3.2 completes. |

   Signing in again on the same installation returns the same `deviceId` and ends that device's older sign-in (`SUPERSEDED`).
3. From then on, refresh with `POST /api/v1/auth/refresh {"refresh_token", "deviceProof": {REFRESH proof}}`.

**Legacy (undeclared) sign-ins: compatibility and migration.** A client that never declares keeps working for Meetings, Calling and MANAGED messaging, but it has no device authority (no linking, approval, device revocation or E2EE enrollment). It also ends whenever the account's primary mobile is replaced or any device is revoked, because an undeclared sign-in cannot be attributed to a device. Declared web devices stay connected. When every client declares at sign-in, the owner sets `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM`. From that date, an undeclared sign-in past its 10-minute declaration window is refused with `403 DEVICE_DECLARATION_REQUIRED`, on requests and on refresh. The client must sign in again and declare immediately. Clients should therefore declare at every sign-in, not only before Messaging.

### 3.2 Linking a device (new browser, new phone)

On the **new device**:

1. `POST /api/v2/me/device-links {proof: LINK_REQUEST}` returns `201 {linkId, comparisonCode, qrPayload, expiresAt}`.
2. Show the 6-digit `comparisonCode`, and optionally the QR code of `qrPayload`.
3. Poll `GET /api/v2/me/device-links/{linkId}` until `status` is `APPROVED`.
4. **No trusted device at hand?** Call `POST /api/v2/me/device-links/{linkId}/email-code` (at most once a minute) and ask for the emailed code.
5. Finish with `POST /api/v2/me/device-links/{linkId}/complete {proof: LINK_COMPLETE, emailCode?}`.

   For a pending mobile, the response includes `replacedDeviceId`. The old phone is signed out everywhere; trusted browsers stay signed in.

On a **trusted device** (web or primary mobile):

1. `GET /api/v2/me/device-links` lists pending requests: platform, label, `keyThumbprint`. It never shows a code.
2. The user types or scans the code shown on the new device.
3. `POST /api/v2/me/device-links/{linkId}/approve {comparisonCode, proof: LINK_APPROVE}`. Five wrong codes reject the request.
4. To refuse instead: `POST .../reject`.

A copied QR code or code is useless on its own: approving needs a trusted device's sign-in and key, and completing needs the new device's key.

### 3.3 Device management screen

* `GET /api/v2/me/device-policy` returns:
  * `accountState`: `WEB_ONLY`, `MOBILE_PRIMARY` or `DEVICE_REPLACEMENT_PENDING`;
  * the limits `maxWebDevices` and `maxPrimaryMobiles`;
  * `activeWebDevices`;
  * `linkingMethods`;
  * `currentDeviceStatus`.
* `GET /api/v2/me/devices?includeRevoked=true` lists devices (`primary`, `trust`, `openSessions`, `revokeReason`, `replacedByDeviceId`). Sign-ins are listed at `GET /api/v2/me/sessions` (`deviceId`).
* `POST /api/v2/me/devices/{id}/revoke`:
  * revoking itself: `{}`;
  * revoking another device: `{proof: REVOKE}` from a trusted device.

  Revoking the primary mobile leaves the account `WEB_ONLY`.
* `GET /api/v2/me/security-events` is the account's security log. Poll it on start and show an alert for `PRIMARY_MOBILE_REPLACED` or `DEVICE_REVOKED` that this device did not cause. An email notice is also sent.
* `POST /api/v2/me/sessions/revoke-all` signs out everywhere and revokes every device.

### 3.4 Personal E2EE (Messaging) on each device

Each trusted device has its **own** MLS device. Never copy keys between devices.

1. Enroll as today: `POST /api/v2/messaging/devices/registration-challenges` → `POST /api/v2/messaging/devices` → `.../key-packages`. An untrusted or unclassified sign-in gets `403 DEVICE_NOT_TRUSTED`.
2. **Link the Messaging device in the Hub** (required): `POST /api/v2/communications/devices/link {"platform", "chatDeviceId"}` (and `callEndpointId` for the Calling endpoint). This binds them to the sign-in, so a replacement or revocation ends them.
3. Your own new devices: a member device adds them like any member device. The web engine's existing "smallest in-tree device adds, others after a grace period" rule applies.
4. On `MLS_REKEY_REQUIRED` (a device was revoked or replaced): read `GET .../mls/state` `pendingRemovals`, then commit the removal (`removedDeviceIds`) from a surviving device before sending. The web engine's `commitPendingRemovals` does this.
5. A new device sees future messages only. For older history, use the existing device-to-device history transfer from a surviving device. Without one, history stays inaccessible by design.
6. Use a fresh `clientMessageId` per message per device. A retried send with the same id and bytes returns the original message.

### 3.5 Automatic onboarding (no activation step)

Users never "turn on" private messaging. The web client does all of this on its own; mobile clients should do the same.

1. **At every sign-in** (password, verify-then-sign-in, Google): declare the device right away (§3.1), reusing the key and installation this browser or phone already has. On failure or timeout, sign-in continues and the sign-in simply stays unclassified.
2. **When Messaging starts** without a local MLS device, enroll automatically:
   * trusted: run §3.4;
   * untrusted: start a link request and show only the comparison code, with "Email me a code" as the alternative when no trusted device is at hand;
   * `DEVICE_DECLARATION_REFUSED` (the sign-in predates the device flow): end the sign-in and send the user through the normal login page (`/login?reason=secure_messaging&next=…`), then enroll.

   Transient failures are retried with backoff. Expired or rejected links restart on their own.
3. **Chat with a contact who has no device yet:** the conversation is created `PRIVATE_E2EE` anyway (Messaging 2026-11-15: only the creator needs a device). The MLS group starts with the creator's devices. Messages written meanwhile are **held sealed on the sending device** (the worker's at-rest key; never sent, never shown as sent: state `pending-setup`, "Waiting for secure setup"). When the contact enrolls, a member device adds their devices (§3.4.3), and the held messages are encrypted for them and sent, oldest first. Nothing is ever downgraded to MANAGED, and no key is ever generated on the server.
4. **Message states shown to the sender:**
   * `pending-setup`: held, not sent;
   * `sending`;
   * `sent`: encrypted for the recipients' devices and stored by the Delivery Service, which also covers an offline recipient who fetches it later;
   * seen: read cursor;
   * `failed`: not sent, with retry.
5. **Realtime:** the WebSocket origin is `NEXT_PUBLIC_MESSAGING_URL`, or else the API origin (production serves `/v1/realtime` there). Device and session tokens are renewed on the open socket before expiry (`auth.refresh`). Reconnection uses backoff and catches up from the per-device cursor.

## 4. Errors

| HTTP | Code | When | Client action |
|---|---|---|---|
| 401 | `DEVICE_PROOF_INVALID` | Bad signature, stale `issuedAt`, reused nonce, wrong purpose or context, or refresh of a device-bound sign-in without a proof | Re-sign with a new nonce. If the key is gone, the device must be re-created and re-linked. |
| 403 | `DEVICE_REPLACEMENT_PENDING` | A pending mobile called a normal endpoint | Go to the linking screen (§3.2) |
| 403 | `DEVICE_NOT_TRUSTED` | Approve, revoke another device or enroll E2EE from an untrusted device; or complete before approval | Link this device first |
| 409 | `WEB_DEVICE_LIMIT_REACHED` | Declaring a new browser beyond `maxWebDevices` | Ask the user to revoke a browser from a trusted device |
| 403 | `DEVICE_DECLARATION_REQUIRED` | Device declaration is enforced (`ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM`) and this sign-in never declared its device within 10 minutes of signing in | Sign in again and declare immediately (§3.1) |
| 409 | `DEVICE_DECLARATION_REFUSED` | The sign-in already has another device, is older than 10 minutes, or the device is already trusted (link request) | Sign in again, then declare |
| 409 | `DEVICE_KEY_IN_USE` | The key or installation is already bound to another live device | Generate a new key and installation id |
| 404 | `DEVICE_NOT_FOUND` | Unknown or foreign device, or an undeclared sign-in using device endpoints | Declare first |
| 410 | `DEVICE_LINK_INVALID` | Unknown, foreign, expired, used, rejected or cancelled link | Start a new link |
| 429 | `RATE_LIMITED` | More than 10 link requests per hour, or an email code within 60 s | Wait for `Retry-After` |
| 401 | `AUTHENTICATION_REQUIRED` / revoked | The device or sign-in was revoked (for example the phone was replaced) | Erase local keys and history for the account (§5), then sign in again |

## 5. State transitions

```
(sign-in) --declare--> WEB: ACTIVE/TRUSTED (bootstrap) | ACTIVE/UNTRUSTED
                      MOBILE: ACTIVE/TRUSTED primary (bootstrap) | REPLACEMENT_PENDING
UNTRUSTED web --link complete--> TRUSTED
REPLACEMENT_PENDING --link complete--> ACTIVE/TRUSTED primary  (old primary -> REVOKED DEVICE_REPLACED)
any --revoke / logout-all--> REVOKED (terminal; all sign-ins end; Messaging device, push, Calling endpoint end)
```

When a device learns it was revoked (any 401 on its sign-in after a replacement, `DEVICE_REVOKED` from Messaging, or a 4401 WebSocket close from Calling), it must:

* stop background work;
* erase its E2EE keys and local history for the account;
* show "This device was signed out".

## 6. Web security notes (cannot be removed by E2EE)

The web client keeps its existing controls:

* nonce CSP with `strict-dynamic`;
* Trusted Types;
* the crypto module integrity-pinned (SHA-384) and reproducibly built;
* keys in a dedicated Worker under a non-extractable wrapping key;
* one tab per device (Web Locks).

Residual risks:

* a compromised CF Meet origin or browser profile can use the device while it is signed in;
* login tokens in JavaScript-readable cookies ([web threat model](web-messaging-client-threat-model.md) R1-R4). Moving the refresh token to an HttpOnly cookie remains the recommended follow-up.

The device key of the Account authority should live in the same Worker or IndexedDB scope as the Messaging device, and it must also be non-extractable.
