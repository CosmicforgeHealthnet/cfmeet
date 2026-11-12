# Frontend contract — Q&A restore and LiveKit participant identity

Audience: CF Meet web and mobile developers. Both changes are additive: no existing request,
response, event or token claim changed shape, so current clients keep working.

Base path: `https://api.cfmeet.com/api/v2` (the public gateway routes the same operations).
Swagger: `packages/contracts/openapi/meeting-service.json#v2_restore_question`, tag **Q&A (v2)**.

---

## 1. Restore a dismissed question

```
POST /api/v2/conferences/{id}/questions/{questionId}/restore
Authorization: Bearer <account token>      (or the guest participant token)
```

No request body (send `{}` or nothing).

| Item | Value |
|---|---|
| Permission | `qna.dismiss` — the same one as dismiss and archive (host, co-host, moderator by default). Show the "Restore" action only where you already show "Dismiss". |
| Conference state | Not ENDED / CANCELLED (same as the other moderation actions). |
| Response `200` | `QuestionActionResponse` = `{ "question": QuestionView, "changed": bool }` |

### Result states

| Question before | Result | `changed` | Event |
|---|---|---|---|
| `DISMISSED`, never answered | `OPEN` | `true` | `qna.question_restored` |
| `DISMISSED`, answered before dismissal (live or written) | `ANSWERED`, original `answer` and `answeredAt` kept | `true` | `qna.question_restored` |
| `OPEN` or `ANSWERED` (repeat click, another moderator restored first) | unchanged | `false` | none |
| `ARCHIVED` | `409 CONFERENCE_STATE_CONFLICT` | — | none |

What is preserved: body, author (internally), `anonymous`, answer, `upvoteCount`, every voter's
`votedByMe`, audit history. What is **not** restored: the pin (dismissal unpins; pin again if you
want it on top). `version` increases by one on every real change.

### Errors

