# ADR 0046 — Platform Access: Messaging scopes and the device-association authority split

Status: Accepted (Part V Workstream B). Builds on ADR 0032 (Platform Access) and ADR 0045.

## Decisions

1. **Scopes.** The Part V Messaging scopes are registered in Platform Access's scope
   registry: `messages.send`, `messages.read`, `conversations.create`,
   `conversations.manage`, `attachments.write`, `attachments.read`, `moderation.manage`,
   `messaging.export` (STANDARD) and `messaging.admin` (PRIVILEGED_OPERATOR — only an
   explicit, audited operator grant to a FIRST_PARTY application, honoured only with that
   basis; never a customer grant). No scope implies another. Grants are explicit at both
   levels (application allowance, then credential): existing credentials gain nothing,
   no migration or reissue is needed, and environment-kind restrictions, suspension and
   revocation apply exactly as to every other scope.
2. **Messaging's internal access** (packages/service-auth registry): Messaging may be
   granted `platform-credential.authenticate`, `platform-principal.resolve` and
   `platform-identity.read` — nothing else (no principal registration / tombstone, no
   identity administration, nothing sensitive). No service may call Messaging internally.
3. **Device association authority split.** No new Platform Access endpoint was needed:
   Platform Access already returns, freshly on every call, the authority chain
   (tenant / application / environment / service account / credential, authority mode,
   effective scopes) and the credential's authorized-principal restriction, and resolves
   principals ONLY inside the signed delegated scope it re-validates against its own rows.
   Messaging composes these into device authorization: authenticated credential → scope
   → restriction admits the principal → principal ACTIVE and messaging-capable in that
   environment → single-use server challenge → proof of possession (ADR 0045 decision 7)
   → durable, revocable Messaging device. Platform Access owns principals and
   credentials; Messaging owns device PUBLIC keys. Platform Access stores no MLS key of
   any kind (repository guard `MESSAGING_CRYPTO_ELSEWHERE` / `E2EE_SECRET_IN_SCHEMA`).
4. **Revocation and caching.** Security-critical Messaging operations re-authenticate and
   re-resolve the acting principal on every request. Reads may reuse a positive
   authentication for at most 60 s (configurable, default 10 s), flushed by ANY entry of
   the commit-ordered identity change feed and disabled whenever the feed cannot be read;
   Platform Access unreachable is never an offline bypass (requests fail closed).

## Evidence

`services/platform-access/tests/messaging_scopes.rs` (real router + PostgreSQL): trust
classes, least privilege, no implicit grants to existing credentials, privileged
`messaging.admin`, environment restriction / suspension / revocation, cross-tenant /
cross-environment / forged-context principal resolution, principal restrictions,
Messaging's missing registration / administration rights and forged service tokens.
