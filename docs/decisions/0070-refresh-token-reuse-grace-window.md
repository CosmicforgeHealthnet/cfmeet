# ADR 0070 — A short grace window for the refresh token a rotation just replaced

**Status:** Accepted
**Date:** 2026-11-28
**Amends:** [ADR 0057](0057-account-sessions-typed-tokens-and-revocation-epoch.md) decision 2 (strictly single-use refresh tokens). Everything else in ADR 0057 stays.

## Context

ADR 0057 made every refresh token single use: presenting anything other than the session's
`current_refresh_id` is treated as theft. The session is revoked (`REFRESH_REUSE`), its end
is fanned out to the Hub, Messaging and Calling, and the user is emailed "A CF Meet sign-in
was ended for your protection … used from two places at once".

Users received that email during normal use on a single device. Legitimate clients present
the token a rotation just replaced in several ways:

* the web proxy (`apps/web/src/app/api/session/refresh/route.ts`) keeps the old cookie when
  the upstream call times out (10 s) or fails, although the server may already have
  rotated; the next refresh presents the superseded token;
* the response is lost in transit: the phone sleeps, the network switches, the tab closes
  mid-refresh;
* two refreshes of the same client race (browsers without Web Locks, two app processes).

Each of these ended a sign-in that was never stolen, with a security email that teaches
users to ignore security email.

## Decision

1. **Grace window.** `account_sessions.previous_refresh_id` (nullable, new) holds the
   refresh id the session's last rotation replaced; rotation sets it together with the new
   current id. When the presented refresh id equals `previous_refresh_id` and
   `last_refreshed_at` is within `ACCOUNT_REFRESH_REUSE_GRACE_SECONDS` (default **60**,
   bounded 0–300), the refresh is a retry or race of that rotation:
   * the session is **not** revoked, nobody is emailed and nothing is fanned out;
   * the session is **not** rotated again (that would make the other racer stale and
     cascade into real reuse);
   * the answer is a token pair for the session's **current** refresh id, so every racer
     converges on the same current token;
   * the security log records `REFRESH_TOKEN_GRACE_REUSE` (outcome `SUCCESS`).
2. **Everything else is reuse, as before.** A token two or more generations old, or the
   previous token after the window, revokes the session exactly as ADR 0057 decided.
3. **`0` restores strict single use** (an operator lever, no release needed).
4. **Order of checks unchanged.** The device binding (`check_refresh_binding`, ADR 0060)
   still runs first, so a device-bound sign-in needs a valid proof of its device key in the
   grace path too. The session must still be open and belong to the token's account, and
   the revocation epoch is checked before rotation as before.

This is the industry-standard trade-off (Auth0 "reuse interval", Okta "grace period for
token rotation").

## Consequences

* A thief holding the token a rotation replaced, presenting it within 60 seconds of that
  rotation, receives the current pair instead of tripping detection. The window is short,
  only the immediately previous token qualifies, the attempt is in the security log, and the
  next rotation by either party still leaves the other one token behind. Once the window
  closes, any further use of an old token is reuse and ends the session.
* Clients still keep refreshes single-flight; the window absorbs failures, it is not a
  licence to refresh concurrently.
* Schema: one nullable column, appended to the `account_security_migrations` history
  (`m20261128_000001_refresh_reuse_grace`). The previous image keeps starting and working
  (it never reads the column). While both images serve traffic, a session the previous image
  rotates keeps an older `previous_refresh_id`, which the new image would accept within the
  window after that rotation; the next rotation by the new image corrects it. This exists
  only during a mixed-version rollout or a rollback.