| HTTP | `error.code` | When |
|---|---|---|
| 401 | `UNAUTHORIZED` | no / invalid token |
| 403 | `PERMISSION_DENIED` | caller lacks `qna.dismiss` (attendees, including the question's author) |
| 404 | `QUESTION_NOT_FOUND` | unknown id, or the question belongs to another conference |
| 409 | `CONFERENCE_STATE_CONFLICT` | question is ARCHIVED, or the conference has ended |

### Client handling

* **Pessimistic is recommended** (moderator-only, rare action): disable the button, call, then
  replace the question with `response.question`.
* Optimistic is safe if you prefer it: on `409` put the question back as ARCHIVED; on `403` / `404`
  revert and refresh `GET …/questions`.
* `changed: false` is success, not an error — just apply `response.question`.
* Reconcile by `version`: ignore any question update (response or event) whose `version` is lower
  than the one you hold.

---

## 2. Realtime event `qna.question_restored`

Event name follows the existing Q&A family (`qna.question_created`, `…_answered`,
`…_dismissed`, `…_archived`, `…_pinned`). The brief's spelling `qna.question.restored` is **not**
used; subscribe to `qna.question_restored`.

Delivered on the conference realtime stream (`GET …/realtime`, replay via `GET …/events`) through
the transactional outbox: it is committed together with the status change, carries the conference
sequence number, and is emitted exactly once per real restore (never for `changed: false`).

Realtime frame (WebSocket) — the envelope is the same `RealtimeEnvelope` as every conference event:

```json
{
  "type": "event",
  "event": {
    "eventId": "uuid",
    "eventType": "qna.question_restored",
    "schemaVersion": 1,
    "conferenceId": "uuid",
    "meetingSpaceId": "uuid",
    "sequence": 412,
    "occurredAt": "2026-11-12T10:00:00Z",
    "correlationId": "string | null",
    "payload": {
      "questionId": "uuid",
      "conferenceId": "uuid",
      "body": "When is lunch?",
      "anonymous": false,
      "participantId": "uuid | null",
      "displayName": "string | null",
      "status": "OPEN | ANSWERED",
      "answer": "string | null",
      "pinned": false,
      "upvoteCount": 2,
      "version": 4
    }
  }
}
```

* `participantId` / `displayName` are `null` for anonymous questions — always.
* Attendees: insert the question back into the visible list (it was removed on
  `qna.question_dismissed`); `votedByMe` / `isMine` are viewer-specific, so take them from your
  cached copy or re-read `GET …/questions` if you have none.
* Moderators: update the item in place (moderators also see DISMISSED questions in the list).
* Duplicate delivery (reconnect replay): apply only if `version` is newer than the cached one.

---

## 3. LiveKit participant → CF Meet participant

Every media credential the Meeting Service issues (join response `rtc.credential` and
`POST …/media-credential` refresh) now carries the **canonical conference participant id** — the
same id used by `GET …/participants`, hand raise, spotlight, mute, remove, role, chat restriction
and Q&A (`participantId` everywhere in the v2 API).

| LiveKit field | Value | Use |
|---|---|---|
| `participant.identity` | `ps_<session uuid without dashes>` — one per **session**; changes on reconnect / new device | LiveKit plumbing only |
| `participant.name` | display name | **label only** |
| `participant.attributes.participantId` | conference participant uuid | **the key** |
| `participant.metadata` | `{"participantId":"<same uuid>"}` | same key, for SDKs without attributes |

Reading it:

```ts
// livekit-client (web)
const pid = p.attributes?.participantId
  ?? (p.metadata ? JSON.parse(p.metadata).participantId : undefined);
```

```dart
// livekit_client (Flutter)
final pid = p.attributes['participantId']
    ?? (p.metadata?.isNotEmpty == true ? jsonDecode(p.metadata!)['participantId'] : null);
```

### Mapping rules

1. Build `Map<participantId, ParticipantRecord>` from `GET …/participants` and keep it current
   from `participant.*` events.
2. For each LiveKit `RemoteParticipant` / `LocalParticipant`, read `participantId` and look the
   record up. Key every tile, audio indicator and screen-share tile by `participantId`, not by
   `identity` and **never by display name**.
3. Raised hands (`hand.raised` / `GET …/hand-raise` → `participantId`), spotlight
   (`spotlights[].participantId`), host controls (`/participants/{participantId}/…`) and Q&A
   `participantId` all join on this key. Two people both called "Alex" now resolve to two records.
4. Screen share: the share track belongs to the same LiveKit participant, so it carries the same
   `participantId`.
5. One person may briefly have two LiveKit participants with the same `participantId` (old and new
   session during reconnect, or two devices). Group by `participantId` and prefer the most recently
   connected one; drop the old one on `ParticipantDisconnected`.

### Missing or stale metadata

* LiveKit participants **without** `participantId` are not human CF Meet participants (recording /
  egress / service participants — LiveKit reports them with `kind = EGRESS` or similar). Do not
  render them as tiles, do not offer host controls on them.
* If the id is not (yet) in your participant map, show the tile with its display name and no
  host controls, and refresh `GET …/participants`; it will appear once admission events arrive.
* Clients joined with a credential issued before this release have no metadata until they reconnect
  or refresh their credential (refresh happens before expiry anyway). Keep the old name-based
  fallback only for that window, if at all.

### Reconnect and refresh

* Credential refresh (`POST …/media-credential`): same session, same identity, same
  `participantId`.
* Reconnect (`POST …/join` with `previousSessionId`) and leave → rejoin: new session and new
  `identity`, **same** `participantId`.
* A new participant never receives another participant's id; ids are not reused.

### Security

* The id is informational. It grants nothing: every protected call is authorized server-side from
  the caller's own token, and the server never reads LiveKit metadata to decide anything.
* Clients cannot rewrite their metadata or attributes (`canUpdateOwnMetadata = false` in the
  credential and in every live permission update), and a token not signed by the server is
  rejected by LiveKit — a forged `participantId` cannot be presented.
* Metadata holds only the participant id: no account id, email, role or permissions.

### Remove display-name matching

Delete any code path that maps media tiles to participants by comparing `name`/`displayName`.
Display names are user-editable, not unique, and can collide inside one meeting.
