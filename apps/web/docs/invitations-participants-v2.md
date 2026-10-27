# Conferences, membership, invitations and participants

## Backend contracts reviewed

The implementation follows `services/meeting-service/src/handlers/v2/{conferences,members,invitations,participants}.rs`, their DTOs and views, and the `conference`, `membership`, `invitations`, `admission`, `meeting_permissions` and `rtc_control` services.

- Conferences are occurrences of a persistent meeting space. Concurrent lifecycle operations are serialized by the backend. `end` returns ENDING; provider confirmation alone completes ENDED. Cancel creates one exception for a recurring occurrence; reschedule is limited to CREATED in the current service implementation.
- Membership is persistent. Owners/hosts manage members with bounded grants; owners/hosts/co-hosts list members. Membership role edits require the reviewed version. Ownership changes use owner transfer.
- Invitations store only a token hash. The creation response returns the raw token once. Expiry, revocation and guarded transactional use limits are enforced by the backend. Resolve validates without consuming a use. Join checks space/conference binding and any verified-email binding before consuming a use.
- Participants are conference-scoped logical people, separate from their connected device sessions. Roster contains admitted participants, including people without a currently connected media session. `/participants/me` supplies current caller state. Moderation and role changes check role policy and grant hierarchy. Removing and banning save business state before provider disconnection; a pending result is not provider confirmation. Mute returns examined session and muted track counts.

## Frontend entry points

- **Meeting Spaces → Manage:** occurrences, recurrence, persistent members, plus invitation creation/list/revocation. Invitations support optional occurrence and verified-email bindings, role, expiry and use limit. Links are shown once, copied explicitly, and discarded on dismissal/navigation. Creation sends no email.
- **Occurrence → Details & participants:** conference summary/full view, lifecycle controls, admitted roster and moderation. A summary does not enable roster or moderation requests.
- **Room sidebar:** roster, current self state, role change, provider mute, removal, conference/space ban, waiting queue and chat. Self, equal-role and higher-role moderation actions are excluded. Role changes capture the version when the dialog opens; refreshes cannot silently substitute a newer version. Backend denial/conflict errors remain visible.
- **Join → Have an invitation link or token?** or `/invite#<token>`: explicit read-only review, then pre-join. The token is removed from the address bar after capture and remains only in component memory. Resolution uses a mutation with zero GC time, not a raw-token query key; mutations are reset after use. The token goes into the v2 join body. Bound occurrence IDs are checked against the resolved space.

Components live under `components/meeting-spaces` and reuse shadcn dialogs/buttons/inputs, TanStack Query hooks in `hooks/v2`, API clients in `services/v2`, and the semantic theme tokens from `globals.css`. No new dependencies or independent color system were added.

Participant mutations refresh roster, self and waiting-room caches on success and error. Current self state is polled while connected, so removal/ban/denial closes the local room even when the sidebar is closed. Persistent membership and conference participant roles remain separate.

## Verification

Run `node --experimental-strip-types --test tests/invitations-participants-v2.test.mjs tests/scheduling-v2.test.mjs` from `apps/web`, plus TypeScript and scoped ESLint. Tests cover grant hierarchy and policy overrides, token/link validation, expiry/use limits, exact route/payload contracts, read-only invitation resolution, provider-pending responses, conflict refreshes, secret-cache behavior and Rust enum parity.

Live backend, Redis/database concurrency, clipboard behavior and media-provider integration require configured services and accounts and are not exercised by the unit/contract tests.
