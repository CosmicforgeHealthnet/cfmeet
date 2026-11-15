# 06 — Frontend notification consumption inventory (web + mobile)

Auditor: read-only. Repo: `/home/user/cfmeet` at HEAD `6f3743f`. No files modified, no network calls.
Scope: `apps/web` (Next.js) and `apps/mobile` (Expo SDK 57 / RN 0.86). Line numbers refer to HEAD.

---

## 0. Executive summary (one paragraph per platform)

**Web.** No service worker, no Web App Manifest, no `PushManager.subscribe`, no VAPID fetch, and no
call to any push-registration endpoint exist. The ONLY OS-level notification is a page-context
`new Notification(...)` for an incoming call while a CF Meet tab is open and hidden
(`IncomingCallLayer.tsx:70-72`). Everything else is in-app and driven by (a) three WebSockets
(Calling signaling, Messaging realtime, Conference realtime) and (b) extensive React Query
polling. The Communications Hub contract exposes notification preferences, delivery status,
per-conversation mute and device push registration, but the web UI consumes none of them
(it only reads `InboxEntry.notifications` to draw a "muted" icon and PATCHes per-contact
`notifications`). There is no global bell/inbox/unread badge; unread counts exist only inside
the Messages page and the Voicemail tab.

**Mobile.** No `expo-notifications`, no FCM/APNs/PushKit/CallKit/ConnectionService libraries,
no Android `POST_NOTIFICATIONS` permission, no realtime socket of any kind. iOS declares
`UIBackgroundModes: ["audio","voip"]` (`app.json:26-28`) with no VoIP push implementation.
All "notification-like" behaviour is polling while foregrounded, plus an in-app toast. The
settings screen states truthfully "Push notifications: not available yet"
(`app/(settings)/communication.tsx:23-27`). Mobile cannot place or receive calls.

---

## 1. Push registration

### 1.1 Web

| Item | Finding | Evidence |
|---|---|---|
| Service worker file | **None.** `apps/web/public/*.js` = only `messaging-crypto/crypto-worker.33ee2cd68f0ec412.js` (dedicated MLS Web Worker, not a SW), `messaging-crypto/cf-messaging-crypto.44076f63c6687d75.js` (WASM glue), `security/trusted-types-default.js`. | `ls apps/web/public` ; grep for `serviceWorker` in `apps/web/src`, `public`, `next.config.ts` → 0 hits |
| `navigator.serviceWorker.register` | None | grep → 0 |
| Web App Manifest | None (`app/manifest.ts` / `manifest.webmanifest` absent; `app/` has only `icon.png`) | `ls apps/web/src/app` |
| VAPID key fetch (`GET /v1/messaging/push/configuration`) | **Allowlisted but never called.** Mirrored in client relay allowlist only. | `apps/web/src/lib/messaging/routes.ts:48`; grep for `push/configuration` elsewhere → 0 |
| Push subscribe (`PUT /v1/messaging/devices/{id}/push-registration`) | **Allowlisted but never called.** | `routes.ts:47` (PUT + DELETE) |
| Hub push target (`PUT/DELETE /api/v2/communications/devices/{deviceId}/push`) | Not implemented in `communicationsService` | `apps/web/src/services/v2/communications.service.ts:85-144` (no `devices`/`push` method) |
| Calling endpoint push target | `RegisterEndpointRequest.pushTargetRef?` typed but never set; web registers `{principalId, endpointKey, endpointKind:"WEB", capabilities}` only | `apps/web/src/types/calling/endpoint.ts:18-24`; `apps/web/src/hooks/calling/useCallingDevice.ts:57-58` |
| `Notification.requestPermission()` prompts | Two explicit, user-gesture-gated buttons: (1) Calls › Devices page "Allow notifications"; (2) link on the ringing card "Show calls as notifications when this tab is in the background". No prompt on load. | `apps/web/src/components/calling/CallingDevicesPage.tsx:26,64`; `apps/web/src/components/calling/IncomingCallLayer.tsx:109-112` |
| UI copy confirming absence | "Calls ring only while a CF Meet tab is open; waking a closed browser (Web Push) is not available yet." / header comment "Web Push / APNs / FCM delivery is EXTERNAL_BLOCKED (CAL-003)" | `CallingDevicesPage.tsx:62`; `IncomingCallLayer.tsx:8-9` |
| CSP impact for a future SW | `worker-src 'self' blob:` (would allow a same-origin SW); `connect-src` includes `'self'`, API origin, `wss:`, calling/messaging origins. Crypto worker has its own `worker-src 'none'`. Permissions-Policy does not restrict notifications. | `apps/web/src/lib/security/csp.ts:73-91`, `112-121`, `133-135` |

**"Device" registrations the web DOES perform (precursors a Notification Service would key on):**

| Registration | Endpoint (method path) | Code | Lifecycle |
|---|---|---|---|
| Calling endpoint (per browser, per principal) | `POST /api/calling/v1/calling/endpoints` → adapter → Calling `POST /v1/calling/endpoints` | `services/calling/endpoints.service.ts:7-8`; auto-registered in `useCallingDevice.ts:45-68, 86-91` | Stable `endpointKey` in `localStorage["cfmeet.calling.endpoint.<principalId>"]` (`useCallingDevice.ts:28,51-56`). Opt-out flag `localStorage["cfmeet.calling.receive.<principalId>"]="off"` (`:27,30-32,73`). Turning off → `DELETE /v1/calling/endpoints/{id}` (`:71-84`). WS close 4401 (revoked) → drop endpoint and re-register (`:134-140`). |
| Calling signaling credential | `POST /api/calling/v1/calling/signaling-credentials` | `endpoints.service.ts:11-12`; `useCallingDevice.ts:110` | Renewed before expiry via `renewalDelay` (`useCallingDevice.ts:112-113,145`) |
| Messaging E2EE device (MLS) | `POST /api/v2/messaging/devices/registration-challenges`, `POST /api/v2/messaging/devices`, key packages | `services/v2/messaging.service.ts:16-20` | Revoked at logout (`lib/messaging/signout.ts`, `engine.ts:607-621`) |
| Messaging client token (E2EE device realtime) | `POST /api/v2/messaging/client-tokens` `{deviceId}` | `services/v2/messaging.service.ts:23`; `services/messaging-session.ts:31-46` | in-memory only |
| Messaging MANAGED "session device" token | `POST /api/v2/messaging/session-tokens` `{sessionDeviceId?}` | `services/v2/messaging.service.ts:25-26`; `engine.ts:707-733`; validated by `messaging-session.ts:118-131` | session-device id in `sessionStorage["cfmeet.messaging.session-device.<principal>"]` (`engine.ts:189,711-729`) |
| Account device (browser identity / linking) | `POST /api/v2/me/devices/current`, `/me/device-links*` | `lib/auth/account-device.ts:54-61` | — |

**Token refresh / unregister on logout or revocation (web):**

