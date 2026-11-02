# Meetings and Conferences navigation

The backend models are separate but related:

- `meeting_spaces::Model` is the persistent definition: owner, code, timezone, policies, membership and recurrence configuration. Its status is ACTIVE / ARCHIVED / DELETED.
- `conference_sessions::Model` is one scheduled or live occurrence, linked through `meeting_space_id`. It has its own dates, participants, sessions, provider room, lock state and CREATED → PREPARING → WAITING → LIVE → ENDING → ENDED lifecycle.

Both desktop and mobile sidebars use `lib/dashboard-navigation.ts`; scheduled occurrences are accessed through **Meetings**, not a separate Conferences navigation item.

## Meetings

`/dashboard/meetings` lists persistent meeting definitions without conference status badges or direct Join actions. `/dashboard/meetings/new` creates only the space; it never creates a scheduled conference as a side effect. The detail page manages the space's conferences, settings, members, invitations and recurring rules.

## Conferences

On `/dashboard/meetings/{spaceId}`, the Conferences section lists that meeting's sessions, date filters, and role-authorized Schedule occurrence action. The backend exposes these lists through `/meeting-spaces/{id}/conferences`; it does not require or imply a global conferences endpoint. Each session links to `/dashboard/meetings/{spaceId}/conferences/{conferenceId}` for lifecycle and participant management, or to the selected occurrence's pre-join flow. Conference details link back to their meeting.

The old `/dashboard/schedule` URL redirects to `/dashboard/meetings`. Dashboard instant creation still uses the backend's atomic instant-meeting operation (space plus LIVE conference), and joins the returned conference explicitly.

Conference UI components live in `components/conferences`; persistent meeting UI lives in `components/meeting-spaces`. Dashboard instant creation still uses the backend's atomic instant-meeting operation (space plus LIVE conference), and joins the returned conference explicitly.

Existing shadcn components, TanStack hooks and `globals.css` theme tokens are reused.
