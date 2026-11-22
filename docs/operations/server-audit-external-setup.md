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

## Operator-managed and independent security-code witnesses

Owner decision, 22 November 2026: use a CF Meet-operated cosigner; no outside operator
is required for this deployment. `activate-managed-kt-witness`, with
`confirm=operator-managed`, starts the existing Rust witness program in an isolated
container, preserves the current log signing identity, pins its public key, and retains
monotonic state across restarts. The separate signing key has protected recovery material.
Messaging is configured with quorum 1 only inside a rollback-protected activation which
must observe a verified cosignature of the current head. Its witness ID is
`cfmeet.operated`. This is an **operator-managed cosignature**, not independent oversight:
the same administrator can control both the log and its cosigner. Do not restore the
witness state to an older snapshot or implicitly replace its key.

Run this after releasing the image containing `messaging-kt-witness` and before enabling
the local send-policy fast path. Re-run `activate-service-log-collector` afterwards so
its container joins the archived inventory. The activation reports
`independentOperator=false` explicitly. The older independent-operator setup below is
optional and remains the stronger trust model.


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
when quorum is lost. An operator-managed cosigner does not satisfy that independent-operator acceptance.
Report the actual cosignature state and who controls the configured witness separately.

## Other production dependencies

- On 21 November, Resend showed verified SPF/DKIM and successful real-mailbox delivery.
  Missing DMARC was added at the DNS owner and read back from an authoritative server.
  Recent delayed acceptance messages targeted `example.invalid`; the Resend adapters now
  refuse that reserved suffix before HTTP. See the coordinated release runbook for the
  evidence and remaining controlled-recipient acceptance. Read-only `diagnose` reports
  domain verification when the key permits it, without exposing the key.
- Meeting recording storage, per-request Egress S3 targeting and a real capture/playback/
  deletion acceptance operation are now prepared in the coordinated release runbook.
  They remain inactive until the approved release and that actual acceptance pass.
  Existing Calling storage is not substituted for Meeting's separate artifact bucket.
- **M8 owner decision, 21 November 2026:** leave transcripts and AI notes disabled. No
  OpenAI key was created and no AI-provider integration was activated in this pass.


## Recording callback activation

`activate-meeting-recording` with `prepare` checks both existing provider identities
against the shared SFU's configured keys. Self-hosted LiveKit has a global webhook signer;
Meeting currently verifies callbacks with its own provider credentials. The activation
therefore aligns Meeting's client credentials with that existing signer while capture
remains disabled. It never rotates or removes an SFU key,
so existing participant tokens remain valid. Unknown keys or callback layouts are refused.
It extends only the known Calling callback configuration. The media server restarts only
when both participant and active-capture counts are zero. A failed restart or route
verification restores the exact previous configuration. Preparation completes as a
separate workflow run before `enable-30-days`; re-run preparation for an older storage-only
setup. Enable refuses a remaining signer mismatch and changes only the capture flag.
A failed capture acceptance then restores the prepared container, retaining the signer
that verifies the already configured callbacks.

Use `diagnose` to read the allowlisted controlled recording receipt, its own artifact's
state/failure code, the boolean signer comparison and known public RTC host/port. No media,
storage URL, credential,
provider reference, or failure detail is exported by these diagnostics. The web recording
flag (`NEXT_PUBLIC_MEETING_RECORDING=on`) must be enabled in Production and rebuilt only
after backend recording acceptance passes. Transcripts and AI notes remain disabled.

The disposable media client uses the host network, matching the existing Calling RTC
acceptance and avoiding Docker NAT/hairpin on public WSS and ICE. It remains non-root,
read-only, resource limited, without capabilities or provider/storage administrator keys.
References: [LiveKit's server key and webhook configuration](https://github.com/livekit/livekit/blob/master/config-sample.yaml).
