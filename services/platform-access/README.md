# CF Meet Platform Access

The shared identity authority of CF Meet (Part IV Workstream B,
[ADR 0032](../../docs/decisions/0032-platform-access-shared-identity-authority.md)).
It owns, in its own PostgreSQL database:

* the hierarchy **Tenant → Application → Environment → Service Account → API Credential**,
  first-party / external classification, suspension and revocation;
* **API credentials**: issuance, rotation with overlap, revocation, and verification —
  `cfmk_<sbx|test|live>_<24 hex>_<64 hex>`, HMAC-SHA-256 verifiers under the pepper (the
  Part III construction, unchanged; the raw secret is returned once and never stored);
* the **scope registry** — Calling scopes, `numbers.search | read | manage | assign |
  kyc.read | kyc.manage | portability`, `sms.send | read`, the Part V Messaging scopes `messages.send | read`,
  `conversations.create | manage`, `attachments.write | read`, `moderation.manage`,
  `messaging.export` (STANDARD) and `numbers.admin` / `messaging.admin`
  (PRIVILEGED_OPERATOR: an explicit audited operator grant to a FIRST_PARTY application
  only) — with optional per-environment-kind restriction;
* **principals** (opaque references only) and their tombstones;
* the **failed-authentication throttle**, the commit-ordered **identity change feed**
  consumers project from, Part III **import receipts** and an append-only **operator
  audit**.

It exposes no customer API. Consumers call the service-authenticated `/internal/v1` API
(`packages/service-auth`; contract `cfmeet.platform-access.internal-api`, messages
`cfmeet.platform-access.identity`):

| Route | Permission | Consumer |
|---|---|---|
| `POST /internal/v1/credentials/authenticate` | `platform-credential.authenticate` | Calling, Numbering, SMS Gateway, Messaging |
| `GET /internal/v1/credentials/{id}` (delegated) | `platform-identity.read` | Calling, Numbering |
| `POST /internal/v1/authorization/evaluate` (delegated) | `platform-identity.read` | Calling, Numbering |
| `GET /internal/v1/identity/context` (delegated) | `platform-identity.read` | Calling, Numbering |
| `POST /internal/v1/identity/changes` | `platform-identity.read` | Calling (projection), Messaging (cache invalidation) |
| `GET /internal/v1/identity/imports/{digest}` | `platform-identity.read` | Calling (hand-off) |
| `GET /internal/v1/scopes` | `platform-identity.read` | any consumer |
| `POST /internal/v1/principals/resolve` (delegated) | `platform-principal.resolve` | Calling, Numbering, SMS Gateway, Messaging |
| `POST /internal/v1/principals/telephone-endpoints` (delegated) | `platform-principal.register` | Calling |
| `POST /internal/v1/principals/{id}/tombstone` | `platform-principal.register` | Calling |
| `/internal/v1/admin/**` (+ `X-CF-Operator`) | `platform-identity.administer` | operator console |

"Delegated" operations require the signed delegated platform context; Platform Access
re-validates its tenant / application / environment / service account against its own
rows and refuses anything inconsistent (`DELEGATED_CONTEXT_INVALID`). A tenant id in a
request never becomes authority.

## Running locally (no Docker required)

```bash
cp .env.example .env     # DATABASE_URL, API_CREDENTIAL_PEPPER, CF_SERVICE_* (see comments)
make platform-access-migrate
make platform-access     # listens on 127.0.0.1:8094
curl http://127.0.0.1:8094/ready
```

## Administration

`platform-access-admin` (JSON on stdout; every mutation needs `--operator` /
`CF_OPERATOR_ID` and writes one audit row):

```bash
export CF_OPERATOR_ID=ops.alice
A="cargo run -q --bin platform-access-admin --"
$A tenant create --slug acme --name "Acme" --classification external
$A application create --tenant acme --slug dispatch --name "Dispatch"
$A environment create --tenant acme --application dispatch --environment production
$A service-account create --tenant acme --application dispatch --environment production --name backend
$A scope grant --tenant acme --application dispatch --scope calls.read [--environment-kind production]
$A scope grant-privileged --tenant cosmicforge --application numbering-console --scope numbers.admin
$A credential create --tenant acme --application dispatch --environment production \
   --service-account backend --scope calls.read           # prints the secret ONCE
$A credential rotate --credential <public id> --overlap-minutes 60
$A credential revoke --credential <public id>
$A principal register --tenant acme --application dispatch --environment production \
   --kind external-application-principal --reference user-123
$A identity import --bundle part-iii.json                 # hand-off from Part III Calling
$A identity export --out recovery.json                    # 0600; verifiers, never secrets
```

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make platform-access-test
```

Suites: `authentication` (Part III continuity, every rejection reason, rotation,
expiry, revocation, safety-only authority, throttling, pepper rotation),
`credential_storage`, `scopes` (registry, first-party / external, privileged grants,
environment-kind restriction), `isolation` (cross-tenant / application / environment,
forged delegated context, internal authorization), `messaging_scopes` (Part V B), `change_feed`, `handoff`
(Part III import), `admin_cli`, `migrations`, `contracts` (OpenAPI = routes, responses
conform). Calling's suite runs against this service's real binaries.