| Event | What happens | Evidence |
|---|---|---|
| Logout (`authService.logout`) | `signOutMessaging()` (clears outbox, removes session-device ids, revokes MLS device via `POST /api/v2/messaging/devices/{id}/revoke`, wipes IndexedDB keys; bounded 5 s) then `POST /api/v1/auth/logout`, then `forgetLocalSession()` | `services/auth.service.ts:97-105`; `lib/messaging/signout.ts:12-45` |
| Logout everywhere | same + `POST /api/v2/me/sessions/revoke-all` | `auth.service.ts:111-117` |
| Calling endpoint at logout | **NOT revoked.** `forgetLocalSession` clears workspace keys only; the `CallingRoot` unmounts (WS closes) but the server-side endpoint stays `ACTIVE`; `endpointKey` stays in localStorage and is re-used on next login. | `auth.service.ts:134-142`; `context/CallingContext.tsx:74-79` |
| Messaging device revoked elsewhere | poll detects `deviceIsActive=false` → wipe + "revoked" phase; WS `auth-required` → re-check | `engine.ts:381-390, 464-466, 758-763` |
| Messaging realtime token renewal | `auth.refresh` frame with a fresh single-use ticket 60 s before expiry | `services/messaging-realtime.ts:142-156`; `engine.ts:739-751` |

### 1.2 Mobile

| Item | Finding | Evidence |
|---|---|---|
| `expo-notifications` / `getExpoPushToken` / `getDevicePushToken` | **Not a dependency; not imported.** | `apps/mobile/package.json:5-41`; grep → 0 |
| FCM / APNs / PushKit / CallKit (`react-native-callkeep`, `react-native-voip-push-notification`) / ConnectionService | None | `package.json`; grep `voip|callkit` in source → 0 |
| iOS background modes | `UIBackgroundModes: ["audio","voip"]` declared with **no** VoIP push / CallKit implementation | `apps/mobile/app.json:26-29` |
| Android permissions | CAMERA, RECORD_AUDIO, MODIFY_AUDIO_SETTINGS, BLUETOOTH_CONNECT, ACCESS_NETWORK_STATE, INTERNET — **no `POST_NOTIFICATIONS`**, no `USE_FULL_SCREEN_INTENT`, no `FOREGROUND_SERVICE_PHONE_CALL`/`MANAGE_OWN_CALLS` | `app.json:41-48` |
| Config plugins | `expo-router`, `expo-status-bar`, `expo-splash-screen`, LiveKit, WebRTC — no notifications plugin | `app.json:53-71` |
| Permission prompts for notifications | None | grep → 0 |
| Token registration endpoints | None; the mobile route table (sole allowed HTTP surface, contract-checked) contains no push/notification/device route | `apps/mobile/lib/api/routes.ts:32-166` |
| Mobile relay allowlist | 9 Messaging relay ops; no `push-registration`, no `push/configuration` | `apps/mobile/lib/messaging/relay-paths.ts:64-74` |
| Unregister on logout | Nothing to unregister. Logout = `POST /api/v1/auth/logout` (errors ignored) → `clearAllStorage()` → `useLiveSession.clear()` → `queryClient.clear()` | `apps/mobile/features/auth/hooks/use-logout.ts:9-27` |
| User-facing status | "Push notifications: not available yet — The app does not receive push notifications yet. Chats, meetings and waiting-room requests update while the app is open." | `apps/mobile/app/(settings)/communication.tsx:23-27`; linked from Settings tab `app/(tabs)/settings.tsx:203-206` |

---

## 2. Push handling (payloads, E2EE previews, click routing)

### 2.1 Web
- **No push payload handler exists** (no SW `push` / `notificationclick` listeners).
- The only OS notification: `IncomingCallCard` effect (`IncomingCallLayer.tsx:66-75`):
  - Conditions: `Notification.permission === "granted"` AND `document.visibilityState === "hidden"` at the moment the effect runs (deps: `headline,label,invitationId`).
  - Content: title `Incoming voice|video call` / `Transferred … call` (`lib/calling/incoming-call.ts:52-55`), body `"<label> is calling"`, `tag: cfmeet-call-<invitationId>` (dedupes across tabs at OS level), `requireInteraction: true`.
  - Caller label derived client-side: Call view `participants[role=INITIATOR].displayName` via adapter `GET /v1/calls/{id}` else `POST /api/v2/messaging/principals/names`, else "CF Meet user" (`incoming-call.ts:1-50`; `IncomingCallLayer.tsx:46-49`).
  - Click: `window.focus(); notification.close()` — **no URL navigation → no deep-link / open-redirect surface** (`IncomingCallLayer.tsx:72`).
  - Also sets `document.title = "📞 <headline> — <label>"` (`:67-68`) and plays a Web Audio ringtone (`lib/calling/ringtone.ts:1-40`), with UI hint when autoplay is blocked (`IncomingCallLayer.tsx:103`).
- **E2EE previews:** no push previews exist. In-app conversation list preview for `PRIVATE_E2EE` uses locally-decrypted text only when messages are already loaded in this tab, otherwise the literal `"Encrypted message"`; server `previewText` is ignored for E2EE (`components/messaging/MessagesContent.tsx:1079-1085`; `lib/messaging/engine.ts:886, 1525-1536`). E2EE realtime events never carry content to the UI — they trigger an authoritative MLS sync (`engine.ts:777-783`). Contract confirms push payloads are metadata only (`PushConfiguration.payload = "METADATA_ONLY"`; Hub `payloadPolicy: "ROUTING_METADATA_ONLY"`).
- **Deep-link handling relevant to future notification clicks:**
  - Login `?next=` is validated by `safeReturnPath` (≤200 chars, starts with `/`, not `//`, regex allowlist `^/organizations/invitations$` or `^/dashboard(/[A-Za-z0-9_\-/]*)?$`) — no open redirect (`lib/organizations/organization-core.ts:323-328`; `components/auth/LoginModal.tsx:22`).
  - Org invitation token in URL fragment, scrubbed with `history.replaceState` and stashed in sessionStorage with TTL (`components/organizations/OrganizationInvitationDecision.tsx:11-23,54-55`).
  - Connect-invite token parser accepts only `/connect/<16-256 base64url>` with no query/hash (`lib/communications/comms-core.ts:58-69`).

### 2.2 Mobile
- No push handler, no notification response listener, no `Linking` URL listener beyond Expo Router's built-in scheme routing (`scheme: "cfmeet"`, `app.json:8`). No universal links / Android App Links (`associatedDomains`/`intentFilters` absent in `app.json`).
- Deep links that a future push could target:
  - `cfmeet://join?code=<code>[&invitation=<token>]` → auto-opens on mount (`app/join.tsx:1-27`); code parsed by `parseMeetingIdentifier` (any `scheme://host/.../<id>` → last segment) then resolved server-side `GET /api/v2/resolve/{identifier}`; navigation is always internal (`router.push("/(room)/[id]")`) — **no open redirect** (`features/meetings/open-meeting.ts:19-28,46-70`; `lib/invitations.ts:33-40`).
  - `cfmeet://invitation?token=<token>` → org invitation screen; token is masked (`secureTextEntry`) and only sent in POST body (`app/(settings)/invitation.tsx:1-6,40`).
  - **Inconsistency:** `lib/invitations.ts:6` documents `cfmeet://organizations/invitations?token=` but no such Expo Router route exists (route is `/(settings)/invitation`).
  - Custom schemes are hijackable on Android/iOS (any app can claim `cfmeet://`), so invitation tokens in deep links are exposed to a malicious co-installed app (low).

