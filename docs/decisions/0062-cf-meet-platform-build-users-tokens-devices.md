# ADR 0062 — CF Meet Platform "Build": partner users, the user token, device exchange, allowed websites

**Status:** Accepted (owner decision, 2026-11-18); delivered in phases B1–B8, each behind its own flag and activation
**Date:** 2026-11-18
**Extends:** [ADR 0061](0061-cf-meet-platform-partner-embedding.md) (names, traffic model). Plan: [`docs/platform/cf-meet-platform-integration-plan.md`](../platform/cf-meet-platform-integration-plan.md).

## Context

Partners (for example a delivery app whose riders call customers) put CF Meet chat, calls and
meetings in their own web and mobile apps, for their own users, who have no CF Meet account.
Leading providers (Zoom Video SDK, Stream, Twilio, Sendbird, Daily, Azure Communication Services,
LiveKit) share one shape:

* a secret key on the partner's server;
* partner-created users;
* **one short-lived user token** per user, minted by the partner's server, which the device SDK uses and refreshes;
* allowed websites per environment;
* pushes sent with the partner's own keys.

Today CF Meet has the server side, but none of the rest:

* partner users can only be created by an operator;
* devices need three different credentials, with different rules;
* the simple chat device login is first-party only;
* partner websites are refused by CORS and origin checks.

## Decisions

1. **Partner users are Platform Access principals.**
   * Kind `EXTERNAL_APPLICATION_PRINCIPAL`, in the partner's environment.
   * The Developer Console serves the Users API (`/v1/users`). It is idempotent on the partner's `externalId`, which must be an opaque id, never a phone number.
   * The Developer Console delegates to a dedicated Platform Access router. Platform Access itself never receives public traffic.
   * Profiles hold a display name and image. Caps apply per environment.
2. **One CF Meet user token** (`cfut_<sbx|test|live>_<JWS>`).
   * Signed by Platform Access with its own Ed25519 key.
   * Minted for one user by the partner's server, with scope `user_tokens.issue`.
   * Grants: `msg`, `call` and `mtg`. They are a subset of the minting key's scopes, re-intersected on every verification.
   * One hour by default, and stateless. It dies when:
     * the minting key is revoked;
     * the user is suspended or deleted;
     * `revoke-sessions` advances the user's epoch;
     * the environment is suspended;
     * the signing key is retired.
   * Product services verify it live with Platform Access (a short positive cache is allowed).
3. **Devices exchange the user token** for the credentials each product already issues:
   * Messaging: a partner session device and client token;
   * Calling: an endpoint and a signaling credential;
   * Meeting Service: a participant token and a media credential.

   Derived credentials keep the minting key id, so existing "minting credential still valid" checks keep working, and revoking the partner's key ends them all.
4. **Allowed websites per environment.**
   * Platform Access holds exact HTTPS origins (localhost only in test environments), managed from the Developer Console.
   * The gateway answers CORS for a partner origin only on routes that accept a device login, never on API-key-only routes, so API keys never belong in browsers.
   * Messaging checks the same list on tickets and WebSocket upgrades.
   * Partner device traffic uses `api.cfmeet.com`; media stays direct.
5. **Partner chat defaults to MANAGED** (server-readable: search, moderation, scanning, exports).
   * Partner devices get MANAGED-only session devices with a closed REST allowlist, so they can send as well as receive.
   * PRIVATE_E2EE stays available per conversation, through MLS devices.
6. **Calls stay server-placed.** The partner's server decides who may call whom: for example, only while a delivery is active.
7. **Meetings.** A user joins under their own identity: the role comes from the grant, the name from the profile, and a re-join keeps the same participant.
8. **Every phase is off by default.** Each is enabled by a journaled activation whose post-checks prove the milestone with two fresh tenants, including cross-tenant `404`s.

## Consequences

* CF Meet's own apps are unchanged; they stay on their direct `/api/v2` ingress.
* New usage facts (`MESSAGE_SENT`, `ACTIVE_USER_DAY`) make chat and monthly active users meterable.
* The frontend owns every console screen for these features.
