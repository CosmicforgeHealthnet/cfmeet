# API Gateway scope pre-check (`x-cfmeet-required-scope`)

Section 15, finding PLT-011.

## What the gateway checks

When a request on a gateway route is authenticated with a platform credential
(`Authorization: Bearer cfmk_…`), the gateway compares the credential's scopes with
the route's `requiredScope`. If the scope is missing, the gateway answers
`403 INSUFFICIENT_SCOPE` and the request never reaches the upstream
(`services/api-gateway/src/http/ingress.rs`).

The route's `requiredScope` comes from the source contract's operation extension
`x-cfmeet-required-scope`. `scripts/contracts/public_api.py` copies it into
`infra/api-gateway/route-manifest.json`. The generator rejects any scope that is not in
the Platform Access registry (`services/platform-access/src/identity/scopes.rs`), so a
gate scope can never lock out every credential.

The pre-check is defence in depth: it rejects bad requests early and gives SDK users a
scope hint. It does not replace the upstream check. Every upstream still enforces its
own scopes and keeps final resource authorization. The gateway checks only
platform-credential requests. Client tokens, meeting sessions and other end-user tokens
on the same routes go to the upstream unchanged.

## Rule

A route gets a gate scope only when its upstream handler requires **exactly one**
registry scope on every request. That scope must be a necessary condition, so the
gateway can never refuse a request the upstream would accept. If the upstream accepts
any one of several scopes, picks the scope per request, or needs no scope at all, the
route has no gate scope. The upstream checks it. Those routes are listed below.

## Coverage (after PLT-011)

| Domain | Credential routes | With gate scope | Upstream-checked |
|---|---:|---:|---:|
| meetings | 21 | 21 | 0 |
| calling | 119 | 119 | 0 |
| developer-platform | 51 | 51 | 0 |
| messaging | 94 | 90 (new in PLT-011) | 4 |
| numbering | 52 | 51 (new in PLT-011) | 1 |
| sms | 6 | 6 (new in PLT-011) | 0 |
| privacy-relay | 17 | 16 | 1 |
| gateway | 1 | 0 | 1 |
| **Total** | 361 | 354 | 7 |

The gate scopes match the handlers' own checks:

- messaging: `require_scope`, `acting_with`, `administration` and `administrator` in
  `services/messaging/src/**/http.rs`, `usage.rs` and `backups.rs`;
- numbering: `CustomerCaller::require_scope` in `services/numbering/src/http/*.rs`;
- sms: `require_scope(SMS_READ | SMS_SEND)` in `services/sms-gateway/src/http/customer.rs`.

## Routes left to the upstream on purpose

| Route | Operation | Why there is no gate scope |
|---|---|---|
| `GET /v1/conversations` | `messaging.listConversations` | Picked per request: `messages.read` with a `CF-Acting-Principal`, otherwise `conversations.manage` (`conversations/http.rs` `reader`). |
| `GET /v1/conversations/{conversationId}` | `messaging.getConversation` | Same per-request choice (`reader`). |
| `GET /v1/conversations/{conversationId}/members` | `messaging.listMembers` | Picked per request: `messages.read` with an acting principal, otherwise `conversations.manage` (`membership/http.rs` `list`). |
| `DELETE /v1/conversations/{conversationId}/members/{principalId}` | `messaging.removeMember` | A member leaving needs `messages.send`. Removing someone else needs `conversations.manage` (`membership/http.rs` `remove`). |
| `GET /v1/numbers/catalog/countries/{countryCode}` | `numbering.getCatalogCountry` | Accepts either scope: `numbers.search` or `numbers.read` (`numbering/src/http/catalog.rs` `catalog_country`). |
| `POST /v1/privacy-relay/contact-access-grants` | `privacy-relay.createContactAccessGrant` | Set per request: every `privacy.contacts.*` scope named in the body must be held. Calling enforces this (PLT-001). |
| `GET /v1/developer-platform/context` | `gateway.getDeveloperPlatformContext` | No scope needed. Any live credential can read its own delegated context, and the Developer Platform re-authenticates the credential (`developer-platform/src/http/internal.rs` `gateway_context`). |

## Changing a route

- When you add a credential operation to a source contract, add
  `x-cfmeet-required-scope` if, and only if, the rule above holds. Use the same scope
  constant the handler checks.
- If a handler's scope logic changes (for example, it starts accepting either of two
  scopes), remove the extension in the same change and add the route to the table above.
- After either change, run `python scripts/contracts/public_api.py` and
  `python sdks/generate.py`, then commit the regenerated manifest, bundles and SDK
  metadata.