---

## 3. In-app notifications, unread counts, feeds, realtime

### 3.1 Web — notification-like UI surfaces

| Surface | Component / function | Data source (method path) | Refresh mechanism |
|---|---|---|---|
| Global incoming-call card (every signed-in page) | `IncomingCallLayer` mounted by `CallingSession` in root `CallingRoot` | `GET /api/calling-signaling/v1/calling/invitations` (signaling bearer) + `GET /api/calling/v1/calls/{id}` | Calling WS + HTTP recovery every 10 s; `useCall` poll 3 s | `context/CallingContext.tsx:225-231`; `app/layout.tsx:42`; `hooks/calling/useCallingDevice.ts:101-106,154`; `hooks/calling/useCalls.ts:34-36` |
| Active call dock | `ActiveCallDock` | Calling adapter | polling 3 s | `CallingContext.tsx:229` |
| Messages list unread badge (count, 99+) + mention "@" badge + "Unread N" filter chip | `MessagesContent` | Messaging `GET /v1/conversations` via relay (`c.inbox.unreadCount/mentionCount`), `GET …/{id}/unread` | Messaging WS (device or session device) + poll `POLL_MS` (default 15 s) | `MessagesContent.tsx:1021-1027,1088-1122`; `engine.ts:194,882-883,1444` |
| Muted-conversation icon (BellOff) | `MessagesContent` | Hub `GET /api/v2/communications/inbox` (`InboxEntry.notifications === "MUTED"`) | `refetchInterval: 60_000` | `MessagesContent.tsx:1014-1015,1109`; `hooks/v2/useCommunications.ts:93-95` |
| Read-cursor sync across sessions | engine `onReadCursor` clears unread | WS frame `read-cursor.synced` | WS | `services/messaging-realtime.ts:258-261`; `engine.ts:662-665` |
| Typing indicators | engine `onTyping` | WS frame `typing` | WS | `messaging-realtime.ts:252-257` |
| Contact requests count pill | `ContactsContent` tabs | Hub `GET /contact-requests/incoming` | Hub change feed poll 20 s | `components/communications/ContactsContent.tsx:554-565,609-613`; `useCommunications.ts:132-170` |
| Voicemail unread count + per-item dot + "Mark as heard/new" | `VoicemailInbox` | adapter `GET /v1/enterprise/voicemail/messages`, `PUT …/{id}/read-state` | poll 30 s | `components/calling/VoicemailInbox.tsx:68,88,102-127`; `hooks/calling/useVoicemail.ts:20-31`; `services/calling/voicemail.service.ts:19-41` |
| Missed calls tab | `CallsContent` (All / Missed / Voicemail) | adapter `GET /v1/calls?missed=true` | poll 10 s | `components/calling/CallsContent.tsx:93-94,118-130,192,221`; `hooks/calling/useCalls.ts:24-32`; `services/calling/calls.service.ts:41-44` |
| In-room join/leave toasts (3 s) | `live-room.tsx` RoomContent | LiveKit participant events | LiveKit | `components/room/live-room.tsx:57,241-292,345-356` |
| In-room notices (poll started, breakout opened/closing, host announcement; 9–15 s) | `RoomNotices` / `emitNotice` via window `CustomEvent("cfmeet:notice")` | Conference realtime events | Conference WS | `components/room/RoomLiveEvents.tsx:18-34`; `components/room/RealtimeRoomSync.tsx:18-46` |
| Messaging device-link approval / history-transfer prompts | `MessagesContent` | relay `messaging/history-transfers`, `/me/device-links` | link poll 2 s, engine poll 15 s | `MessagesContent.tsx:170-210`; `engine.ts:192,2047-2091` |
| Developer-portal toasts | `notify()` in developer-portal `ui.tsx` | n/a (local) | — | `components/developer-portal/ui.tsx:174` |
| Org invitation email delivery result | `OrganizationInvitations` shows `emailDelivery` ACCEPTED / FAILED / DISABLED | `POST /api/v2/organizations/{id}/invitations` (`sendEmail`) | — | `components/organizations/OrganizationInvitations.tsx:72-124` |

**No global bell / notification center / inbox exists.** `TopBar` contains only logo + search
(`components/layout/TopBar.tsx:3-23`); `AppSidebar`/`MobileTabs` render no counts. No toast library
(no `sonner`) at app level. No `navigator.setAppBadge`.

**Backend feeds that exist but are not rendered (web):**
- `useHubActivity()` → `GET /api/v2/communications/activity` (refetch 60 s) — **defined, zero consumers** (`hooks/v2/useCommunications.ts:74-76`; grep usage → none).
- `useHubCallHistory()` → `GET /api/v2/communications/calls/history?missed=` — **defined, zero consumers** (`useCommunications.ts:77-79`). The Calls UI uses Calling's `/v1/calls` instead.
- Hub `GET /api/v2/communications/events` (filtered feed) — not in client.

### 3.2 Web — realtime connections

| # | Connection | URL / auth | Service | Frames consumed | Evidence |
|---|---|---|---|---|---|
| R1 | Calling signaling WS | `wss://<calling origin>/v1/calling/signaling`, subprotocols `["cfmeet.signaling.v1", "bearer.<cfsig_ token>"]`; wss enforced in production | Calling service (direct from browser, not via Next adapter) | Content ignored except `signaling.pong`; any other frame → HTTP recovery + invalidate `["calling"]` queries; 25 s ping; close 4401 = revoked | `services/calling/signaling.service.ts:21-25`; `lib/calling/origin.mjs:15-21`; `hooks/calling/useCallingDevice.ts:108-152` |
| R2 | Messaging realtime WS | ticket `POST <messaging origin>/v1/realtime/tickets` (`Authorization: CF-Client-Token …`), then `wss://<origin>/v1/realtime` subprotocols `["cfmeet.messaging.v1","cfmeet.ticket.<64hex>"]` | Messaging gateway (origin = `NEXT_PUBLIC_MESSAGING_URL` or API origin) | `event` (acked serially), `subscribed`, `sync.required`, `typing`, `read-cursor.synced`, `subscription.revoked`, `server.draining`; sends `subscribe/unsubscribe/ack/typing/auth.refresh` | `services/messaging-realtime.ts:7-10,170-294`; `lib/messaging/urls.ts:18-28`; `engine.ts:204,625-700` |
| R3 | Conference realtime WS | `POST /api/v2/conferences/{id}/realtime/ticket` → `new WebSocket(<websocketPath>?ticket=…&cursor=…)`; replay `GET /api/v2/conferences/{id}/events?after=` | Meeting service | `event`, `resync_required`, `revoked`, `closed` | `services/v2/realtime.service.ts:17-49`; `hooks/v2/useRealtime.ts:153-224`; used by `components/room/RealtimeRoomSync.tsx:75` |
| — | SSE / EventSource | **none** | — | — | grep `EventSource` → 0 |

Hub has no realtime channel; web polls the change feed (`GET /bootstrap` then `GET /sync?after=` every 20 s, `useHubSync`) — `NOTIFICATION_PREFERENCE` domain maps to a `"notifications"` area that **no query uses** (`lib/communications/comms-core.ts:85,125-127`; `useCommunications.ts:127-170`). `useHubSync` is mounted only by `ContactsContent` (`ContactsContent.tsx:554`), so Hub changes are not followed on other pages.

