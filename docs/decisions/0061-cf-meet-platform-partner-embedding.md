# ADR 0061 — CF Meet Platform: partner embedding of meetings, messaging and calling

**Status:** Accepted (owner decision, 2026-11-18); delivered in phases, each behind its own flag and activation
**Date:** 2026-11-18
**Extends:** [ADR 0055](0055-developer-platform-and-api-gateway-foundation.md) (Developer Platform and API Gateway foundation), [ADR 0049](0049-messaging-realtime-gateway.md) amendment A1 (session devices), [first-party direct-ingress decision](../architecture/first-party-direct-ingress-decision.md) (FE-018).

## Context

Other companies (partners) want to put CF Meet's meetings, chat and calls inside their own
products. The building blocks exist:

* the API Gateway (`api.cfmeet.com/v1`): default deny, scoped API keys, quotas, usage;
* Platform Access: tenants, applications, environments, service accounts, keys, scopes and principals;
* the Developer Platform control plane: workspaces, projects, keys, webhooks, usage, production access;
* the product services: Messaging, Calling and the Meeting Service.

An audit on 2026-11-18 found that a partner cannot yet use them end to end:

* a partner cannot create the end users of its own product (principals are operator-only);
* meetings are closed to API keys, and a partner cannot let its users join;
* messaging on end-user devices needs E2EE devices, which are off in production;
* production access is not open;
* calling and realtime traffic is not metered centrally.

The naming also confuses. "Developer Platform" names only the control plane. "Developer
organizations" collide with CF Meet's own business Organizations. And the CF Meet app's
Communications Hub was listed as a public partner API although only the app's own signed-in
accounts can call it.

## Decisions

1. **Names.**
   * **CF Meet Platform** is the product partners buy: CF Meet meetings, messaging and calling inside their own product. It spans the API Gateway, Platform Access, the product services and the SDKs.
   * **Developer Console** is its control plane, the service `services/developer-platform`.
   * A partner's account in the Developer Console is a **workspace**. Paths and fields keep the name *organization* (`/v1/organizations`, `organizationId`): renaming them would break existing clients, so the rename is in text only.
   * Service, crate and database names are unchanged.
2. **Traffic model.**
   * A partner's **backend** calls `https://api.cfmeet.com/v1/...` through the API Gateway with a scoped API key (`PLATFORM_CREDENTIAL`). Quotas, usage and scope checks apply there.
   * A partner's **end-user devices** never hold the API key. The partner's backend obtains short-lived, narrowly bound credentials for them, which the devices use on the existing direct ingress (realtime, media, signaling):
     * a messaging session-device token;
     * a meeting participant grant;
     * a calling signaling credential.
   * **CF Meet's own apps** stay on their direct `/api/v2` ingress (FE-018); they do not move onto the gateway.
3. **The Communications Hub is first-party only.** Its operations (`/api/v2/communications/*`, account session only) are classified `EXCLUDED` with reason `FIRST_PARTY_COMMUNICATIONS_HUB` in `infra/api-gateway/public-routes.json`. They leave the gateway route table, the public API documents and the SDKs. The Hub itself is unchanged.
4. **Webhooks.** The machine-key webhook API stays at Calling's public paths (`/v1/webhooks*`, `/v1/webhook-deliveries/{id}/replay`). The gateway re-points them to the central Developer Console webhooks at the cut-over (`API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`). They are not deprecated: the paths are the permanent public API.
5. **Delivery in phases, each one off by default.** Every phase has its own flag and its own `coordinated_release.py` activation, which is journaled and can be rolled back. Every activation's post-checks include a second tenant.
   * **0.** Names, accurate documentation, catalogue cleanup (this ADR).
   * **1.** Partners create their own end users (principals) with `principals.write` in their environment, within caps.
   * **2.** Partner end-user devices get MANAGED (non-E2EE) realtime session devices.
   * **3.** Meetings: the `/v1/meetings` gateway family; single-use participant grants bound to a principal; read-only recordings and transcripts.
   * **4.** Production-access review is opened by named reviewers.
   * **5.** Usage: messages sent and calling operations.
   * **6.** SDK helpers, quickstarts and publishing.
6. **Isolation.**
   * The environment always comes from the credential.
   * Another tenant's principal, space or conversation answers `404`.
   * Grants and session devices re-check their principal on use.
   * Telecom (numbers, SMS, Privacy Relay, PSTN) stays off by owner decision.

## Consequences

* The public route count falls from 602 to 528. No partner could use the removed routes: they needed a CF Meet account session.
* The first-party direct-ingress record lists the Hub operations under `FIRST_PARTY_COMMUNICATIONS_HUB`.
* The frontend owns every console and partner-facing UI. This work changes no web or mobile code.
* Later phases may extend this ADR with amendments; owner decisions are recorded as they are made.
