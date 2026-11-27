# ADR 0064 — CF Meet Apps: OAuth, installs, delegated access, bots and install events

**Status:** Accepted (owner decision, 2026-11-18); delivered in phases I1–I5
**Date:** 2026-11-18
**Extends:** [ADR 0061](0061-cf-meet-platform-partner-embedding.md), [ADR 0057](0057-account-sessions-typed-tokens-and-revocation-epoch.md) (account sessions), [ADR 0058](0058-first-party-organization-scopes-and-provisioning.md) (organization scopes).

## Context

The second front door of the CF Meet Platform lets outside apps act for **CF Meet's own users and
workspaces**, the way Slack, Zoom Marketplace and Google Meet add-ons do. Examples:

* a calendar app creates CF Meet meetings for a user;
* a bot answers in a workspace chat.

Today there is no OAuth and no install concept; API keys are the only credential.

## Decisions

1. **No new service.** The work is split along existing authorities:
   * **Platform Access** is the token authority. It holds OAuth clients, grants, authorization codes and tokens:
     * `cfoa_` access tokens, 1 hour;
     * `cfor_` refresh tokens, rotating and single use, where reuse revokes the grant;
     * `cfob_` bot tokens.

     It verifies them live with the same chain as API keys.
   * **The Meeting Service** (the account authority) serves `/api/v2/oauth/authorize`: the user's session, consent relay and "Connected apps".
   * **The Developer Console** handles app registration, `/oauth/token`, `/oauth/revoke`, `/v1/me`, review and the directory.
   * **Organizations** holds the workspace install policy.
2. **OAuth 2.0 authorization code with PKCE S256 only.**
   * Redirect URIs must match exactly; localhost is allowed only for draft apps.
   * Codes last 60 seconds and are single use.
   * Client secrets use the Platform Access credential format.
3. **Closed scope catalogue**, mapped to internal scopes:
   * `profile:read`, `profile:email`;
   * `meetings:read`, `meetings:write`;
   * `chat:read`, `chat:write` (as the user, MANAGED only);
   * `chat:read:bot`, `chat:write:bot`;
   * `events:meetings`.

   First-party-only and operator scopes are never delegable. **Apps never read PRIVATE_E2EE content.**
4. **Installs.**
   * **User install:** a grant for (app, user principal) in the user's personal first-party scope.
   * **Workspace install:** a grant for (app, organization), with a bot principal (`BOT_OR_SERVICE`) and an installation service account in the organization's FIRST_PARTY scope. By default only workspace admins may install.
5. **Delegated access in products.**
   * Gateway: a new auth mode restricts an app token to its user or bot principal.
   * The delegated context gains `acting_application_id`. It is rolled out in two steps: receivers first, then senders.
   * Meeting Service: may act only on the user's own meetings, never join, media, recordings or admin.
   * Messaging: MANAGED conversations only, and messages record the sending app.
6. **Install events** reuse the central webhooks. Routing is per installation, with new `app.*` and `bot.*` event types.
7. **Uninstall** revokes the grant and every token at once, and deactivates the bot.

## Consequences

* The consent page, Connected apps, the directory and workspace admin screens are frontend work.
* Open owner decisions:
  * review policy;
  * the draft test-user limit;
  * whether signing out everywhere revokes app grants.