### 3.3 Web — polling inventory (notification-relevant)

| Interval | Hook | Endpoint | File:line |
|---|---|---|---|
| 2 s | admission / waiting room | v2 participants | `hooks/v2/useParticipants.ts:52,66` |
| 3 s | call detail, call actions | adapter `GET /v1/calls/{id}`, `/actions` | `hooks/calling/useCalls.ts:35,39` |
| 5 s | conference detail | v2 | `hooks/v2/useConferences.ts:60` |
| 5 s / 15 s | org messaging messages / conversations | org relay | `components/messaging/OrganizationMessages.tsx:78,84` |
| 10 s | calling invitations recovery | signaling `GET /v1/calling/invitations` | `useCallingDevice.ts:154` |
| 10 s | call history | adapter `GET /v1/calls` | `useCalls.ts:30` |
| 15 s (env `NEXT_PUBLIC_MESSAGING_POLL_MS`) | messaging engine poll | relay conversations, welcomes, transfers, links | `engine.ts:191,451-470` |
| 20 s | Hub change feed | `/api/v2/communications/sync` | `useCommunications.ts:132` |
| 30 s | voicemail; meeting-space invitations; messaging devices | adapter / v2 | `useVoicemail.ts:28`; `hooks/v2/useInvitations.ts:29`; `hooks/v2/useMessaging.ts:24` |
| 60 s | Hub inbox, Hub timeline (10 s when `stale`) | `/api/v2/communications/inbox`, `/conversations/{id}/timeline` | `useCommunications.ts:87,94` |

### 3.4 Mobile — in-app surfaces & polling (no sockets at all)

| Surface | Code | Endpoint (via `send(ROUTES.x)`) | Interval |
|---|---|---|---|
| Toast (in-app) | `ToastProvider`/`useToast` | n/a | — | `components/ui/toast.tsx:58,102,141`; mounted `app/_layout.tsx:79-81` |
| Chats list (no unread badge, no previews) | `app/(tabs)/chats.tsx:66-88`; `useConversations` | `GET /api/v2/messaging/relay/v1/conversations?limit&cursor`, `…/members` | 15 s (`features/messaging/hooks.ts:23`) |
| Chat thread + auto mark-read | `useConversationLines` | `GET …/messages?afterSequence`, `PUT …/read-cursor` | 5 s (`hooks.ts:83,95`; `api.ts:56-57`) |
| `relayPaths.unread` | defined & allowlisted, **never called** | `GET …/unread` | — | `lib/messaging/relay-paths.ts:60,73` |
| Waiting room (hosts) | `participants-panel.tsx:24-38` | `GET /api/v2/conferences/{id}/waiting-room` | 4 s (`features/room/hooks.ts:28`) |
| Participants / chat / hands | `features/room/hooks.ts` | v2 conference routes | 5 s / 4 s / 5 s (`:18,69,88`) |
| Pre-join "meeting started?" | `use-join-flow.ts:51-62` | `GET /api/v2/conferences/{id}/pre-join` | 5 s until LIVE |
| Agenda (upcoming meetings) | `features/meetings/hooks.ts:25-38` | `GET /api/v2/me/conferences` | 60 s |

---

## 4. Notification preference / settings screens

### 4.1 Web

| Screen | Fields shown | Endpoints | Evidence |
|---|---|---|---|
| Calls › Devices (`CallingDevicesPage`) | "Receive calls in this browser" on/off (local + endpoint revoke/register); "Background notifications" (browser permission state + Allow button); registered endpoints list with Revoke | `GET/POST /v1/calling/endpoints`, `DELETE /v1/calling/endpoints/{id}` (adapter) | `components/calling/CallingDevicesPage.tsx:20-88` |
| Contacts › per-contact menu | `notifications`: "All notifications" / "Mentions only" / "Muted"; BellOff icon when muted | `PATCH /api/v2/communications/contacts/{contactId}` `{expectedVersion, notifications}` | `ContactsContent.tsx:249-253,264,273,300-303`; `communications.service.ts:117-118`; `useCommunications.ts:113-118` |
| Contacts › Privacy dialog | contactRequestPermissions, incomingCallPolicy, directChatPolicy, publicProfileVisibility, presenceVisibility, invitationPermissions | `GET/PUT /api/v2/communications/me/privacy` | `ContactsContent.tsx:469-511`; `communications.service.ts:96-97` |
| Messages | Mute **indicator only** (no mute toggle) | Hub `/inbox` read-only | `MessagesContent.tsx:1015,1109`; grep `preferences` in web → 0 |
| Messages › devices panel | Revoke messaging device (reason), "End sessions on all devices" | `POST /api/v2/messaging/devices/{id}/revoke`, relay `POST v1/messaging/client-sessions/{id}/revoke` | `components/messaging/MessagingDevicesPanel.tsx:14-61`; `routes.ts:45` |
| Dashboard › Settings | Tabs: profile, security, api-keys, webhooks — **no notifications tab** | — | `components/dashboard/SettingsContent.tsx:11-58` |
| Calls › Settings / Forwarding | call forwarding rules (to person/extension/voicemail) — routing, not notification prefs | adapter `/v1/enterprise/forwarding-rules` | `components/calling/CallingSettingsPage.tsx`, `ForwardingSettings.tsx`; `app/api/calling/[...path]/route.ts:339-356` |

**Not consumed by web (backend exists):**
`GET/PUT /api/v2/communications/me/notification-preferences` (fields: directMessages, groupMessages,
incomingCalls, missedCalls, contactRequests, groupInvitations, meetingInvitations, securityEvents=true
fixed, payloadPolicy=ROUTING_METADATA_ONLY; PUT requires all 7 booleans + `expectedVersion`);
`GET /api/v2/communications/me/notification-delivery-status` (providers APNS/FCM/WEB_PUSH
CONFIGURED|NOT_CONFIGURED, devices, pendingJobs, deadJobs, callRinging, preferences);
`GET/PUT /api/v2/communications/conversations/{id}/preferences` (notifications ALL|MENTIONS|MUTED,
mutedUntil, mediaAutoDownload ALWAYS|WIFI|NEVER, version); `GET /api/v2/communications/devices`.
Source: `packages/contracts/communications/http/v1.openapi.json` (v1.3.0) schemas
`NotificationPreferences`, `NotificationPreferencesUpdate`, `DeliveryStatus`, `DevicePush`,
`ConversationPreferences(Update)`. The frontend handover lists these as the intended UI surface
(`docs/frontend/cfmeet-frontend-handover.md:93,248-249`).

### 4.2 Mobile
- `app/(settings)/communication.tsx` ("Calls, chats & notifications") is a **static status page**: three `Notice` cards (managed chats only; calls not available; push not available). No toggles, **no endpoints called** (`:9-31`).
- No Hub endpoints at all in `ROUTES`; mobile's route test pins every route to `packages/contracts/openapi/meeting-service.json` (`lib/api/routes.ts:1-11`), which does not contain `/api/v2/communications/*` — adding Hub preferences on mobile requires extending that contract test.

---

## 5. Incoming call UI

