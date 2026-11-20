# ADR 0066 — Private chats: devices join by themselves, senders reach every registered device

**Status:** Accepted
**Date:** 2026-11-19
**Extends:** [ADR 0045](0045-messaging-domain-and-cryptographic-core.md), [ADR 0047](0047-messaging-service-devices-and-key-transparency.md), [ADR 0052](0052-messaging-v2-architecture-decision-baseline.md) (AD-01), [ADR 0060](0060-account-linked-devices-and-production-personal-e2ee.md).

## Context

A production private 1:1 chat showed "2 unread" on a phone browser, a grey bar as its
chat-list preview and an empty chat with "Securing this chat on this browser. It finishes
when someone in the chat is online" — for hours. The cause was the membership design:

* Only a device already in a chat's MLS tree could add another device (Commit with Add from
  the newcomer's published KeyPackages). The creator added what it could claim at creation;
  later devices were added by `addMissingDevices`, which ran only while a member's browser
  had the chat synchronized, only on one "designated" device (smallest device id in the
  tree, possibly an offline browser), and on any other device only after a 5-minute grace
  period that restarted on every page load.
* The sender never checked for new devices before encrypting, so it kept encrypting to the
  recipient's old (often dead) leaf: the message was "delivered" and counted unread, but no
  device of the recipient that could read it existed.
* A claim spent one KeyPackage of EVERY device of the principal, including devices already
  in the tree, and devices never replenished their stock (the start-up top-up published
  zero), so devices ran out and could not be added even by an online member.
* A revoked device that had never entered a tree left a pending removal the web client never
  committed (it only removed leaves it found), blocking every message of that chat.
* The UI showed nothing for messages it could not decrypt.

## Decisions

1. **Self-join (RFC 9420 §12.4.3.2).** Every commit carries the GroupInfo of the epoch it
   creates (public: tree public keys, credentials, group context, `external_pub`); Messaging
   stores the latest (`messaging_mls_group_infos`) and serves it in `GET …/mls/state` only
   while its epoch is current. A device of a member that is not in the tree joins BY ITSELF
   with an external commit — when it opens or syncs the chat, and before it sends. No other
   member has to be online.
2. **Who may join.** Messaging accepts an external commit (a `PublicMessage` whose clear
   sender is `new_member_commit`) only from the acting principal's own ACTIVE device, of an
   ACTIVE member, without Welcomes or removals, and never while a removal of a device that
   is still in the tree is pending (its UpdatePath would reach the removed leaf). Every
   member's client merges it only after verifying the joining leaf: the commit's sending
   device, ACTIVE in the directory, of an ACTIVE member, with exactly the directory key,
   found in the key-transparency log (when pinned). OpenMLS leaves the remaining checks to
   the application; the crypto core enforces them: no PSK, no Add, and a Remove only of a
   leaf of the same device. The server can therefore not inject a device, and removed
   members and revoked devices cannot rejoin.
3. **Senders reach every registered device (WhatsApp).** Before encrypting, the sender adds
   the members' ACTIVE devices missing from the tree from their published KeyPackages
   (`claimKeyPackages` with `deviceIds`: only those devices' packages are spent). Devices
   keep 10–20 KeyPackages published (`keyPackagesAvailable` on one's own devices).
4. **Removals never block a chat.** Pending removals of devices that are not leaves of the
   current GroupInfo's tree are moot and dropped by Messaging; clients attest every pending
   device in their removal commit (a key update when no leaf has to go).
5. **Never nothing.** A message a device cannot read (yet) shows "Waiting for this message";
   messages from before a device was linked show where older history comes from (history
   transfer from another device of the account).

## Consequences

* Contracts: `cfmeet.messaging.common` 1.32.0, `cfmeet.messaging.http-api` 1.20.0 (all
  additive). The table lives in its own migration history (`MlsJoinMigrator`), so a
  rollback to the previous image still starts.
* A device joining by external commit cannot read messages sent before it joined (MLS
  forward secrecy); devices registered before a message was sent receive it through the
  sender's Add.
* Residual: a member that rejected a join (e.g. the joining device was revoked before the
  member processed the commit) stays at the old epoch for that chat; native SDKs merge
  external joins after the core's protocol checks and must add the directory check.
