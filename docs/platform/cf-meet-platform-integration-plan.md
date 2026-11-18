# CF Meet Platform: third-party integration plan (Build + Integrate), backend — 2026-11-18

> This document covers only the third-party integration. It is separate from the passwordless sign-in (email + code) work, which is finished, deployed and documented elsewhere (`docs/frontend/passwordless-auth-handover.md`; PRs #97 and #100). Earlier third-party drafts are replaced by this plan; PR #110 (naming and docs) is parked and folds into phase B0.


## Context
- **Goal.** Outside companies ("partners", e.g. KarryGo) put CF Meet chat, calls and meetings inside their own web and mobile apps for their own users. Those users never have CF Meet accounts. Outside apps also plug into CF Meet for CF Meet's own users (like Slack or Zoom marketplace apps).
- **Starting point.** The audit and research are in the Appendix below. Today a partner reaches only sandbox server APIs. It cannot:
  - create its users;
  - give its users device logins;
  - reach partner websites;
  - send pushes to its app;
  - use meetings;
  - go live;
  - use OAuth or an app directory.
- **Owner decisions (2026-11-18):**
  - Build AND Integrate together.
  - Pushes go out with the partner's own APNs/VoIP/FCM keys, through the Notification Service (we build it there).
  - **Scope (owner, 2026-11-18):** we work on the Notification Service and every other backend service where needed; only the frontend (`apps/web`, `apps/mobile`) is out of scope.
  - Partner chat is MANAGED (server-readable) by default; E2EE optional.
  - Core SDKs come first (TypeScript, Flutter/Dart, React Native); ready-made screens later.
- **Constraints**:
  - backend only (never `apps/web` or `apps/mobile`);
  - Notification Service changes are ours too: careful, additive, behind its own flags, and its existing first-party behaviour must stay unchanged;
  - telecom stays off;
  - every feature is behind a default-off flag and switched on by a journaled `coordinated_release.py` activation whose post-checks use **two fresh tenants**, including cross-tenant 404 checks;
  - migrations are additive;
  - CF Meet's own apps stay on `/api/v2`;
  - each phase is its own PR to `main` after green CI;
  - **no new services**.
- **PR #110** (names, docs, ADR 0061) stays parked; it is merged as part of B0 when building starts.

## Architecture (recommended)

**1. Partner users.** Platform Access principals of kind `EXTERNAL_APPLICATION_PRINCIPAL`, created through a public **Users API** on the Developer Console:
- `POST/GET/PATCH/DELETE /v1/users`, idempotent on `externalId`;
- `POST /v1/users/{id}/revoke-sessions`.

The Developer Console delegates to a new Platform Access router `src/http/end_users.rs`; Platform Access itself is never public. New migration: profiles (display name and image), a per-principal `tokens_not_before`, and per-environment caps (SANDBOX 100 / TEST 1 000 / PRODUCTION 100 000).

**2. One "CF Meet user token"** (`cfut_<env>_<JWS>`).
- **Signing:** Ed25519, signed by Platform Access with its own key.
- **Minting:** the partner backend calls `POST /v1/users/{id}/tokens`, scope `user_tokens.issue`.
- **Grants:**
  - `msg` (read / send);
  - `call` (receive);
  - `mtg` (`[{conference, role}]`, at most 10).
- **Lifetime:** 1 h by default.
- **Revocation (stateless):** the token dies when its minting key is revoked, the principal is suspended, the user's sessions are revoked, the environment is suspended, or the signing key rotates.
- **Verification:** product services verify live through `POST /internal/v1/user-tokens/verify`, with a cache of 30 s or less.
- **Refresh:** the SDK calls the partner's `tokenProvider`.

**3. Device exchange.** Devices trade the user token for the credentials each service already issues; device credentials keep the minting key id (`crd`), so revoking the partner's key ends them all:

| Product | Exchange route | What the device gets back |
|---|---|---|
| Chat | `POST /v1/messaging/user-sessions` | partner session device + client token |
| Calls | `POST /v1/calling/user-sessions` | endpoint + `cfsig_` |
| Meetings | `POST /api/v2/conferences/{id}/join` (now also accepts a user token) | `pt_` + LiveKit credential |

**4. Allowed websites per environment.**
- Stored in Platform Access (exact `https://` origins, at most 20; localhost allowed in test only) and managed from the Developer Console.
- Used by the gateway's CORS (only on routes that accept device logins, so API keys never reach browsers) and by Messaging's ticket and WebSocket origin checks.
- Partner device HTTP and WebSocket traffic goes through `api.cfmeet.com`; media stays direct.

**5. Partner chat (MANAGED by default).**
- Session devices are opened to EXTERNAL tenants with a binding to the user token.
- New closed REST allowlist `PARTNER_SESSION_DEVICE_ROUTES` so partner devices can send, not just receive; refused for PRIVATE_E2EE.
- New policy baseline `CFMEET_PLATFORM_PARTNER` (MANAGED by default, E2EE allowed per conversation).
- Usage fact `MESSAGE_SENT`.

**6. Push with the partner's keys.**
- The Developer Console uploads partner APNs (incl. VoIP) and FCM keys, sealed to the Notification Service on upload, and keeps only metadata.
- Messaging is the device push-registration facade (`platform-principal` subject).
- Calling and Messaging send to a `platform-principal` recipient.
- We build N-P1..N-P5 in `services/notifications` (plus `packages/notification-client` and its contracts):
  - **N-P1** provider vault, scope-locked;
  - **N-P2** registrations;
  - **N-P3** VoIP/FCM fast lane for calls;
  - **N-P4** catalogue recipient kind;
  - **N-P5** observability.

**7. Calls.**
- The partner's server places calls (`POST /v1/calls`); this exists today. Devices use their exchanged `cfsig_`.
- Recommended: move `/v1/calls` and `/v1/calling` behind the gateway, which brings partner-website CORS, quotas and call metering.

**8. Meetings.**
- Turn on `MEETING_PLATFORM_ACCESS_ENABLED` with gateway prefixes on `api.cfmeet.com`.
- New `ActorKind::PlatformUser`: join is principal-bound, with the role taken from the grant, the name from the user's profile, and idempotent re-join (`meeting_platform_participants`).
- Meeting chat for partner meetings uses an environment system principal.
- `meeting.*` webhooks.
- Recordings come later.

**9. Metering and limits.**
- New usage facts: `MESSAGE_SENT`, `ACTIVE_USER_DAY` (gives MAU), and `API_OPERATION` for calls via the gateway.
- Caps: 3 workspaces per account, 10 projects per workspace, user and token-mint limits, sandbox meetings at most 4 people and 40 minutes.
- Billing stays metadata until pricing is decided.

**10. Go-live.** Name reviewers; new capabilities `END_USERS` and `PUSH`; production requires HTTPS origins and production APNs.

**11. Integrate (CF Meet Apps), no new service.** The OAuth work is split across existing services:
- **Platform Access** is the token authority: OAuth clients, grants, codes, `cfoa_` access tokens (1 h), rotating `cfor_` refresh tokens (reuse revokes the grant), `cfob_` bot tokens, and a closed OAuth scope catalogue (`profile:read`, `meetings:read/write`, `chat:*:bot`, `chat:read/write`; PRIVATE_E2EE never readable by apps).
- **Meeting Service** handles `/api/v2/oauth/authorize`: the user's sign-in, consent relay, and "Connected apps" (`/api/v2/me/apps`).
- **Developer Console** handles app registration (client id/secret, exact redirect URIs, PKCE S256 mandatory), `/oauth/token`, `/oauth/revoke`, `/v1/me`, review and the directory.
- **Organizations** holds the install policy for workspaces (admins only by default).

Products authorize app tokens as follows:
- the gateway treats them as a new auth mode with a principal restriction to the user (or the bot);
- a new `acting_application_id` field in the delegated context, rolled out in two steps;
- Meeting Service: `ActorKind::DelegatedUser`, which may create and manage only the user's own meetings;
- Messaging: MANAGED-only access, and messages record which app sent them.

Bots are BOT_OR_SERVICE principals in organization MANAGED chats. Install events reuse the central webhooks with installation routing. In-meeting apps get a short-lived context token (the iframe itself is frontend work).

## Phases and milestones
Build track (B) and Integrate track (I) run in parallel. Platform Access migrations go in one sequence.

| Phase | What | Milestone / acceptance (2 tenants, cross-tenant 404) |
|---|---|---|
| **B0** Foundations | Merge #110 naming; ADRs 0062 (Build), 0063 (push handover), 0064 (Apps), 0065 (review/directory/in-meeting); new service permissions in `packages/service-auth/src/permissions.rs`; `userToken` auth mode in `public_api.py` and the gateway (parse only); scope `user_tokens.issue` | CI green; route table unchanged |
| **B1** Users API + caps | Platform Access `end_users.rs`, profiles, caps; Developer Console `control/users.rs`, workspace/project caps; `activate-platform-users` | Create/upsert users; B gets 404 on A's user; phone-number id refused; cap enforced |
| **B2** User tokens + allowed websites | Platform Access `identity/user_tokens.rs`, `environment_origins.rs`; Developer Console token + origins routes, sandbox token generator; gateway `origins.rs` and CORS; `activate-platform-user-tokens` | Tampered or foreign token refused; key revocation takes effect within 30 s; preflight allowed for partner website only on device routes |
| **B3** Partner chat | Messaging `session_devices.rs`, `sessions.rs`, `middleware.rs`, new `realtime/user_sessions.rs`, `realtime/http.rs` origins, `policy.rs` partner baseline, `MESSAGE_SENT`; `activate-partner-chat` | User sends and receives live in a MANAGED chat; E2EE refused; revoking the user closes the socket |
| **B4** Partner calls | Calling `endpoints/user_sessions.rs`; `/v1/calls` and `/v1/calling` moved behind the gateway (journaled snippet with automatic restore); `activate-partner-calls` | **M1: KarryGo rider calls and chats with a customer in sandbox (app open)** |
| **B5** Push | Notification Service N-P1..N-P5 (provider vault, `platform-principal` registrations, VoIP/FCM call fast lane, catalogue recipient kind, metrics); Developer Console `control/push_credentials.rs`; Messaging push registrations + wake-ups; Calling `signaling/push.rs` platform recipient; `activate-partner-push` | **M2: closed KarryGo app rings**; A's call never reaches B's phones |
| **B6** Meetings | Meeting Service `actor.rs` PlatformUser, `admission.rs` grant join, `meeting_platform_participants`, `unified_messaging` partner chat; Platform Access widened `ensure`; `activate-platform-meetings` | **M3: KarryGo support video meeting in sandbox** with chat; foreign token 404; minutes and webhooks recorded |
| **B7** SDKs (alongside B3–B6) | TypeScript and Dart: `userToken` + `tokenProvider`, chat/calls/meetings/push clients; new `sdks/react-native` (TS core + native bridge interface); v0.2.0; publish after M3 | Generator check; sample app against local stack |
| **B8** Go-live | Reviewers activation; `END_USERS`/`PUSH` capabilities; production checks; `ACTIVE_USER_DAY` exporter; `activate-platform-metering` | **M4: KarryGo live in production**; MAU visible in usage |
| **I1** App registration (parallel with B1–B3) | Developer Console `control/apps.rs` (+ app versions); Platform Access `platform_oauth_clients`; `activate-oauth-apps` | Register app, get client id/secret |
| **I2** OAuth + user installs | Platform Access `identity/oauth.rs`, `oauth_scopes.rs`; Meeting Service `services/oauth_authorize.rs`; Developer Console `/oauth/token`; gateway OAuth auth mode; `activate-oauth` | **M5: an app signs in a CF Meet user**; PKCE required; code and refresh reuse revoke the grant |
| **I3** Delegated product access | `acting_application_id` (two-step rollout); Meeting Service `DelegatedUser` + `delegated_operations.rs`; Messaging restrictions; `activate-delegated-apps` | **M6: calendar app creates CF Meet meetings for consenting users**; E2EE chats 404 to apps |
| **I4** Workspace installs + bots | Organizations `policy.rs` apps section; Platform Access install service account + bot principal; Messaging bot rules; `activate-app-installs` | Non-admin install refused; bot can't join E2EE chats |
| **I5** Install events | Developer Console `webhooks/ingest.rs` installation routing, new event types; `activate-app-events` | **M7: workspace bot answers mentions** |
| **I6** Review + directory | Review workflow (reviewers, no self-approval, CLI), `/v1/app-directory`; organization admin app controls; `activate-app-directory` | **M8: public app listed and installable** |
| **I7** In-meeting apps | Meeting Service apps list + context tokens + key publication; `activate-in-meeting-apps` | **M9** |
| **Later** | E2EE partner chats, partner-signed tokens, recordings/transcripts, device-placed calls, guest join links, embeddable prebuilt pages, partner web push, UI kits | — |

## Handovers (not our code)
- **Frontend:**
  - console pages: Users, Allowed websites, test token generator, Push keys, Apps, review;
  - OAuth consent page and Connected apps;
  - app directory and organization admin Apps page;
  - in-meeting app panel;
  - later the embeddable prebuilt pages.
- **Mobile:** CallKit/PushKit and ConnectionService glue for the RN/Flutter SDKs (unless decision 10 gives it to us); UI kits later.

## Security and risks
- **Invariants:**
  - environment only from a verified credential or token;
  - foreign resources answer 404;
  - device credentials re-check user, epoch and key;
  - push keys are scope-locked;
  - apps never read E2EE;
  - first-party scopes are never delegable.
- **Risks and mitigations:**
  - *token theft*: short TTL, origin checks, per-token ceiling, revoke-sessions;
  - *Platform Access verify load*: cache, plus load tests before B8;
  - *calling edge move*: canary, automatic restore, first-party smoke test;
  - *display names stored as PII*: erased on delete, never logged;
  - *Notification Service changes*: additive and flag-gated; its first-party push, email and inbox tests must stay green; partner credentials are scope-locked so one partner's key can never send to another's users.

## Open owner decisions (asked at the phase that needs them)
1. Do partner devices go through `api.cfmeet.com`, and do `/v1/calls` and `/v1/calling` move behind the gateway? (recommended yes; B2/B4)
2. Names of the production and app reviewers; is auto-approval allowed? (B8/I6)
3. May partner users' display names and images be stored? (B1)
4. White-label, or "Powered by CF Meet"?
5. Push previews: metadata only, or message text for MANAGED chats? (B5)
6. Pricing basis and plan caps. (B8)
7. App review policy: which scopes need review; DRAFT test-user limit. (I6)
8. Workspace installs admin-only by default? Should logout-everywhere revoke app grants? (I2/I4)
9. Recordings and transcripts for partners: consent and retention. (later)
10. Who builds the native push and CallKit glue for the SDKs: us or the mobile team? (B5/B7)

## Verification
- **Every PR:**
  - service unit and integration suites (local Postgres :55432, Redis :56379);
  - contracts validator, `public_api.py --check` (every new route classified), `bundle_openapi`, `sdks/generate.py --check`, frontend reference and first-party ingress checks;
  - TypeScript SDK typecheck and tests;
  - production tooling tests.
- **Cross-service:** user token → session device → send (Messaging); join with user token (Meeting); exchange then call (Calling), against the real Platform Access test harness. A two-tenant isolation matrix per service (A's token × B's resource → 404).
- **Production:** each `activate-*` runs a new `platform_acceptance.py` (extends `developer_acceptance.py`) against `api.cfmeet.com`. It uses two fresh workspaces, proves the milestone and the cross-tenant refusals, always cleans up, and the journal handles rollback. Read-only `certify` afterwards.

---

# Appendix: audit and research behind this plan (2026-11-18)

The evidence the plan above is based on. Nothing here is a build step.

## 1. Deep audit: what a third party can really do today

### 1a. The journey from sign-up to live
| # | Step | Today | Why |
|---|---|---|---|
| 1 | Make a CF Meet account, sign in | ✅ Works | Normal CF Meet account (email + code) |
| 2 | Create a developer workspace (called "organization") | ✅ Works | Live in production since the console activations |
| 3 | Invite teammates | ⚠️ Partly | No invite email is sent; the owner gets the invite token and must pass it on by hand |
| 4 | Create a project, test environment, API key (`cfmk_sbx_…`) | ✅ Works | Self-service for SANDBOX/TEST only |
| 5 | Call chat and calling APIs from their server | ✅ Works | Through `api.cfmeet.com/v1`, with quotas |
| 6 | Create their own users (the people in their app) | ❌ Blocked | Only CF Meet staff can (operator-only route); no public API |
| 7 | Send chat "as" one of their users | ❌ Blocked | Needs step 6 |
| 8 | Give their users live chat on their phones/browsers | ❌ Blocked | See 1b |
| 9 | In-app voice/video calls for their users | ⚠️ Native apps only, after step 6 | See 1b |
| 10 | Meetings | ❌ Blocked | Meeting API for keys is switched off; no way to let their user join with their own name |
| 11 | Webhooks (signed, retried, replayable) | ✅ Works | Live for calling and chat events; meeting events can't happen yet |
| 12 | See usage | ⚠️ Partly | Only API-call counts; chat messages and meeting minutes show nothing |
| 13 | Go live (production) | ❌ Blocked | The review is switched off: no reviewers named |
| 14 | Telecom (numbers, SMS) | ❌ Off | Your decision |
| 15 | Pay / billing | ❌ None | Plans exist as labels only; nothing charges money |
| 16 | Published SDKs, guides | ❌ None | SDKs exist but are private; no getting-started guides |

### 1b. What a third party's **end-user app** (web or mobile) can do
| Capability | Today | Blocker |
|---|---|---|
| Live chat, encrypted (E2EE) | ⚠️ Native apps only | The device must run CF Meet's encryption library. Browsers on the partner's website are refused (only cfmeet.com is allowed). **Correction to my earlier audit:** E2EE devices are switched ON in production (since 2026-11-15), not off |
| Live chat, not encrypted (simpler) | ❌ | This kind of device login exists but is locked to CF Meet's own app |
| Partner picks encrypted vs not, per conversation | ✅ | Already supported (policy per app) |
| Push notifications to the partner's app (new message, incoming call) | ❌ | CF Meet only pushes to its own app; production has web push only. The partner must ring its users itself (it gets a `call.incoming` webhook) |
| In-app calls | ⚠️ | Works on native apps. A partner website can't call the calling API directly (CORS); it must go through the partner's server |
| Join a meeting as themselves | ❌ | Only as an anonymous guest, and not from the partner's website (CORS) |
| Meeting chat in a partner's meeting | ❌ | Fails with `CHAT_UNAVAILABLE` |
| Put a ready-made meeting screen in their website or app (iframe/WebView) | ❌ | CF Meet's pages refuse to be framed (`frame-ancestors 'none'`); no embed page exists |
| Ready-made UI components (chat window, call screen) | ❌ | None exist |
| Recordings / transcripts through the API | ❌ | Meeting recording v2 is off, no transcription provider, not in the API |
| SDK languages | ⚠️ | TypeScript and Dart (Flutter) only, unpublished. No React Native, Swift (iOS) or Kotlin (Android) packages. CF Meet's own mobile app is React Native but is not an SDK |

### 1c. Other things found
- **Sign-in for apps:** there is no OAuth, so a third-party app can't act for a CF Meet user or workspace (like "Sign in with Slack/Zoom"). API keys are the only kind.
- **Key restrictions:** keys can't be limited to IP addresses or websites (origins).
- **Abuse limits:** nothing caps how many workspaces or projects one person creates.
- **Data residency:** not mentioned anywhere.
- **Three separate device logins today:**
  - a chat client token, which needs an encrypted device;
  - a calling signaling credential;
  - a meeting invitation or participant token.

  Each has its own rules, which is hard for partners.

## 2. Research: how other platforms do it

There are **two different kinds** of "third-party" platform. Most companies offer one, and the biggest offer both.

**A. "Build with us": put our meetings, chat and calls inside YOUR app** (the partner's users, the partner's brand).
| Platform | Server side | User's device | Ready-made UI | Mobile |
|---|---|---|---|---|
| Zoom Video SDK | App key/secret | Short-lived JWT the partner's server signs, with a host-or-participant role (`role_type`) | UI Toolkit (prebuilt video UI) | iOS, Android, web, Flutter, RN |
| Zoom Meeting SDK | Separate credentials | Signature to join real Zoom meetings | Zoom's own meeting UI embedded | iOS, Android, web |
| Stream (chat + video) | API secret on server | User token minted by partner's server; SDK refreshes it with a "token provider" callback | UI kits per platform | iOS, Android, Flutter, RN, React; CallKit / VoIP push ringing built in |
| Microsoft Azure Communication Services | Identity service | Access token from partner's "trusted service" | "UI Library" composites (whole call or chat screen) | iOS, Android, web; can join Teams meetings as a guest |
| Twilio (Conversations/Video) | Account keys | Access token with "grants" (which products this token may use) | Little | Partner uploads APNs/FCM keys so Twilio can push to the partner's app |
| Sendbird | App API token | Session token per user from Platform API | UIKit | iOS, Android, Flutter, RN |
| Daily | API key | Meeting token with expiry and role | **Daily Prebuilt**: a whole meeting in an iframe in minutes, or a custom UI with the "call object" | Web, RN, iOS, Android |
| Whereby | API key | Room URL / host URL | Whole meeting embedded (web component, iframe, React Native WebView) | Via WebView |
| LiveKit (what CF Meet uses for media) | API key/secret | Access token with room grants | Prebuilt `VideoConference` React component | Swift, Android, Flutter, RN |

**B. "Integrate with us": add your app into OUR product, for our users** (marketplace apps).
| Platform | How |
|---|---|
| Slack | Apps installed into a workspace through OAuth with scopes; bot tokens and user tokens; Events API (to your URL, or over a WebSocket with "Socket Mode"); Block Kit UI inside Slack; app directory |
| Zoom App Marketplace | "General app" with OAuth, installed by an admin (whole account) or by each user; Zoom Apps run inside the Zoom client |
| Google Meet | REST API to create and manage meetings; Add-ons SDK so your app runs inside a Meet call (side panel). **No way to embed Meet in your own app.** Google is shutting down its Meet Media API (raw audio/video) on 15 Jan 2027 |

**The common shape across leaders (type A):**
1. **Server API with a secret key**, server-side only. CF Meet has this.
2. **One short-lived user token** that the partner's server mints for each of its users and the device uses. Twilio puts several "grants" in one token; Zoom has a role. SDKs refresh it automatically. Users are created on the fly.
3. **Three UI levels, per platform:**
   - **Prebuilt:** a whole meeting or chat screen by URL, iframe or WebView;
   - **UI kit:** ready-made components the partner arranges;
   - **Core SDK:** no UI; the partner builds everything.
4. **Mobile push on the partner's behalf.** The partner uploads its own APNs (incl. VoIP) and FCM keys, and the platform rings its users (CallKit on iPhone, full-screen call on Android).
5. **Per-environment settings:** allowed websites (origins), test vs live, webhooks.
6. **Developer experience:**
   - sandbox;
   - test token generator (development only);
   - quickstarts and sample apps for each platform;
   - usage dashboard;
   - pricing per minute / per monthly active user.

## 3. Proposed layout for CF Meet (for discussion)

**CF Meet Platform** with two front doors. Build the first one now; the second later.

```text
                        ┌────────────── Developer Console (console.cfmeet.com) ───────────────┐
                        │ workspaces · apps · test/live · API keys · allowed websites · push   │
                        │ keys (APNs/VoIP/FCM) · webhooks · usage · go-live review · team      │
                        └──────────────────────────────────────────────────────────────────────┘
 Partner's server ──API key──► api.cfmeet.com/v1 ─► Users · Meetings · Chat · Calls · Webhooks · Usage
        │
        └─ mints ONE "CF Meet user token" per user (grants: chat / meeting join + role / calls; ≤1h)
                 │
 Partner's web & mobile app ──user token──► CF Meet SDKs, 3 levels:
     1. Prebuilt   embed.cfmeet.com/… whole meeting / chat screen (iframe on web, WebView on mobile)
     2. UI kit     React · React Native · Flutter components (later SwiftUI · Jetpack Compose)
     3. Core SDK   TypeScript (web) · React Native · Flutter (later Swift · Kotlin): chat, calls, meetings
```

**Front door 1: "Build" (embed in your app).** Main work. It closes every ❌ in 1a and 1b:
- **Users API:** the partner creates and updates its users, or they are created automatically on the first token.
- **One user token:** `POST /v1/users/{id}/tokens` with grants. CF Meet turns it internally into today's three device credentials, so partners learn one thing.
- **Chat:**
  - partner users default to the not-encrypted (MANAGED) mode, so the server can moderate and search;
  - E2EE stays an option per conversation;
  - live chat works in partner browsers and apps.
- **Meetings:** create and run meetings from the server; join with a token that carries the user's name and role; meeting chat; later recordings and transcripts.
- **Calls:** ringing through push on the partner's behalf; works from the partner's website.
- **Per-environment settings:** allowed websites (CORS, WebSocket, iframe permission) and push keys.
- **Prebuilt embed pages** (`embed.cfmeet.com`) that accept the user token. These are the fastest way for partners to get started.

**Also covered:**
- **"Join a CF Meet meeting" (light version):** a partner's user joins a meeting hosted by a CF Meet account, as a named guest with the same token. This is like Zoom Meeting SDK or Teams guest join.
- **Front door 2, "Integrate" (later): CF Meet Apps.** OAuth so outside apps can act for CF Meet users and workspaces, like Slack/Zoom:
  - bots in chat;
  - create meetings from a calendar;
  - apps inside a meeting;
  - an app directory.

**Who builds what:**
| Part | Owner |
|---|---|
| Users API, user tokens, origins and push-key settings, meeting grants, managed chat for partners, metering, go-live | Backend (us) |
| Push sending to partner apps | Backend (us), in the Notification Service |
| Embed pages, UI kits, console screens | Frontend |
| React Native / Swift / Kotlin SDKs | Mobile team |

**Suggested order:**
1. Users API + one user token + allowed websites.
2. Chat and calls on partner devices, plus push.
3. Meetings: server API, join with token, chat, and prebuilt embed page.
4. Go-live review, usage and metering.
5. SDKs published, quickstarts, sample apps, UI kits.
6. Later: CF Meet Apps (OAuth / marketplace).

## 4. Decisions taken (2026-11-18)
- **Scope:** Build and Integrate together.
- **Push:** CF Meet sends with the partner's own keys.
- **Partner chat default:** MANAGED.
- **First UI level:** core SDKs (TypeScript, Flutter, React Native).
- **Still open:** branding, reviewers, pricing and the rest, listed in the plan's "Open owner decisions".

Sources:
- Zoom:
  - [Zoom Video SDK auth (role_type)](https://developers.zoom.us/docs/video-sdk/auth/)
  - [Zoom Video SDK UI Toolkit](https://developers.zoom.us/docs/video-sdk/web/ui-toolkit/)
  - [Zoom forum: Meeting SDK vs Video SDK](https://devforum.zoom.us/t/does-jwt-deprecation-and-transition-to-oauth-affect-video-sdk-web/72120)
  - [Zoom app management](https://developers.zoom.us/docs/build-flow/basic-info/app-mgmt/)
  - [Zoom key concepts](https://developers.zoom.us/docs/platform/key-concepts/)
- Google:
  - [Google Meet SDK & API overview](https://developers.google.com/workspace/meet/overview?authuser=8)
  - [Google Meet release notes (Media API wind-down)](https://developers.google.com/workspace/meet/release-notes?hl=en)
- Slack:
  - [Slack Events API](https://docs.slack.dev/apis/events-api/)
  - [Slack tokens](https://docs.slack.dev/authentication/tokens)
  - [Slack platform overview](https://docs.slack.dev/concepts/overview)
- Stream:
  - [Stream authentication & tokens](https://getstream.io/docs/platform/authentication/)
  - [Stream ringing & CallKit](https://getstream.io/video/docs/flutter/advanced/ringing_and_callkit)
- Microsoft:
  - [Azure Communication Services UI Library](https://learn.microsoft.com/en-us/azure/communication-services/concepts/ui-library/ui-library-use-cases)
  - [ACS Teams interop](https://learn.microsoft.com/en-us/azure/communication-services/concepts/teams-interop)
- Twilio:
  - [Twilio Conversations push (Android)](https://www.twilio.com/docs/conversations/android/push-notifications)
- Daily:
  - [Daily Prebuilt vs call object](https://docs.daily.co/docs/daily-js/concepts/call-modes.md)
  - [Daily meeting tokens](https://docs.daily.co/reference/rest-api/meeting-tokens/create-meeting-token)
- Whereby:
  - [Whereby React Native embed](https://docs.whereby.com/whereby-for-mobile-development/in-react-native)
- Sendbird:
  - [Sendbird Calls authentication](https://sendbird.com/docs/calls/sdk/v1/javascript/features/authentication)
- LiveKit:
  - [LiveKit tokens & grants](https://docs.livekit.io/frontends/reference/tokens-grants/)