### 5.1 Web
Flow (all in `hooks/calling/useCallingDevice.ts`, `context/CallingContext.tsx`, `components/calling/IncomingCallLayer.tsx`):
1. Signed-in → `useCallingIdentity()` (`GET /api/calling/identity`, adapter) → principalId (`CallingContext.tsx:81-87`).
2. Auto-register WEB endpoint → mint signaling credential → open R1 WS (§3.2).
3. Learning of a call: **WS frames are treated as a doorbell only**; the offer list is always re-read over HTTP `GET /api/calling-signaling/v1/calling/invitations` on open, on every non-pong frame and every 10 s ("a missed push never hides a ringing call") (`useCallingDevice.ts:11-14,101-106,122-130,154`). Contract frames available but not parsed: `call.incoming`, `call.ringing`, `call.ring_cancelled`, `call.invitation_expired`, `call.answered_elsewhere`, … (`packages/contracts/calling/realtime/v1.schema.json`).
4. `ringingOffers()` filters actionable + unexpired, oldest first, one per call (`lib/calling/incoming-call.ts:20-26`).
5. Accept/Decline → `POST /api/calling-signaling/v1/calling/invitations/{id}/accept|decline` with `expectedVersion` + `Idempotency-Key`; accept then joins media via `POST …/sessions/{id}/media-credentials` (`services/calling/signaling.service.ts:12-20`; `CallingContext.tsx:170-174`). Decline ends this device's offer only (CAL-018) (`IncomingCallLayer.tsx:10`).
6. The Next route `/api/calling-signaling/[...path]` only allows those 4 operations, requires `Bearer cfsig_…` (≤2060 chars), body ≤4096, `redirect:"error"` upstream (`app/api/calling-signaling/[...path]/route.ts:16-31`).
- Multiple tabs: every tab opens its own WS with the same endpoint (shared localStorage key) and each tab rings; OS notification is deduped by `tag`.
- No push path: closed browser never rings (copy at `CallingDevicesPage.tsx:62`). Push-invitation payload contract exists but unused: `packages/contracts/calling/push/invitation/v1.schema.json` (`schema: cfmeet.calling.push-invitation`, callId, invitationId, callKind, caller.displayName, issuedAt, expiresAt).
- **Missed calls:** an offer that expires or is dismissed disappears silently (local `dismissed` Set; `IncomingCallLayer.tsx:27-41`). No missed-call toast/badge/notification. Missed calls only visible in Calls › Missed tab (`CallsContent.tsx:118-130,221`, outcome labels `:93-94`), polled every 10 s. Hub `InboxEntry.latestActivity.missed` typed (`types/communications.ts:207-213`) but not rendered.

### 5.2 Mobile
No incoming-call capability: no calling routes, no signaling, no CallKit/ConnectionService. Stated in UI: "The mobile app cannot place or receive calls yet." (`app/(settings)/communication.tsx:18-22`). No missed-call display.

---

## 6. Meeting reminders / invitations / email verification & OTP UI

### 6.1 Meeting reminders
- **Web & mobile: no reminder UI, no client-scheduled local notifications, no "starts in N min" alerts.** Upcoming meetings are shown passively: web `UpcomingMeetings` (`components/meetings/UpcomingMeetings.tsx:16-26`, `GET /api/v2/me/conferences` via `services/v2/conferences.service.ts:63-66`); mobile agenda (`features/meetings/hooks.ts:25-38`, 60 s poll; `features/meetings/api.ts:62-63`).
- Meeting notification emails are server-side only (handover: "Meeting notification email — Active, SMTP"; `NOTIFICATIONS_EMAIL_ENABLED`) (`docs/frontend/cfmeet-frontend-handover.md:65,414`); no frontend toggle.

### 6.2 Invitations
| Kind | Platform | UI | Endpoints | Email? |
|---|---|---|---|---|
| Meeting-space invite links | web | `InvitationsPanel` ("This doesn't send an email; share the link yourself.") | `GET/POST /api/v2/meeting-spaces/{id}/invitations`, `POST …/{invitationId}/revoke`, public `POST /api/v2/invitations/{token}/resolve` | **No** | `components/meeting-spaces/InvitationsPanel.tsx:33-49`; `services/v2/invitations.service.ts:19-52`; `hooks/v2/useInvitations.ts:24-31` |
| Organization invitations | web | `OrganizationInvitations` (checkbox `sendEmail`, shows `emailDelivery`) ; decision page `/organizations/invitations#<token>` | `POST /api/v2/organizations/{id}/invitations`, `POST /api/v2/organization-invitations/accept|decline` | Yes, optional (`EmailDelivery = DISABLED|ACCEPTED|FAILED`, `types/generated/meeting-service.ts:1254`) | `OrganizationInvitations.tsx:72-124`; `OrganizationInvitationDecision.tsx:11-55` |
| Organization invitations | mobile | `app/(settings)/invitation.tsx` accept/decline | `POST /api/v2/organization-invitations/accept|decline` | n/a | `routes.ts:62-63` |
| Meeting join invitation token | mobile | `cfmeet://join?code=&invitation=` | `GET /api/v2/resolve/{id}` then join | — | `app/join.tsx` |
| Contact (connect) invites / QR | web | `ContactsContent`, `ConnectInvite` | `/api/v2/communications/invites*`, `/me/qr`, `/discovery/invites/resolve` | No | `communications.service.ts:93-104` |
| Messaging conversation invitations | web | **none** — relay allowlists `GET v1/messaging/invitations`, `POST …/invitation/accept|decline` but nothing calls them | — | — | `lib/messaging/routes.ts:28-29` |

### 6.3 Email verification / OTP / reset flows
| Flow | Web UI | Mobile UI | Endpoint |
|---|---|---|---|
| Email verification (6-digit OTP) | `/(auth)/verify?email=` → `VerifyModal` (`autoComplete="one-time-code"`, resend cooldown) | `app/(auth)/verify.tsx`, `features/auth/hooks/use-verify-email.ts` | `POST /api/v1/auth/verify-email` `{email, token}` | `services/auth.service.ts:62`; `components/auth/VerifyModal.tsx:46-115`; mobile `routes.ts:36` |
| Resend verification | `VerifyModal` "Resend code" | `useResendVerification` | `POST /api/v1/auth/resend-verification` | `auth.service.ts:67`; `app/(auth)/verify/page.tsx:9-19`; mobile `routes.ts:37` |
| Forgot password (email code) | `/forgot-password` → `/forgot-password/verify-token?email=` → `/forgot-password/reset?email=&token=` | `app/(auth)/forgot-password.tsx` | `POST /api/v1/auth/forgot-password`, `/verify-reset-token`, `/reset-password` | `auth.service.ts:153-168`; `app/(auth)/forgot-password/verify-token/page.tsx:19-49`; `reset/page.tsx:11-20`; mobile `routes.ts:38-40` |
| New-browser device-link email code | Messages page "send email code" during account device linking | — | `POST /api/v2/me/device-links/{id}/email-code`, `POST …/complete {emailCode}` | `lib/auth/account-device.ts:57,61`; `lib/auth/account-device-core.ts:215-237`; `engine.ts:2128` |

---

## 7. Generated / hand-written contract types relevant to notifications

