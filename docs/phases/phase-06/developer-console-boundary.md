# Developer Console integration boundary (Part VI §5)

The Developer Console is the **existing** route `apps/web/src/app/dashboard/developers/`
(`page.tsx` → `components/dashboard/DevelopersContent`). Part VI creates no other frontend
application and does not change this route in §§1–5.

## Boundary

| Concern | Rule |
|---|---|
| Transport | The console calls the **published** API Gateway contract (`cfmeet.api-gateway.http-api`) exactly like any customer — never `services/developer-platform` directly, never an `/internal` path, never Platform Access administration. |
| Identity | Credentials and their scopes are issued and verified by Platform Access. The console never stores a credential verifier and never derives tenant / application / environment from its own state; it displays what the gateway returns (canonical Platform Access ids). |
| Authorization | Domain services and the Developer Platform keep final resource authorization; the console hides nothing it relies on for security. |
| Contracts | Types come from `sdks/typescript` (`@cfmeet/platform-sdk`, generated from the published contracts). First-party CosmicForge products and external customers use the same contracts; they differ only by scopes and trust classification. |
| Operator tooling | Operator administration (Platform Access `platform-identity.administer`, operator console identity) is not reachable through the console or the customer ingress. |
| Rollout | Disabled by default. Enabling requires `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true` on the gateway and `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=true` on the Developer Platform; a console-side flag (planned `NEXT_PUBLIC_DEVELOPER_CONSOLE_PLATFORM_ENABLED`, default `false`) is introduced together with the Section 50+ console work so that `apps/web` and its Vercel deployment are untouched now. |

## First integration (foundation route)

`GET /v1/developer-platform/context` (Bearer credential) → the canonical context of the
credential's application environment (`cfmeet.developer-platform.common` `GatewayContext`).
Everything else the console needs (projects, keys, usage, webhooks, logs) is Sections 50+.
