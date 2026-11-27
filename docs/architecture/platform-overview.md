# CF Meet platform overview

**CF Meet, CosmicForge Tech Limited** · updated 2026-11-18 ([ADR 0061](../decisions/0061-cf-meet-platform-partner-embedding.md))

CF Meet is two things built on one set of services:

1. **The CF Meet apps** (web, mobile): CosmicForge's own meetings, messaging and calling product
   for signed-in CF Meet accounts.
2. **The CF Meet Platform**: the same meetings, messaging and calling, offered to other companies
   (partners) to put inside **their** products, through one API and SDKs.

## Names

| Name | What it is | Where |
|---|---|---|
| **CF Meet Platform** | The partner offering: CF Meet meetings, messaging and calling in a partner's product | API Gateway + Platform Access + product services + SDKs |
| **Developer Console** | The control plane where a partner manages its workspace, projects, environments, service accounts, API keys, scopes, webhooks, usage, limits and production access | `services/developer-platform` |
| **Workspace** | A partner's account in the Developer Console. Paths and fields still say *organization* (`/v1/organizations`, `organizationId`). Not CF Meet business Organizations | Developer Console, EXTERNAL Platform Access tenant |
| **API Gateway** | The single front door for partner backends, `https://api.cfmeet.com/v1` | `services/api-gateway` |
| **Platform Access** | The only identity, credential and scope authority (tenants, applications, environments, service accounts, keys, principals) | `services/platform-access` |
| **Principal** | An end user inside a partner's product (or CF Meet's own app user, `NATIVE_CF_USER`) | Platform Access |

## How a partner connects

```text
Partner backend ──API key (scoped)──► API Gateway  api.cfmeet.com/v1
                                        │  default deny · key + scope check · quotas · usage
                                        │  60 s signed context (the key is never forwarded)
                                        ▼
                  Messaging · Calling · Meetings · Developer Console
                                        ▲
Partner end-user device ──short-lived, bound credential──► direct realtime / media / signaling ingress
  (messaging session-device token, meeting participant grant, calling signaling credential;
   minted by the partner backend, never the API key)
```

CF Meet's own apps do not use the gateway: they call the services on their direct `/api/v2`
ingress with the CF Meet account session ([first-party direct-ingress decision](first-party-direct-ingress-decision.md)).

## Services

| Service | Path | Responsibility |
|---|---|---|
| Meeting Service | `services/meeting-service` | CF Meet accounts and sign-in (email + code), meetings and conferences, LiveKit media tokens, meeting chat; first-party relays to Organizations, Notifications and Messaging |
| Messaging | `services/messaging` | Conversations, messages (MANAGED and E2EE), realtime, attachments, search, moderation |
| Calling | `services/calling` | App-to-app voice / video calls, signaling, enterprise calling, the public-network side of calls |
| Communications | `services/communications` | The CF Meet app's Communications Hub (contacts, CF Meet ID, timeline); first-party only |
| Organizations | `services/organizations` | CF Meet's first-party business organizations |
| Notifications | `services/notifications` | Email, push and in-app notifications |
| Platform Access | `services/platform-access` | Identity, credential and scope authority |
| Developer Console | `services/developer-platform` | Partner control plane (see Names) |
| API Gateway | `services/api-gateway` | Partner ingress (see Names) |
| Privacy Relay | `services/privacy-relay` | Communication contexts, protected contacts, relay numbers |
| Telephony | `services/telephony` | Public-network call execution, CF-owned SIP / media edge |
| Numbering · SMS Gateway | `services/numbering`, `services/sms-gateway` | Numbers and SMS |
| Security scan gateway | `services/security-scan-gateway` | Attachment scanning |

Each service owns its own database; no service reads another's tables. Shared code lives in
`packages/` (contracts, service authentication, webhooks, usage facts, messaging crypto, …).

## What a partner can do today

| Capability | Status |
|---|---|
| Sign up, create projects, environments, service accounts, API keys, webhooks (SANDBOX / TEST) | Available |
| Server-to-server messaging on MANAGED conversations | Available |
| In-app (WebRTC) calling | Available |
| Create its own end users (principals) | Phase 1 of ADR 0061 |
| Live chat on its users' devices (MANAGED session devices) | Phase 2 |
| Meetings with an API key, participant grants, recordings | Phase 3 |
| Production access | Phase 4 (owner names the reviewers) |
| Usage for messages and calling | Phase 5 |
| Published SDKs and quickstarts | Phase 6 (SDKs are generated, unpublished) |
| Numbers, SMS, Privacy Relay, public-network calls | Off by owner decision |

## Contracts

`packages/contracts` holds every API, event and webhook contract. The gateway route table
(`infra/api-gateway/route-manifest.json`), the public API documents
(`packages/contracts/bundled/public/`) and the SDK sources (`sdks/`) are generated from those
contracts plus the explicit allow-list `infra/api-gateway/public-routes.json`
(`scripts/contracts/public_api.py`, `sdks/generate.py`; CI checks they are current).

## Infrastructure

PostgreSQL, Redis and LiveKit; the SIP / media edge in `infra/telephony`; production deployment
and acceptance tooling in `infra/calling/production` (coordinated release, activations,
certification); per-service metrics and incidents.
