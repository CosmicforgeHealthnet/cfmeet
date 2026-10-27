# Meetings and Conferences navigation

The backend models are separate but related:

- `meeting_spaces::Model` is the persistent definition: owner, code, timezone, policies, membership and recurrence configuration. Its status is ACTIVE / ARCHIVED / DELETED.
- `conference_sessions::Model` is one scheduled or live occurrence, linked through `meeting_space_id`. It has its own dates, participants, sessions, provider room, lock state and CREATED → PREPARING → WAITING → LIVE → ENDING → ENDED lifecycle.

Both desktop and mobile sidebars now use `lib/dashboard-navigation.ts` with separate **Meetings** and **Conferences** entries. Dashboard tabs match that distinction.

## Meetings

`/dashboard/meetings` lists persistent meeting definitions without conference status badges or direct Join actions. `/dashboard/meetings/new` creates only the space; it never creates a scheduled conference as a side effect. The detail page manages settings, members, invitations and recurring rules, with a link to Conferences filtered by that meeting space.

## Conferences

`/dashboard/conferences` lists sessions across the caller's meeting spaces. Because the backend exposes member-scoped lists through `/meeting-spaces/{id}/conferences`, the page combines those independent TanStack queries and retains per-space loading/error states. It does not invent a global conferences endpoint or infer membership from a link.

Selecting a space reveals its date-filtered session history and the role-authorized Schedule occurrence action. Each session links to `/dashboard/conferences/{id}` for lifecycle and participant management, or to the selected occurrence's pre-join flow. Conference details link back to Conferences and separately to persistent meeting settings.

Conference UI components live in `components/conferences`; persistent meeting UI lives in `components/meeting-spaces`. The old `/dashboard/schedule` URL redirects to Conferences. Dashboard instant creation still uses the backend's atomic instant-meeting operation (space plus LIVE conference), is labelled Instant conference, and joins the returned conference explicitly.

Existing shadcn components, TanStack hooks and `globals.css` theme tokens are reused.
