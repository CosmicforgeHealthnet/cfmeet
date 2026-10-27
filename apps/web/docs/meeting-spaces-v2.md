# Meeting Spaces v2 components

The dashboard list, create form, editor, instant-meeting action and identifier inputs
use the Meeting Service's v2 space API through `spacesService` and TanStack hooks.

- `/dashboard/meetings`: lists owned/member spaces. Owner/host full views can edit
  or duplicate configuration. Only an owner sees archive and ownership-transfer
  controls. A summary response or a meeting link grants no management rights.
- `/dashboard/meetings/{identifier}/edit`: reads a space using any backend-supported
  identifier, then PATCHes its canonical UUID with the version the user reviewed.
  The editor changes reusable settings, never conference start/end times or ownership.
  A stale draft is not silently retried against a newer version; the user must reload.
- `/dashboard/schedule`: creates a space, optionally followed by a scheduled conference.
  These are separate operations. If conference creation fails after the space was
  created, retry reuses that space instead of creating another one. Disabling the
  first-conference option creates configuration only.
- Dashboard New Meeting: calls `/api/v2/instant-meetings`, which atomically creates
  the space and a LIVE conference. Admission/RTC joining happens separately through
  the existing room/prejoin flow, so an RTC join error cannot trigger another creation.
- Join inputs resolve links, UUIDs and legacy/v2 codes through `/api/v2/resolve` and
  navigate using the canonical meeting code. Resolution does not assign membership,
  roles, or bypass admission policy.

Archive uses a reviewed version, preserves history, stops recurrence and cancels
unstarted future occurrences. It does not end an ongoing conference, and there is no
restore endpoint in the listed backend API. Duplicate creates only reusable
configuration, owned by the authenticated caller. Owner transfer uses its dedicated
endpoint with `newOwnerUserId`, `version`, and `retainPreviousOwner`.

The shared v2 client uses `http://localhost:8080/api/v2` by default. For space/resolver
operations it attaches the login credential when available and excludes a previous
conference's `X-Participant-Token`. Anonymous resolution remains supported. The
backend's role checks and version checks are authoritative.

Policy/role vocabulary matches the Rust domain enums, including AUTHENTICATED,
INVITE_ONLY, OWNER and MODERATOR. Passcode hashes are never returned or displayed:
blank on edit preserves an existing passcode, and replacement/removal is explicit.

Verification from `apps/web`:

```text
node --test tests/meeting-spaces-v2.test.mjs
npx tsc --noEmit
```

Tests exercise the actual service/client with mocked HTTP and check permissions,
versioned payloads, identifier normalization, passcode preservation and backend enum
parity. They do not create real spaces, transfer ownership or start conferences.
