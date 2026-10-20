# ADR 0047 — Messaging service foundation, device registration and the key-transparency log

Status: Accepted for the foundation (Part V Workstream C). Builds on ADR 0045 / 0046.

## Decisions

1. **An independent service.** `services/messaging` (`cfmeet.messaging`, port 8097): its
   own Cargo workspace, migration history (`messaging_seaql_migrations`), database, roles
   (`provision-service-database.sh --service messaging`, isolation certified by
   `verify-service-isolation.sh`), image, CI workflow and release. No automatic deployment;
   never inside the Meeting Service deployment. Only `messaging_*` tables.
2. **Routing separation.** Public probes; `/v1/**` customer routes (Platform Access
   credential, scope, verified acting principal); `/internal/v1/**` service-authenticated
   (empty trust bundle — no internal operation); metrics on a separate listener. A
   service token never authenticates a customer route and vice versa. Client tokens are
   refused explicitly until their lifecycle exists — there is no fake or permanent token.
3. **Fresh authorization for security-critical operations.** Every mutation
   re-authenticates the credential with Platform Access and re-resolves the acting
   principal; reads may use the bounded, feed-invalidated cache (ADR 0046).
4. **Device registration** = single-use challenge (server nonce, server-assigned device
   id, bound to credential + principal + environment, ≤ 1 h, rate-limited) answered by a
   registration KeyPackage whose credential identity and signed `application_id` bind it
   to the challenge (ADR 0045 decision 7). One transaction consumes the challenge,
   enforces the active-device limit under a per-principal advisory lock, stores the
   PUBLIC key (unique for ever), appends a transparency leaf, writes audit and outbox.
   The same answer replayed returns the same device; anything else is `CHALLENGE_INVALID`.
5. **KeyPackages**: validated (ADR 0045 decision 6), bound to the device and its key,
   never last-resort or registration packages, unique by KeyPackageRef (re-submission is
   `ALREADY_PUBLISHED`, never a second acceptance), bounded per device, immutable,
   `AVAILABLE → RESERVED → CONSUMED` / `REVOKED` / `EXPIRED` enforced by trigger.
   `claim_key_packages` reserves one per active device with `FOR UPDATE SKIP LOCKED`
   (single use under concurrency); its public endpoint belongs to membership (E).
6. **Revocation** is terminal (trigger), closes the device's packages, appends a
   transparency leaf; revoked devices cannot publish and leave the directory.
7. **Directory and anti-enumeration.** Device lists and proofs are served only for
   principals / devices of the caller's own environment (Platform Access resolution);
   anything else is `NOT_FOUND`; lookups are rate-limited per credential.
8. **Key-transparency log.** RFC 9162 Merkle tree in the same transaction as the device
   change, serialized by the tree-head row lock (sufficient for the foundation; batching or
   sharding is a later capacity decision), append-only by trigger. Proofs come from
   O(log² n) stored nodes; clients verify with `cf-messaging-crypto`. Status is reported
   as `UNWITNESSED`: verifiable, NOT anti-equivocation, NOT certified key transparency.
   Revision (18 October 2026, after the specification review — Part V G.5 "clients cache
   signed tree heads", §142): every served tree head is signed with a dedicated Ed25519
   key (`MESSAGING_KT_SIGNING_KEY_FILE`, required; never the service-authentication key)
   over a length-prefixed encoding of log id, size, root and timestamp
   (`cf_messaging_crypto::transparency::tree_head_signing_input`). Deterministic Ed25519
   makes two signed heads with different roots for one size portable evidence of
   equivocation; detecting it still needs witnesses (AD-05). Without a key the directory
   and proof routes answer 503 — an unsigned head is never served.
9. **Content boundary.** `OpaqueCiphertext` (no text accessor, no Display, length-only
   Debug) vs `ManagedContent`, admitted only through the persisted profile
   (`MessagePayload::accept`); bounded `TransportMetadata`. The send engine is Workstream F.
10. **Outbox.** Events commit with their state; a leased, `SKIP LOCKED` publisher with
    backoff and dead-lettering exists and is tested, but no consumer is wired yet
    (realtime / webhooks are later workstreams), so events stay PENDING and readiness
    reports the backlog.

## Not certified here

Full E2EE (encrypted send / decrypt / reconnect) is certified after Workstream I.
Independent key-transparency witnessing, mobile / Web clients and external cryptographic
review are open (ADR 0045).
