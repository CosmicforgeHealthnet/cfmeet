# Server audit: integration setup and acceptance

Prepared for the 21 November 2026 audit closure. Code availability, production activation,
and witnessed real-device delivery are separate acceptance states. A green adapter health
check does not prove that an app closed on a phone can ring.

## Optional account phone numbers

Implemented: session-bound email confirmation before adding/changing/removing a number,
Twilio Verify SMS confirmation, one-use challenges, five verification attempts, three
challenges per account or destination per 15 minutes, private-by-default discovery,
Contacts visibility/block enforcement, and sign-in with the existing authenticator step.
Phone sign-in requires an existing linked account; it never creates or verifies an email
account. Removing the binding revokes all account sessions. Account download includes
the verified binding; account erasure removes bindings and challenges.

External setup:

1. Create a Twilio account and Verify service with SMS enabled. Configure permitted
   destination countries and provider fraud controls/budget limits in that account.
2. On the production VPS, place these **root-owned 0400 files** in
   `/opt/cfmeet-core/secrets/` through an approved secret delivery channel:
   - `phone-verify.json`: `{"accountSid":"AC<32 hex digits>","serviceSid":"VA<32 hex digits>"}`
   - `phone-verify-auth-token`: the Twilio account auth token, one line.
3. Release the exact green main revision through the coordinated production workflow.
   Dispatch `activate-phone-accounts` with `confirm=twilio-verify-ready` and that revision.
   The operation verifies the service belongs to the account with a read-only provider
   request, mounts the token read-only, and uses the existing activation rollback.
4. With controlled real accounts, prove: email step-up → SMS → binding → opt-in contact
   discovery → phone sign-in → authenticator when enabled → replay refusal → removal.
   Verify private/blocked numbers cannot be discovered and removed numbers cannot sign in.
5. Only then set `NEXT_PUBLIC_PHONE_ACCOUNTS_ENABLED=true` for the web build. Account
   settings use backend availability; phone sign-in and phone lookup remain hidden by
   default. The mobile sign-in UI still needs its separate passwordless migration.

No SMS is sent by deployment or the activation preflight. Provider SMS charges apply
only when someone requests a code after activation.

