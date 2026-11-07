# Frontend handoff: Contact Vault (`/v1/privacy-relay/contacts*`) upstream (CAL-013)

**Finding:** CAL-013 (P2, SHARED_CONTRACT): "Privacy Relay not deployed; web admin pages proxy
`/v1/privacy-relay/contacts*` to Calling, which does not serve them". Blueprint v1.0 Section 16.
**Backend disposition:** `FRONTEND_HANDOFF`. The contract, the gateway manifest and the routing
gate are already correct (verified below), so there is no backend routing change. Deploying
Privacy Relay, and deciding whether its Contact Vault routes get an ingress, is an owner
decision (`EXTERNAL`). Nothing was deployed.
**Recorded:** 2026-11-07, branch `blueprint/s16-retirement-tooling`.

## 1. Who serves what

| Operations | Served by | Authentication | Gateway class |
|---|---|---|---|
| `/v1/privacy-relay/contexts*`, `/v1/privacy-relay/sessions*` (15 operations) | **Calling** (`services/calling/src/privacy_relay/http.rs`). Calling is the authenticated platform ingress for the Privacy Relay API and forwards to Privacy Relay over signed internal calls | `Authorization: Bearer cfmk_…` platform credential, `privacy.contexts.*` / `privacy.sessions.*` scopes | `PUBLIC_CUSTOMER`, upstream `cfmeet.calling` |
| `POST /v1/privacy-relay/contact-access-grants` | **Calling** (`services/calling/src/privacy_relay/contacts.rs`). It mints a short-lived Contact Vault access token signed with Calling's dedicated contact-delegation key | platform credential holding every requested `privacy.contacts.{read,write,verify}` scope | `PUBLIC_CUSTOMER`, upstream `cfmeet.calling` |
| `POST /v1/privacy-relay/contacts`, `GET`/`PATCH …/contacts/{contact_reference}`, `POST …/{contact_reference}/disable`, `POST …/{contact_reference}/verification` | **Privacy Relay** (`services/privacy-relay/src/contact_vault/http.rs`), contract paths marked `x-cfmeet-served-by: cfmeet.privacy-relay` | **only** `X-CF-Contact-Authorization: Bearer <Contact Vault access token>` (typ `cf-contact-access+jwt`, at most 300 s old). An `Authorization` header, a service token or another audience's token is refused with `401 CONTACT_DELEGATION_INVALID`. A missing contact scope returns `403 CONTACT_SCOPE_DENIED` | `EXCLUDED` / `CONTACT_VAULT_PRIVATE`: never routed by the gateway |

Calling has no `/v1/privacy-relay/contacts*` route. A request proxied there gets Calling's
404, and a request carrying the platform credential is refused by the Contact Vault on
purpose. The Contact Vault routes are the only CF Meet routes that accept a raw telephone
number, so a telephone number never transits Calling or the gateway.

### Verified on this branch

* `packages/contracts/privacy-relay/http/v1.openapi.json`: the info description and the
  `createContactAccessGrant` operation describe the flow above.
* `infra/api-gateway/public-routes.json` and the `route-manifest.json` catalogue classify the five
  contact operations `EXCLUDED` / `CONTACT_VAULT_PRIVATE`. The 17 routable `privacy-relay`
  routes all have upstream `cfmeet.calling` and `PLATFORM_CREDENTIAL` auth.
* `python3 scripts/contracts/check_gateway_upstreams.py`: every routable gateway route resolves to
  a Calling handler. The contact operations aren't routable, so they aren't checked against
  Calling.
* `python3 scripts/contracts/first_party_ingress.py --check`: the
  [first-party ingress decision record](../architecture/first-party-direct-ingress-decision.md)
  lists them under `CONTACT_VAULT_PRIVATE`.
* Swagger: Calling's `/swagger-ui/` used to list the contact operations it doesn't host. That
  is PLT-006, handled separately in Section 16.

## 2. What the web app does today (read-only findings; `apps/` is not changed by the backend)