| Artifact | Generated? | Source contract | Notification-relevant content | Evidence |
|---|---|---|---|---|
| `apps/web/src/types/generated/meeting-service.ts` (3990 lines) | Yes (`npm run contracts:generate`, CI staleness check) | `packages/contracts/openapi/meeting-service.json` v1.5.0 | `EmailDelivery`, `VerifyEmailRequest`, invitation types, `client-tokens`/`session-tokens`; **no push/notification types** | file header lines 1-7; `:1254`; `apps/web/scripts/generate-contract-types.mjs:3,17` |
| `apps/mobile/lib/api/generated/meeting-service.ts` (2725 lines) | Yes (`node scripts/generate-contract-types.mjs --check` in `npm test`) | same contract (subset) | same | `apps/mobile/package.json` scripts.test |
| `apps/web/src/types/communications.ts` | **Hand-written** "subset", header says contract **1.2.0** (contract is **1.3.0**) | `communications/http/v1.openapi.json` | `ContactNotifications`, `InboxEntry{notifications, mutedUntil, latestActivity.missed}`, `ConversationContext.actions.mute`, `ActivityPage`, `CallHistoryEntry`; **no** `NotificationPreferences`, `DeliveryStatus`, `ConversationPreferences`, `Devices` | `types/communications.ts:1-8,107,185-213` |
| `apps/web/src/types/calling/*` | Hand-written, "mirrors" calling control schema | `calling/control/v1.schema.json` | `RegisterEndpointRequest.pushTargetRef`, `Endpoint.pushTargetRef`, `CallHistoryEntry.participation.missed/outcome` | `types/calling/endpoint.ts:1-55`; `types/calling/history.ts:14-100` |
| `apps/web/src/types/messaging.ts` | Hand-written | messaging http | devices/tokens; no push types | — |
| `sdks/typescript/src/generated/operations.ts` | Generated SDK, **not consumed by apps** | aggregate contracts | `getNotificationPreferences`, `updateNotificationPreferences`, `registerCommunicationsDevicePush` / `removeCommunicationsDevicePush` (idempotency REQUIRED), messaging `getPushConfiguration`, `registerPush`, `unregisterPush` | `operations.ts:195,214,216,235,485,518,538`; `apps/web/src/lib/developer-platform/client.ts:14` (explains why hand-written) |
| Contracts (reference) | — | `packages/contracts/messaging/common/v1.schema.json` `PushRegistrationRequest{platform: APNS|FCM|WEB_PUSH|LOCAL_TEST, handle 8..4096, enabled}`, `PushRegistration{pushDeliveryConfigured}`, `PushConfiguration{platforms, webPush.applicationServerKey (87-char b64url P-256), payload: METADATA_ONLY}`; `calling/push/invitation/v1.schema.json`; `calling/realtime/v1.schema.json` (call.* frames) | — | — |

---

## 8. Notification-related backend endpoints referenced by each app

Legend: **Called** = invoked at runtime; **Allowlisted only** = present in a client allowlist but no caller; paths are as seen by the backend (web adapter prefixes `/api/calling` and `/api/calling-signaling` are Next.js same-origin proxies to the Calling service).

### 8.1 Web

| # | Method | Path (backend) | Via | Status | File:line |
|---|---|---|---|---|---|
| W1 | GET | `/identity` (calling principal) | `/api/calling` adapter | Called | `services/calling/endpoints.service.ts:5` |
| W2 | GET | `/v1/calling/endpoints` | adapter | Called | `endpoints.service.ts:6` |
| W3 | POST | `/v1/calling/endpoints` | adapter | Called (auto) | `endpoints.service.ts:7-8` |
| W4 | DELETE | `/v1/calling/endpoints/{id}` | adapter | Called | `endpoints.service.ts:9-10` |
| W5 | POST | `/v1/calling/signaling-credentials` | adapter | Called | `endpoints.service.ts:11-12` |
| W6 | WS | `/v1/calling/signaling` (`bearer.<cfsig>` subprotocol) | direct to Calling origin | Called | `services/calling/signaling.service.ts:21-25` |
| W7 | GET | `/v1/calling/invitations` | `/api/calling-signaling` | Called (10 s + on frame) | `signaling.service.ts:12` |
| W8 | POST | `/v1/calling/invitations/{id}/accept` / `/decline` | `/api/calling-signaling` | Called | `signaling.service.ts:16-19` |
| W9 | POST | `/v1/calling/sessions/{id}/media-credentials` | `/api/calling-signaling` | Called | `signaling.service.ts:20` |
| W10 | GET | `/v1/calls` (`missed` filter) — call history / missed | adapter | Called | `services/calling/calls.service.ts:41-44` |
| W11 | GET | `/v1/calls/{id}` (caller identity) | adapter | Called | `calls.service.ts:51-54` |
| W12 | GET | `/v1/enterprise/voicemail/mailboxes`, `/messages`, `/messages/{id}` | adapter (owner-filtered) | Called | `services/calling/voicemail.service.ts:19-27` |
| W13 | PUT | `/v1/enterprise/voicemail/messages/{id}/read-state` | adapter | Called | `voicemail.service.ts:41` |
| W14 | POST | `/api/v2/messaging/principals/names` | Meeting | Called (caller label) | `lib/calling/incoming-call.ts:6` |
| W15 | POST | `/api/v2/messaging/client-tokens` | Meeting | Called | `services/v2/messaging.service.ts:23` |
| W16 | POST | `/api/v2/messaging/session-tokens` | Meeting | Called | `messaging.service.ts:25-26` |
| W17 | POST | `/v1/realtime/tickets` | Messaging origin direct | Called | `services/messaging-realtime.ts:9,179-182` |
| W18 | WS | `/v1/realtime` | Messaging origin direct | Called | `messaging-realtime.ts:10,189-192` |
| W19 | POST | `/api/v2/messaging/devices/{id}/revoke` | Meeting | Called (logout, devices panel) | `messaging.service.ts:21-22` |
| W20 | GET | relay `v1/conversations` (inbox counts), `v1/conversations/{id}/unread`, PUT `…/read-cursor`, GET `…/events` | `/api/v2/messaging/relay/…` | Called | `lib/messaging/routes.ts:10,21,24`; `engine.ts:1444` |
| W21 | POST | relay `v1/messaging/client-sessions/{id}/revoke` | relay | Called | `routes.ts:45` |
| W22 | PUT/DELETE | relay `v1/messaging/devices/{id}/push-registration` | relay | **Allowlisted only** | `routes.ts:47` |
| W23 | GET | relay `v1/messaging/push/configuration` (VAPID key) | relay | **Allowlisted only** | `routes.ts:48` |
| W24 | GET/PUT | relay `v1/messaging/presence/settings` | relay | Allowlisted only (no caller found) | `routes.ts:46` |
| W25 | GET / POST | relay `v1/messaging/invitations`, `…/invitation/accept|decline` | relay | **Allowlisted only** | `routes.ts:28-29` |
| W26 | GET | `/api/v2/communications/bootstrap`, `/sync?after=` | Meeting→Account relay | Called (Contacts page only) | `communications.service.ts:89,143` |
| W27 | GET | `/api/v2/communications/inbox` (mute) | Hub | Called | `communications.service.ts:129` |
| W28 | PATCH | `/api/v2/communications/contacts/{id}` (`notifications`) | Hub | Called | `communications.service.ts:117-118` |
| W29 | GET/PUT | `/api/v2/communications/me/privacy` | Hub | Called | `communications.service.ts:96-97` |
| W30 | GET | `/api/v2/communications/contact-requests/incoming` | Hub | Called | `communications.service.ts:108` |
| W31 | GET | `/api/v2/communications/activity` | Hub | **Client method defined, no UI consumer** | `communications.service.ts:132`; `useCommunications.ts:74-76` |
| W32 | GET | `/api/v2/communications/calls/history` | Hub | **Defined, no UI consumer** | `communications.service.ts:133`; `hub-calls.service.ts:12` |
| W33 | GET | `/api/v2/communications/conversations/{id}/timeline`, `/context` | Hub | Called | `communications.service.ts:130-131` |
| W34 | POST | `/api/v2/communications/contacts/{id}/calls`, `/conversations/{id}/calls` | Hub | Called (call launch) | `services/calling/hub-calls.service.ts:60-68` |
| W35 | POST | `/api/v2/conferences/{id}/realtime/ticket`; WS `<websocketPath>?ticket&cursor`; GET `/api/v2/conferences/{id}/events` | Meeting | Called (in room) | `services/v2/realtime.service.ts:17-49` |
| W36 | GET/POST | `/api/v2/meeting-spaces/{id}/invitations`, POST `…/{invId}/revoke`, POST `/api/v2/invitations/{token}/resolve` | Meeting | Called | `services/v2/invitations.service.ts:19-52` |
| W37 | POST | `/api/v2/organizations/{id}/invitations` (`sendEmail`), `/api/v2/organization-invitations/accept|decline` | Meeting | Called | `OrganizationInvitations.tsx:83`; `services/v2/business-organizations.service.ts:108` |
| W38 | POST | `/api/v1/auth/verify-email`, `/resend-verification`, `/forgot-password`, `/verify-reset-token`, `/reset-password`, `/logout`; `/api/v2/me/sessions/revoke-all` | Meeting (account) | Called | `services/auth.service.ts:57-168` |
| W39 | POST | `/api/v2/me/device-links/{id}/email-code`, `/complete`, `/approve`, `/reject`; GET `/me/device-links` | Meeting | Called | `lib/auth/account-device.ts:54-61` |
| W40 | GET/POST/PATCH/DELETE | `/webhooks` (legacy) ; `/v1/environments/{envId}/webhooks*` (developer webhooks; outbound notification channel for developers) | v1 / dev gateway | Called | `services/webhook.service.ts:14-29`; `lib/developer-platform/client.ts:75-81` |
| — | GET/PUT | `/api/v2/communications/me/notification-preferences` | Hub | **Not referenced** | contract only |
| — | GET | `/api/v2/communications/me/notification-delivery-status` | Hub | **Not referenced** | contract only |
| — | GET/PUT | `/api/v2/communications/conversations/{id}/preferences` | Hub | **Not referenced** | contract only |
| — | GET / PUT / DELETE / POST | `/api/v2/communications/devices`, `/devices/{id}/push`, `/devices/{id}/revoke` | Hub | **Not referenced** | contract only |
| — | GET | `/api/v2/communications/events` | Hub | **Not referenced** | contract only |

