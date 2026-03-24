# Participant Management

Complete participant control system with listing, kicking, waiting room management, and media controls (audio/video/screen sharing).

## Table of Contents

- [Architecture](#architecture)
- [Components](#components)
- [Participant Lifecycle](#participant-lifecycle)
- [API Endpoints](#api-endpoints)
- [Waiting Room](#waiting-room)
- [Media Control](#media-control)
- [Database Schema](#database-schema)
- [Security](#security)
- [Usage Examples](#usage-examples)
- [Error Handling](#error-handling)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)
- [Quick Reference](#quick-reference)

## Architecture

```
┌─────────────────┐     ┌────────────────────┐     ┌─────────────────┐
│   HTTP Client   │────▶│ Participant API    │────▶│    Database     │
│  (Browser/App)  │     │  (Control)         │     │  (Participants) │
└─────────────────┘     └────────────────────┘     └─────────────────┘
        │                        │                          │
        │                        ▼                          │
        │               ┌─────────────────┐                 │
        │               │ Authorization   │                 │
        │               │ (Host/Self)     │                 │
        │               └─────────────────┘                 │
        │                        │                          │
        ▼                        ▼                          ▼
┌──────────────────────────────────────────────────────────────┐
│                     LiveKit SFU                              │
│           (Media Tracks: Audio, Video, Screen)               │
└──────────────────────────────────────────────────────────────┘
```

## Components

| Component             | Location                   | Purpose                |
| --------------------- | -------------------------- | ---------------------- |
| `ParticipantHandlers` | `handlers/participants.rs` | HTTP request handlers  |
| `ParticipantRoutes`   | `routes/participants.rs`   | Route definitions      |
| `ParticipantModel`    | `models/participants.rs`   | Database entity        |
| `ParticipantDTO`      | `dto/meetings.rs`          | Request/response types |
| `SessionLogs`         | `models/session_logs.rs`   | Event logging          |

## Folder Structure

```
backend/src/
├── handlers/
│   └── participants.rs     # Participant management + media control
├── routes/
│   ├── meetings.rs         # Meeting-scoped participant routes
│   └── participants.rs     # Participant-specific routes
├── models/
│   ├── participants.rs     # Participant entity
│   └── session_logs.rs     # Event logs
└── dto/
    └── meetings.rs         # DTOs and validation
```

## Participant Lifecycle

### Status Flow

```
                    ┌──────────────┐
                    │   WAITING    │ (Private meetings only)
                    └──────────────┘
                           │
                    ┌──────┴──────┐
                    │             │
              Admit │             │ Deny
                    │             │
                    ▼             ▼
           ┌──────────────┐   ┌──────────┐
           │   JOINED     │   │  KICKED  │
           └──────────────┘   └──────────┘
                    │             ▲
                    │             │
              Leave │       Kick  │
                    │             │
                    └─────────────┘
```

### Status Transitions

| From      | To       | Trigger       | Who Can Do This |
| --------- | -------- | ------------- | --------------- |
| `waiting` | `joined` | Host admits   | Host only       |
| `waiting` | `kicked` | Host denies   | Host only       |
| `joined`  | `joined` | Leave meeting | Self            |
| `joined`  | `kicked` | Host kicks    | Host only       |

### Roles

| Role          | Description                | Permissions          |
| ------------- | -------------------------- | -------------------- |
| `host`        | Meeting creator            | Full control         |
| `participant` | Regular attendee           | Control own media    |
| `viewer`      | Observer only (future use) | View only (no media) |

## API Endpoints

### List Participants (Protected)

```
GET /api/v1/meetings/{meeting_id}/participants
```

**Authorization**: Host or participant in meeting

**Response:**

```json
{
  "success": true,
  "data": [
    {
      "participant_id": "uuid",
      "meeting_id": "uuid",
      "user_id": "uuid",
      "role": "host",
      "display_name": "John Doe",
      "status": "joined",
      "join_time": "2026-01-22T10:05:00Z",
      "leave_time": null,
      "is_muted": false,
      "is_video_on": true,
      "is_screen_sharing": false
    }
  ]
}
```

**Errors:**

- `401` - Unauthorized (no token)
- `403` - Forbidden (not host or participant)
- `404` - Meeting not found

### Kick Participant (Host Only)

```
POST /api/v1/participants/{participant_id}/kick
```

**Authorization**: Host only

**Response:**

```json
{
  "success": true,
  "message": "Participant John Doe has been kicked"
}
```

**Restrictions:**

- ❌ Cannot kick the host
- ✅ Sets participant status to `kicked`
- ✅ Sets `leave_time` to current time
- ✅ Logs kick event with metadata

**Errors:**

- `401` - Unauthorized
- `403` - Only host can kick
- `404` - Participant not found
- `409` - Cannot kick the host

---

## Waiting Room

Private meetings automatically place non-host participants in waiting room until admitted.

### List Waiting Participants (Host Only)

```
GET /api/v1/meetings/{meeting_id}/waiting
```

**Response:**

```json
{
  "success": true,
  "data": [
    {
      "participant_id": "uuid",
      "user_id": "uuid",
      "display_name": "Jane Smith",
      "join_time": "2026-01-22T10:03:00Z"
    }
  ]
}
```

### Admit Participant (Host Only)

```
POST /api/v1/meetings/{meeting_id}/waiting/{participant_id}/admit
```

**Response:**

```json
{
  "success": true,
  "message": "Participant Jane Smith has been admitted"
}
```

**Effect:**

- ✅ Status changed from `waiting` to `joined`
- ✅ Participant can now join media session
- ✅ Logs admit event

**Errors:**

- `409` - Participant not in waiting room

### Deny Participant (Host Only)

```
POST /api/v1/meetings/{meeting_id}/waiting/{participant_id}/deny
```

**Response:**

```json
{
  "success": true,
  "message": "Participant Jane Smith has been denied"
}
```

**Effect:**

- ✅ Status changed from `waiting` to `kicked`
- ✅ Sets `leave_time`
- ✅ Logs deny event

---

## Media Control

Participants can control their own media. Hosts can control any participant's media.

### Update Audio (Mute/Unmute)

```
PATCH /api/v1/participants/{participant_id}/audio
```

**Request:**

```json
{
  "is_muted": true
}
```

**Authorization**: Self or Host

**Response:**

```json
{
  "success": true,
  "participant_id": "uuid",
  "is_muted": true,
  "is_video_on": true,
  "is_screen_sharing": false
}
```

**Errors:**

- `403` - Can only control your own media (unless you're host)

### Update Video (Enable/Disable)

```
PATCH /api/v1/participants/{participant_id}/video
```

**Request:**

```json
{
  "is_video_on": false
}
```

**Authorization**: Self or Host

**Response:**

```json
{
  "success": true,
  "participant_id": "uuid",
  "is_muted": false,
  "is_video_on": false,
  "is_screen_sharing": false
}
```

### Start Screen Share

```
POST /api/v1/meetings/{meeting_id}/screen-share/start
```

**Authorization**: Participant in meeting

**Response:**

```json
{
  "success": true,
  "participant_id": "uuid",
  "is_screen_sharing": true,
  "message": "Screen sharing started successfully"
}
```

**Restrictions:**

- ❌ Only one participant can share screen at a time
- ✅ Logs screen share start event

**Errors:**

- `404` - Not a participant in meeting
- `409` - Already screen sharing OR another participant is sharing

### Stop Screen Share

```
POST /api/v1/meetings/{meeting_id}/screen-share/stop
```

**Response:**

```json
{
  "success": true,
  "participant_id": "uuid",
  "is_screen_sharing": false,
  "message": "Screen sharing stopped successfully"
}
```

**Errors:**

- `409` - Not currently screen sharing

---

## Database Schema

### Participants Table

```sql
CREATE TABLE participants (
    id UUID PRIMARY KEY,
    meeting_id UUID NOT NULL,
    user_id UUID,                    -- NULL for guests
    role participant_role NOT NULL,   -- 'host', 'participant', 'viewer'
    join_time TIMESTAMP NOT NULL,
    leave_time TIMESTAMP,
    status participant_status NOT NULL, -- 'waiting', 'joined', 'kicked'
    is_muted BOOLEAN DEFAULT FALSE,
    is_video_on BOOLEAN DEFAULT TRUE,
    is_screen_sharing BOOLEAN DEFAULT FALSE,
    display_name VARCHAR(100) NOT NULL,

    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL
);
```

### Event Types

```sql
CREATE TYPE event_type AS ENUM (
    'meeting_start',
    'meeting_end',
    'participant_join',
    'participant_leave',
    'role_change',
    'screen_share_start',
    'screen_share_end',
    'media_toggle'           -- For audio/video changes
);
```

---

## Security

### Authorization Rules

| Action               | Self | Host | Other Participant |
| -------------------- | ---- | ---- | ----------------- |
| List participants    | ✅   | ✅   | ✅                |
| Kick participant     | ❌   | ✅   | ❌                |
| View waiting room    | ❌   | ✅   | ❌                |
| Admit from waiting   | ❌   | ✅   | ❌                |
| Deny from waiting    | ❌   | ✅   | ❌                |
| Mute/unmute self     | ✅   | ✅   | ❌                |
| Mute/unmute others   | ❌   | ✅   | ❌                |
| Control own video    | ✅   | ✅   | ❌                |
| Control others video | ❌   | ✅   | ❌                |
| Share screen         | ✅   | ✅   | ✅                |

### Private vs Public Meetings

**Public Meetings:**

- Anyone can join immediately
- Status = `joined` on join

**Private Meetings:**

- Host joins immediately (`joined`)
- Others placed in waiting room (`waiting`)
- Host must admit before they can join

---

## Usage Examples

### Example 1: Admit Participant from Waiting Room

```bash
# 1. List waiting participants
curl -X GET "http://localhost:8080/api/v1/meetings/{meeting_id}/waiting" \
  -H "Authorization: Bearer {host_token}"

# Response shows Jane waiting
{
  "success": true,
  "data": [{
    "participant_id": "p-123",
    "display_name": "Jane Smith",
    "join_time": "2026-01-22T10:00:00Z"
  }]
}

# 2. Admit Jane
curl -X POST "http://localhost:8080/api/v1/meetings/{meeting_id}/waiting/p-123/admit" \
  -H "Authorization: Bearer {host_token}"

# Response
{
  "success": true,
  "message": "Participant Jane Smith has been admitted"
}
```

### Example 2: Mute Participant (Host Action)

```bash
curl -X PATCH "http://localhost:8080/api/v1/participants/p-456/audio" \
  -H "Authorization: Bearer {host_token}" \
  -H "Content-Type: application/json" \
  -d '{"is_muted": true}'

# Response
{
  "success": true,
  "participant_id": "p-456",
  "is_muted": true,
  "is_video_on": true,
  "is_screen_sharing": false
}
```

### Example 3: Start Screen Sharing

```bash
curl -X POST "http://localhost:8080/api/v1/meetings/{meeting_id}/screen-share/start" \
  -H "Authorization: Bearer {participant_token}"

# Response
{
  "success": true,
  "participant_id": "p-789",
  "is_screen_sharing": true,
  "message": "Screen sharing started successfully"
}
```

### Example 4: Kick Participant

```bash
curl -X POST "http://localhost:8080/api/v1/participants/p-456/kick" \
  -H "Authorization: Bearer {host_token}"

# Response
{
  "success": true,
  "message": "Participant John Doe has been kicked"
}
```

---

## Error Handling

### Common Errors

| Code | Error        | Cause                                |
| ---- | ------------ | ------------------------------------ |
| 401  | Unauthorized | Missing or invalid token             |
| 403  | Forbidden    | Not authorized for this action       |
| 404  | Not Found    | Participant or meeting doesn't exist |
| 409  | Conflict     | Invalid state transition             |

### Error Response Format

```json
{
  "error": "Only the host can kick participants"
}
```

---

## Best Practices

### 1. Waiting Room Management

✅ **DO:**

- Regularly check waiting room for new participants
- Admit or deny promptly to avoid confusion
- Use participant display name to identify users

❌ **DON'T:**

- Leave participants in waiting room indefinitely
- Forget to check waiting room in private meetings

### 2. Media Control

✅ **DO:**

- Allow participants to control their own media
- Use host controls sparingly (emergencies only)
- Log media state changes for debugging

❌ **DON'T:**

- Force-mute participants without good reason
- Control others' video unless absolutely necessary

### 3. Screen Sharing

✅ **DO:**

- Enforce one-sharer-at-a-time rule
- Check `is_screen_sharing` before starting
- Stop sharing when done presenting

❌ **DON'T:**

- Allow multiple simultaneous screen shares
- Forget to stop screen sharing after presentation

---

## Troubleshooting

### Participant Stuck in Waiting Room

**Symptom**: Participant status remains `waiting` after admit

**Solution**:

1. Check host token is valid
2. Verify participant is actually in waiting room:
   ```bash
   GET /api/v1/meetings/{id}/waiting
   ```
3. Re-admit using correct participant_id

### Cannot Mute Participant

**Error**: `403 Forbidden`

**Causes**:

- Not the host or self
- Participant not in meeting

**Solution**:

- Verify you're the host or controlling your own media
- Check participant status is `joined`

### Screen Share Won't Start

**Error**: `409 Conflict - Another participant is sharing`

**Solution**:

1. Ask current sharer to stop
2. Or as host, check who is sharing:
   ```bash
   GET /api/v1/meetings/{id}/participants
   # Look for "is_screen_sharing": true
   ```

### Kicked Participant Can Rejoin

**Expected Behavior**: Being kicked doesn't ban the user permanently

**Solution**:

- For private meetings: Don't admit them again
- For public meetings: This is expected (no ban system yet)
- Future enhancement: Implement ban list

---

## Quick Reference

### Participant Status Values

| Status    | Meaning                   |
| --------- | ------------------------- |
| `waiting` | In waiting room (private) |
| `joined`  | Active participant        |
| `kicked`  | Removed from meeting      |

### Participant Roles

| Role          | Level         |
| ------------- | ------------- |
| `host`        | Full control  |
| `participant` | Standard user |
| `viewer`      | Read-only     |

### Media State Flags

| Flag                | Type    | Default | Meaning        |
| ------------------- | ------- | ------- | -------------- |
| `is_muted`          | boolean | `false` | Audio off      |
| `is_video_on`       | boolean | `true`  | Video on       |
| `is_screen_sharing` | boolean | `false` | Sharing screen |

### Key Endpoints

```bash
# Participant Management
GET    /api/v1/meetings/{id}/participants             # List
POST   /api/v1/participants/{id}/kick                 # Kick

# Waiting Room
GET    /api/v1/meetings/{id}/waiting                  # List
POST   /api/v1/meetings/{id}/waiting/{pid}/admit      # Admit
POST   /api/v1/meetings/{id}/waiting/{pid}/deny       # Deny

# Media Control
PATCH  /api/v1/participants/{id}/audio                # Mute
PATCH  /api/v1/participants/{id}/video                # Video
POST   /api/v1/meetings/{id}/screen-share/start       # Start share
POST   /api/v1/meetings/{id}/screen-share/stop        # Stop share
```

---

**Related Documentation:**

- [MEETINGS.md](./MEETINGS.md) - Meeting lifecycle management
- [DATABASE_SCHEMA.md](./DATABASE_SCHEMA.md) - Complete database schema
- [LIVEKIT.md](./LIVEKIT.md) - LiveKit integration details