Provider contract: [Twilio Verify verifications](https://www.twilio.com/docs/verify/api/verification)
and [verification checks](https://www.twilio.com/docs/verify/api/verification-check).

## Native phone push

The Notification Service already has APNs/FCM adapters, sealed registrations, token removal,
and incoming-call TTL/cancellation handling. `activate-native-push` prepares these adapters
on both replicas, preserving Web Push. It stays unavailable until credentials exist.

External files, also root-owned 0400 in `/opt/cfmeet-core/secrets/`:

- `native-push.json`, for the platforms being enabled:
  ```json
  {
    "apns": {"keyId":"ABCDEFGHIJ","teamId":"KLMNOPQRST","topic":"com.yourcompany.cfmeet"},
    "fcm": {"projectId":"your-firebase-project"}
  }
  ```
- `native-push-apns.p8`: Apple Push Notification authentication key, PKCS#8 PEM. Its
  Apple developer team, key ID, app bundle ID and push entitlement must match the app.
- `native-push-fcm.json`: Firebase service-account JSON for that project with FCM HTTP v1
  sending permission. Enable the Firebase Cloud Messaging API. Put the matching Android
  client configuration in the signed app build; never embed the service-account key.

After releasing green main, dispatch `activate-native-push` with
`confirm=native-push-credentials-ready`. Only the server uses these private keys.
Adapter readiness is checked on each replica; failure restores the previous containers.
The operation deliberately reports real-device acceptance as outstanding.

Mobile integration still required before advertising phone alerts:

The prepared client coordinator is
`apps/mobile/features/notifications/native-push-registration.ts`, with the account relay
adapter in `lib/api/native-push.ts`. It serializes token replacements, drains pending
registration before removal, and guards late callbacks across logout/account changes.
Supply its native token source from the signed app, and bind it to the approved Account
Device lifecycle below; it is deliberately not enabled by the current mobile screens.

1. In the Expo SDK 57 app, install the SDK-compatible `expo-notifications` with
   `npx expo install expo-notifications`, configure its app plugin and platform client
   credentials, and produce a signed development/release build.
2. Complete the existing cryptographic Account Device declaration/linking flow for the
   mobile install. A random UUID or the current account session ID is not a device proof.
3. After user permission, use `getDevicePushTokenAsync()` for the **native APNs/FCM token**,
   not an Expo push-service token. Register with
   `PUT /api/v2/notifications/devices/{accountDeviceId}/push` using `{platform,handle}`.
   Subscribe to native token changes and replace the registration. Deregister before
   logout with `DELETE` on that path and `?platform=APNS` or `?platform=FCM`.
4. Foreground refreshes and notification taps must re-fetch authorized data; push payloads
   are hints. Allow only known in-app destinations, never an arbitrary push-supplied URL.
5. Background incoming iOS calls require a native PushKit/CallKit integration and an
   `APNS_VOIP` registration in addition to ordinary alert permission. Android needs the
   corresponding background call UI and runtime notification behavior. The Expo alert
   token alone does not implement either call experience.
6. Prove delivery on physical iOS and Android devices with the app foregrounded, backgrounded
   and closed. Check ring cancellation, expiry, token refresh, logout/revocation, duplicate
   delivery and permission denial. Keep the current truthful mobile availability screen
   until these checks pass.

[Expo SDK 57 notifications reference](https://docs.expo.dev/versions/v57.0.0/sdk/notifications/).

## Independently witnessed security codes

Do not operate a second key on the same administrative account and label it independent.
An external party must control its own signing key, host and durable monotonic state.

Give that operator the deployed log ID and pinned base64url Ed25519 public key. The
repository supplies `messaging-kt-witness`, built from
`services/messaging/src/bin/messaging-kt-witness.rs`. The operator configures:

- `KT_WITNESS_ID`: stable independent operator ID;
- `KT_WITNESS_SIGNING_KEY_FILE`: their own PKCS#8 Ed25519 key;
- `KT_WITNESS_LOGS_FILE`: `[{"logId":"<deployed-log-id>","publicKey":"<pinned-log-key>"}]`;
- `KT_WITNESS_STATE_FILE`: persistent state, protected against rollback to older backups;
- `KT_WITNESS_BIND_ADDRESS`: private listener behind their HTTPS endpoint.

They return their HTTPS `POST /witness/v1/add-checkpoint` endpoint and base64url Ed25519
public key. Configure Messaging's read-only `MESSAGING_KT_WITNESSES_FILE` as
`[{"witnessId":"<operator-id>","url":"https://<operator>/witness/v1/add-checkpoint","publicKey":"<key>"}]`
and set `MESSAGING_KT_WITNESS_QUORUM` to the agreed number of distinct operators. Preserve
the pinned log identity and the web `NEXT_PUBLIC_MESSAGING_KT_LOG_ID` / `_LOG_KEY`.

Acceptance requires actual distinct cosignatures, proof verification and client display
of `WITNESSED`, plus refusal of inconsistent/stale checkpoints and truthful degradation
when quorum is lost. No independent witness has been supplied; production remains
`UNWITNESSED` until that evidence exists.

## Other production dependencies

- The new read-only `diagnose` output checks Resend's `cfmeet.com` domain and record
  verification states. A send-only API key may not authorize this read; that is reported
  without exposing the key. Correct failed SPF/DKIM/return-path records at the DNS owner,
  then verify controlled recipient delivery. The permanent-bounce fix cannot make an
  invalid recipient or remote mailbox accept mail.
- Meeting recording, transcription and intelligence remain off until their own storage
  and provider journeys pass. Recording needs an artifact bucket, scoped service
  credentials, provider upload/finalization, playback/deletion and retention evidence.
  Transcription/intelligence also need actual provider implementations and credentials.
  Existing Calling recording storage is a separate owner and is not proof of Meeting
  artifact readiness.