### 8.2 Mobile (complete; route table is exhaustive by design, `lib/api/routes.ts:1-11`)

| # | Method | Path | Status | File:line |
|---|---|---|---|---|
| M1 | POST | `/api/v1/auth/verify-email` | Called | `routes.ts:36`; `features/auth/api/auth.api.ts:23-24` |
| M2 | POST | `/api/v1/auth/resend-verification` | Called | `routes.ts:37`; `auth.api.ts:25-26` |
| M3 | POST | `/api/v1/auth/forgot-password`, `/verify-reset-token`, `/reset-password` | Called | `routes.ts:38-40` |
| M4 | POST | `/api/v1/auth/logout` | Called (no device cleanup) | `routes.ts:42`; `use-logout.ts:13-19` |
| M5 | GET / POST | `/api/v2/me/sessions`, `/me/sessions/{id}/revoke`, `/me/sessions/revoke-all` | Called | `routes.ts:49-51`; `features/account/api.ts:14-16` |
| M6 | POST | `/api/v2/organization-invitations/accept|decline` | Called | `routes.ts:62-63` |
| M7 | GET | `/api/v2/me/conferences` (agenda, 60 s) | Called | `routes.ts:59`; `features/meetings/api.ts:62-63` |
| M8 | GET | `/api/v2/conferences/{id}/waiting-room` (4 s) ; POST `…/admit|deny` | Called | `routes.ts:94-96` |
| M9 | GET/POST/PUT | `/api/v2/messaging/relay/v1/conversations…` (list 15 s, messages 5 s, read-cursor) | Called | `routes.ts:132-134`; `relay-paths.ts:47-74` |
| M10 | GET | relay `v1/conversations/{id}/unread` | Allowlisted only | `relay-paths.ts:60,73` |
| M11 | GET/POST | `/api/v2/messaging/me`, `/directory`, `/directory/resolve`, `/principals/names` | Called | `routes.ts:123-126` |
| — | any | push / notification / Hub / Calling | **None** | — |

---

## 9. Gaps, inconsistencies, security concerns

### 9.1 Backend exists, UI absent
| ID | Gap | Evidence |
|---|---|---|
| G1 | Web Push end-to-end (SW, VAPID fetch, `pushManager.subscribe`, `push-registration`) — backend + contract + relay allowlist ready; client absent | `routes.ts:47-48`; contract `PushConfiguration` |
| G2 | Account-wide notification preferences (7 toggles) — no screen on web or mobile | Hub contract; §4 |
| G3 | Notification delivery diagnostics (`NOT_CONFIGURED` truthfulness) — not surfaced | Hub contract |
| G4 | Per-conversation mute / mutedUntil / mediaAutoDownload — web shows muted icon only, no control; `ConversationContext.actions.mute` typed but unused | `types/communications.ts:203`; `MessagesContent.tsx:1015` |
| G5 | Hub devices list + push target + revoke — not used; web manages Calling endpoints and Messaging devices separately (two device UIs, two IDs) | `CallingDevicesPage.tsx`; `MessagingDevicesPanel.tsx` |
| G6 | Hub activity feed and Hub missed-call history — hooks written, never rendered | `useCommunications.ts:74-79` |
| G7 | Calling push-invitation contract + `pushTargetRef` — never populated by web | `types/calling/endpoint.ts:23`; `calling/push/invitation/v1.schema.json` |
| G8 | Messaging conversation invitations (accept/decline) — allowlisted, no UI | `routes.ts:28-29` |
| G9 | Hub change-feed `NOTIFICATION_PREFERENCE` domain → `"notifications"` area with no query to invalidate; `useHubSync` only mounted on Contacts page | `comms-core.ts:125-127`; `ContactsContent.tsx:554` |
| G10 | Mobile: no push, no realtime, no calling, no unread counts (`unread` path unused), no Hub | §1.2, §3.4 |
| G11 | Calling realtime frames (`call.incoming`, `call.ring_cancelled`, `call.answered_elsewhere`, …) are not parsed — client relies on HTTP polling, so a Notification Service cannot assume clients act on frame payloads | `useCallingDevice.ts:122-130` |

