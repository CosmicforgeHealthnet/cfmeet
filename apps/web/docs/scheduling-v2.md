# Scheduling, conferences and members (v2)

Open **Dashboard → Meetings** for persistent definitions, membership and recurring rules, and **Dashboard → Conferences** for scheduled/live sessions and participant management. See `meeting-conference-navigation.md` for the separated routes and workflows. Both follow the existing service → TanStack Query hook → component folders, use shared shadcn controls, and inherit light/dark colors and borders from `globals.css`.

## Scheduling

- Create individual occurrences and filter the occurrence list by date. These dates use the browser timezone and are serialized as absolute ISO timestamps.
- Read, create/replace and end recurrence. `startLocal` remains a local datetime with no UTC conversion; the backend interprets the RRULE in the space timezone and performs bounded, idempotent materialization.
- Read exceptions and cancel/reschedule a single future occurrence, including one outside the materialization horizon. Its original timestamp must include an offset; replacement dates use the browser timezone.
- Ending recurrence accepts HTTP 204. Only the documented `404 MEETING_NOT_FOUND` from recurrence reads becomes an empty state. Authorization and transport errors remain visible.

## Conference lifecycle

The workspace consumes read/create/list, prepare, start, cancel, reschedule, lock/unlock and end operations. Rescheduling is restricted to CREATED, matching the backend service. Preparing can be retried while PREPARING. Existing legacy occurrences cannot be changed through v2 lifecycle commands.

Lifecycle responses remain authoritative: ending a conference displays ENDING until the provider confirms ENDED. Occurrence lists and room conference status refresh every five seconds. Mutations refresh the conference and space/resolver caches, including on conflicts.

Occurrence links include `?conference={id}`. Pre-join reads and checks that conference against the resolved space, loads admission requirements, and submits passcode, invitation token, explicit consent decisions and the previous session ID. It retains the returned participant token and session ID. Credentials from an earlier conference are excluded from requests targeting a different conference.

Leaving calls the v2 leave endpoint with the caller's participantSessionId before disconnecting. Failed explicit leave requests remain visible and can be retried. A provider disconnect attempts session closure and clears the tab's participant credentials while preserving account login. Leaving does not revoke membership or change admission state. The room provides separate lock/unlock and confirmed end-for-everyone actions.

## Membership

Owners, hosts and co-hosts can read members. Owners/hosts can add, change roles and revoke within backend grant limits. Add accepts exactly one email or user ID; this is not an email invitation. Owner grants use the separate ownership-transfer operation. Role changes submit the reviewed membership version; conflicts refresh the list for review. Conference role changes remain separate from persistent membership.

## Verification

`node --experimental-strip-types --test tests/scheduling-v2.test.mjs`

The tests cover endpoint methods and payloads, wall-time preservation, timestamp/window validation, role bounds and co-host overrides, empty recurrence vs authorization failures, HTTP 204, cache refreshes after conflicts, ENDING preservation, and backend enum parity. TypeScript and scoped ESLint are also checked. Live backend/provider integration requires the user's configured services and accounts; these checks do not launch servers.
