# ADR 0067 — Signing in is enough for a browser (no verification step on the web)

**Status:** Accepted (owner decision, 2026-11-19)
**Date:** 2026-11-19
**Supersedes:** [ADR 0060](0060-account-linked-devices-and-production-personal-e2ee.md) decision 3 for WEB devices (trust) and the refusal part of decision 2 (`WEB_DEVICE_LIMIT_REACHED`). Everything else in ADR 0060 stays.

## Context

ADR 0060 made a new browser *untrusted* until another trusted device approved it, by typing
a 6-digit code the browser showed, or until an email step-up code arrived ("Verify this
browser … Code for your other device … No other device with you? Email me a code"). An
untrusted browser could not enroll a personal E2EE (Messaging) device, so private chats did
not work on it.

The owner asked for the platform to be easier to use: no browser verification step. A user
who signs in has already proved ownership of the account:

* the sign-in is an emailed one-time code (the same mailbox the email step-up used), a
  Google identity, and the authenticator code when two-step sign-in is on;
* the ADR 0060 email step-up therefore added no factor the sign-in did not already prove.

## Decisions

1. **A browser is trusted by the fresh sign-in it is declared from.** `POST
   /api/v2/me/devices/current` with `deviceClass: WEB` makes the device `TRUSTED` and
   `ACTIVE` at once. No link, approval or email code is needed or shown on the web.
   * The value stored is `trustedVia = BOOTSTRAP` ("trusted by its own sign-in, no other
     device"), so no schema or migration change is needed and a rollback to the previous
     image reads the rows unchanged. The security log records `trustedBy: SIGN_IN`; the
     account's first trusted device keeps `trustedBy: BOOTSTRAP` and its email notice. Every
     new sign-in already sends the new sign-in alert.
   * A browser declared as untrusted under ADR 0060 is trusted the same way when it
     declares again with a fresh proof of its key (the web client does so on every start),
     or when it signs in again.
2. **Kept security properties.**
   * The device key is generated in the browser, non-extractable, and bound to the sign-in
     (Ed25519 proof of possession with a single-use nonce; refresh of a device-bound
     sign-in needs a `REFRESH` proof).
   * A device can only be declared from a sign-in younger than the re-authentication
     window (`DEVICE_DECLARATION_REFUSED` otherwise). A stolen old access token cannot bind
     a new trusted device. A sign-in made before the device flow therefore still needs one
     more sign-in; this is the only remaining "sign in again", and the web hands over to the
     normal sign-in page on its own.
   * Revocation is terminal (database trigger). A revoked browser that signs in again gets a
     new device row; the revoked row never comes back.
   * Sign out everywhere still revokes every device.
3. **The per-account browser limit never blocks a sign-in.** At `ACCOUNT_MAX_WEB_DEVICES`
   active browsers, the declaration revokes the least recently used browser of the account
   (oldest of its last declaration and its sign-ins' last refresh), in the same transaction
   under the account lock. Its sign-ins end and the usual fan-out ends its Messaging device
   (MLS removal at the next commit), push, Calling endpoint, realtime and meeting media. The
   security log records `DEVICE_REVOKED` with `reason: WEB_DEVICE_LIMIT_REACHED`.
   * Chosen over a "too many browsers" message because it never leaves the user stuck. A
     browser the user forgot about is the one that goes; signing in there again works.
   * `WEB_DEVICE_LIMIT_REACHED` stays in the error catalogue (older clients may still
     handle it), but the declaration no longer returns it.
4. **Mobile is unchanged.** A new phone is still `REPLACEMENT_PENDING` until a link
   completes (approval by a trusted device, or the email step-up), because it replaces the
   primary mobile and signs the old phone out. The link endpoints stay for that. The web
   only shows the approval prompt for a new phone ("A new phone wants to use private
   messaging on your account"); it never shows or requires a link for itself.
5. **Contract.** `cfmeet.meeting-service.http-api` 1.8.0: descriptions only (no operation,
   schema or error code added or removed).

## Consequences

* Private chats work on a new browser right after sign-in, with no extra screen. Setup is
  silent; a message written meanwhile shows as "Sending…" and goes out once setup ends.
* Anyone who can sign in to the account can now get a trusted browser, which can also
  approve a new phone. Before, the email step-up already allowed the same to anyone holding
  the mailbox, which is what the emailed sign-in code proves. Two-step sign-in raises the
  bar for both. The new sign-in alert and the security log show every new browser.
* A browser never gets messages encrypted before its messaging device existed. The web
  shows them as "Sent before this browser joined" (with the existing "Bring over history
  from another device" path when another device exists) instead of an endless "Waiting for
  this message".