### 9.2 UI exists, backend semantics unclear / inconsistent
| ID | Item | Evidence |
|---|---|---|
| I1 | Two "notification level" stores for the same person: per-contact `Contact.notifications` (PATCH `/contacts/{id}`) and per-conversation `ConversationPreferences.notifications`; web writes only the former and reads mute from `InboxEntry` (conversation-level). Precedence between contact-level and conversation-level mute is undefined in the client. | `ContactsContent.tsx:264,302`; `MessagesContent.tsx:1015` |
| I2 | Missed calls sourced from Calling `/v1/calls?missed=true` (web UI) vs Hub `/calls/history?missed` (unused) vs Hub `InboxEntry.latestActivity.missed` (unused) — three sources of truth. | `calls.service.ts:41`; `communications.service.ts:133`; `types/communications.ts:212` |
| I3 | Web `types/communications.ts` header claims contract 1.2.0; served contract is 1.3.0; types are hand-maintained (drift risk, no CI check unlike generated meeting-service types). | `types/communications.ts:2`; `communications/http/v1.openapi.json` info.version |
| I4 | Mobile deep-link docs disagree: `cfmeet://invitation?token=` (screen) vs `cfmeet://organizations/invitations?token=` (parser doc; no matching route). | `app/(settings)/invitation.tsx:3`; `lib/invitations.ts:6` |
| I5 | Mobile `UIBackgroundModes` includes `voip` without PushKit/CallKit — App Store review risk; and no Android notification permission for API 33+. | `app.json:26-29,41-48` |
| I6 | Web unread counts and Messaging realtime run only while the Messages page is mounted (`new MessagingEngine()` inside `MessagesContent`); no app-wide unread indicator, no message notification of any kind outside that page. | `MessagesContent.tsx:55`; `TopBar.tsx` |
| I7 | Background call notification is only shown if the tab is already hidden when ringing starts (effect deps don't include visibility), and it is never re-shown on `visibilitychange`. | `IncomingCallLayer.tsx:66-75` |
| I8 | Meeting-space invites never email (by design per copy) while org invites optionally email with delivery status — inconsistent invitation notification semantics for a central service. | `InvitationsPanel.tsx:34`; `OrganizationInvitations.tsx:104-124` |

### 9.3 Security observations
| ID | Severity (auditor view) | Observation | Evidence |
|---|---|---|---|
| S1 | Medium | **Calling endpoint not revoked on logout.** Server endpoint stays ACTIVE with a stable `endpointKey` in localStorage; offers continue to be issued to an endpoint nobody is listening on (affects "answered elsewhere"/missed semantics) and a later user of the same browser profile with the same principal re-binds to it. A future push target bound to this endpoint would keep receiving call pushes after logout. | `auth.service.ts:97-105,134-142`; `useCallingDevice.ts:28,51-58` |
| S2 | Low–Medium | Calling signaling bearer token (`cfsig_…`, ≤3600 s) is transported in the `Sec-WebSocket-Protocol` header (`bearer.<token>`), which proxies/load balancers commonly log. Messaging uses a single-use 64-hex ticket instead (better). | `signaling.service.ts:24`; contrast `messaging-realtime.ts:192` |
| S3 | Low | Conference realtime ticket and cursor are put in the WebSocket **query string** (access-log exposure; mitigated if single-use/short-lived). | `services/v2/realtime.service.ts:46-47` |
| S4 | Low–Medium | Password-reset code is placed in the URL query of `/forgot-password/reset?email=&token=` (browser history; `Referrer-Policy: strict-origin-when-cross-origin` limits cross-origin leakage to origin). | `app/(auth)/forgot-password/verify-token/page.tsx:30-31`; `reset/page.tsx:11-20`; `csp.ts:130` |
| S5 | Low | Mobile custom-scheme deep links carry invitation tokens (`cfmeet://invitation?token=`, `cfmeet://join?…&invitation=`); custom schemes can be claimed by other apps. No universal/app links configured. | `app/(settings)/invitation.tsx:3`; `app/join.tsx:2-3`; `app.json` |
| S6 | Info (positive) | No open-redirect surface from notifications: web notification click only focuses the window; `?next=` is strictly allowlisted; mobile deep links navigate internally only. A future SW `notificationclick` must keep a same-origin path allowlist (reuse `safeReturnPath`). | `IncomingCallLayer.tsx:72`; `organization-core.ts:323-328`; `open-meeting.ts:19-28` |
| S7 | Info (positive) | E2EE: no plaintext leaves the device for notifications; list previews for E2EE are local-only or "Encrypted message"; contracts mandate metadata-only push payloads. A future Web Push SW cannot decrypt (MLS keys live in a dedicated worker + IndexedDB with a per-tab lock), so E2EE push must stay generic ("New message") unless the SW gains crypto access. | `MessagesContent.tsx:1084`; `engine.ts:777-783`; `signout.ts:12-34` |
| S8 | Info | Multiple tabs each hold a signaling WS for the same endpoint and each rings; OS notification deduped by `tag`, ringtone is not. | `useCallingDevice.ts:93-160`; `IncomingCallLayer.tsx:71` |
| S9 | Info | `/api/calling-signaling` proxy is tightly scoped (4 ops, bearer format check, size caps, `redirect:"error"`, https-only upstream). | `app/api/calling-signaling/[...path]/route.ts:16-31` |

### 9.4 Compatibility constraints for a centralized Notification Service (derived)
1. Web clients already depend on: Calling endpoint registry + signaling WS + `/v1/calling/invitations` HTTP recovery; Messaging realtime ticket/WS protocol `cfmeet.messaging.v1` with ack cursors; Hub `/inbox` (`notifications`, `mutedUntil`), `PATCH /contacts/{id}` (`notifications`), `/sync` domains incl. `NOTIFICATION_PREFERENCE`/`CONVERSATION_PREFERENCE`. These must remain stable or be versioned.
2. Web relay allowlist (`lib/messaging/routes.ts`) mirrors Meeting `relay.rs` ROUTES (push-registration at `services/meeting-service/src/services/unified_messaging/relay.rs:460-471,979`); moving push registration to a new service requires changing both or adding a Hub-style façade (`/api/v2/communications/devices/{id}/push`, which already "delegates to the existing Messaging push adapters").
3. Mobile can only call routes present in `packages/contracts/openapi/meeting-service.json` (enforced by `apps/mobile/tests/routes.test.ts` per `routes.ts:4-8`); Hub/Notification endpoints must be added to that contract (or the test extended) before mobile can adopt them.
4. Hub mutations require `Idempotency-Key` (web `communicationsRequest` adds one automatically, `communications.service.ts:62-70`); Hub preference PUTs use `expectedVersion` optimistic concurrency (`VERSION_CONFLICT`).
5. Any push payload must stay metadata-only (conversation id + sequences; call: callId/invitationId/kind/expiresAt + optional caller displayName) to remain compatible with existing contracts and the E2EE threat model; clients will then re-fetch via the existing HTTP recovery paths (`/v1/calling/invitations`, relay `…/events`).