| File | Problem |
|---|---|
| `apps/web/src/app/api/calling/[...path]/route.ts` (`privacyRelayRules`, lines ~49–52) | allows `POST v1/privacy-relay/contacts`, `GET`/`PATCH v1/privacy-relay/contacts/{id}` and `POST …/{id}/disable` and forwards them through `callingFetch` to `CALLING_API_URL` with `Authorization: Bearer ${CALLING_PLATFORM_CREDENTIAL}`. That is the wrong upstream and the wrong credential type |
| `apps/web/src/lib/calling/resource-routes.ts` (`"privacy/contacts"`, lines ~46–56) | the "Protected contacts" admin page uses `createPath: "/v1/privacy-relay/contacts"` and `detailPath: "/v1/privacy-relay/contacts/{id}"` through that proxy |
| same proxy, `v1/privacy-relay/contact-access-grants` rule | correct upstream (Calling), but the web never uses the token it returns |

Contexts and sessions in the same files go to the right upstream (Calling).

`scripts/contracts/web_contract_parity_report.py` (Contracts workflow, report-only) flags these
calls as `CONTACT_VAULT_PRIVATE` until they change.

## 3. Correct client flow (once Privacy Relay is deployed with a reachable Contact Vault ingress)

All of this stays **server-side** in the web app's route handler. The browser never holds the
platform credential or a Contact Vault token.

1. The server-side adapter authorizes the signed-in principal as today
   (`CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS`, deny by default).
2. `POST {CALLING_API_URL}/v1/privacy-relay/contact-access-grants` with the platform credential,
   body `{"scopes": ["privacy.contacts.write"]}` (or `read` / `verify` as needed; ask only for
   what the operation needs). Response `201 {"grant": {grantId, accessToken, tokenType,
   authorizationHeader, audience, scopes, expiresAt}}` carries the token once;
   `Cache-Control: no-store`. The credential must itself hold the scopes, otherwise
   `403 INSUFFICIENT_SCOPE`. Calling answers 503 while `CONTACT_DELEGATION_KEY_ID` /
   `CONTACT_DELEGATION_PRIVATE_KEY_FILE` aren't configured.
3. Call the Contact Vault on the **Privacy Relay** base URL (a new server-only variable, for
   example `PRIVACY_RELAY_CONTACT_VAULT_URL`, https in production) with
   `X-CF-Contact-Authorization: <grant.authorizationHeader>` (`Bearer <token>`) and **no**
   `Authorization` header. Send `expectedVersion` where the contact schema requires it
   (`PATCH`, `disable`).
4. Never cache or log the token or the request body (it contains `telephoneNumber`). Responses
   never return a telephone number; show the masked fields only.
5. Map `401 CONTACT_DELEGATION_INVALID` to "mint a new grant and retry once", and
   `403 CONTACT_SCOPE_DENIED` / `INSUFFICIENT_SCOPE` to a permission message.

## 4. Until Privacy Relay is deployed (now)

* Privacy Relay is built (`docker build -f services/privacy-relay/Dockerfile .`) but not
  deployed. Its listener is documented as cluster-internal only (`infra/deployment/README.md`),
  and the web app runs on Vercel. Exposing the Contact Vault routes (host, TLS, network policy)
  is part of the owner's deployment decision. It isn't something the frontend can configure.
* Frontend, short term: hide the "Protected contacts" page, or show it as "not available", and
  remove the four `v1/privacy-relay/contacts*` rules from the Calling proxy allowlist, so the
  app stops sending them to Calling. The contexts and sessions pages can stay (they are
  served by Calling, which reports `503 PRIVACY_RELAY_DISABLED` while the feature is off).
* Owner, at release: deploy Privacy Relay; decide the Contact Vault ingress; configure Calling's
  contact-delegation key and the Privacy Relay trust for it; give the web project's platform
  credential the `privacy.contacts.*` scopes only if the admin page is wanted. Then the frontend
  applies §3.

## 5. Not changed by the backend

No contract, manifest, gateway or deployment change was needed or made for CAL-013. The
`api.cfmeet.com` upstreams, Vercel configuration and `apps/` are untouched.
