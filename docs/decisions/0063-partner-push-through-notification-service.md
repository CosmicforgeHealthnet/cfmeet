# ADR 0063 — Push to partner apps with the partner's own keys, through the Notification Service

**Status:** Accepted (owner decision, 2026-11-18); delivered in phase B5
**Date:** 2026-11-18
**Extends:** [ADR 0062](0062-cf-meet-platform-build-users-tokens-devices.md).

## Context

A partner's user must be alerted when the partner's app is closed: an incoming call rings, a new
message arrives. Today:

* Messaging and Calling push only to CF Meet's own app, with CF Meet's own Apple and Google keys;
* production sends Web Push only.

Leading providers (Twilio, Stream, Sendbird) let the partner upload its own APNs (including VoIP)
and FCM keys, then send on the partner's behalf.

## Decisions

1. **The Notification Service sends partner pushes.** It is the one push sender for every service. The owner placed its partner work in our scope (2026-11-18). Its additions are:
   * **N-P1, provider vault.** Partner APNs / APNs VoIP / FCM credentials are stored sealed and scope-locked to one tenant, application and environment.
   * **N-P2, registrations.** For a `platform-principal` subject (scope, principal, installation, platform).
   * **N-P3, call fast lane.** iOS uses APNs VoIP (PushKit); Android uses FCM high-priority data messages. It is synchronous with no retries, like today's call lane.
   * **N-P4, recipient kind.** `platform-principal` in the notification event contract, with a metadata-only payload.
   * **N-P5, observability.** Per-scope counters and disabling of invalid handles.
2. **The Developer Console only uploads the keys.**
   * It seals the secret to the Notification Service's public key on receipt, forwards it, and keeps only metadata: provider, bundle or project, key id, fingerprint, status.
   * Test environments may use only APNs sandbox hosts.
3. **Messaging is the facade for device push registrations.** Messaging and Calling address a `platform-principal` recipient. CF Meet's own first-party push paths are unchanged.
4. **Isolation.**
   * A partner's credential can only send to that partner's own registrations; this is enforced in storage.
   * Revoking a credential stops sends within a minute.
   * Payloads carry identifiers, never message text, unless the owner later allows previews for MANAGED chats.

## Consequences

* Notification Service changes are additive, behind their own flags. Its existing push, email and inbox behaviour and tests must stay unchanged.
* The mobile SDKs need native CallKit, PushKit and ConnectionService glue. Owner decision 10 says who builds it.
