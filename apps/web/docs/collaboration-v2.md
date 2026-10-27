# Conference collaboration v2

Collaboration state and moderation belong to a conference occurrence, not the reusable
meeting space. The room UI uses conference-scoped participant IDs and leaves persistent
space membership to the meeting-space workspace.

## Room controls

- **Conference policy** updates the effective collaboration policy with the reviewed
  `collaborationVersion`. It covers default microphone/camera locks, screen-share
  policy, reactions, hand raising and spotlight limits.
- **Chat policy** uses the dedicated chat-policy endpoint and supports conference,
  private and breakout chat settings. Chat send modes are `EVERYONE` and `HOSTS_ONLY`;
  disabling conference chat is represented by `chatEnabled: false`.
- **Participants** exposes conference-scoped role changes, provider mute, persistent
  media locks/unlocks, immediate screen-share stop, spotlight, chat restriction and
  hand lowering. Immediate stop is separate from locking future screen sharing.
- **Engagement** exposes backend-supported reactions and the caller's hand raise.
  Raised hands and conference chat history use cursor-based recovery; chat can load
  older pages without repeatedly polling every historical page.
- **Refresh media access** requests a fresh credential with the active
  `participantSessionId`, then reconnects the LiveKit room with the returned URL and
  credential. This controlled reconnect does not close the server-side participant
  session.

The room polls canonical collaboration state for current effective permissions,
caller lock state, spotlights, raised hands and active screen shares. The backend does
not expose every other participant's lock state, so the UI offers explicit lock and
unlock commands rather than guessing a remote state. Role/policy checks only control
which actions are presented; backend authorization remains authoritative, and mutation
errors are surfaced in the room.

Verification from `apps/web`:

```text
node --test tests/collaboration-v2.test.mjs tests/invitations-participants-v2.test.mjs
npx tsc --noEmit
npx eslint src/types/v2/index.ts src/services/v2/collaboration.service.ts src/hooks/v2/useCollaboration.ts src/lib/meeting-authority.ts src/components/conferences/CollaborationPolicyControls.tsx src/components/conferences/ParticipantsPanel.tsx src/components/room/RoomEngagementControls.tsx src/components/room/conference-controls.tsx src/components/room/footer-controls.tsx src/components/room/live-room.tsx src/components/room/sidebar.tsx
```
