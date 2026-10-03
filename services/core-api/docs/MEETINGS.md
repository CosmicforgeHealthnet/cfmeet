# Meeting Management

Complete meeting lifecycle management with scheduling, real-time sessions, participant control, and automatic meeting termination.

## Table of Contents

- [Architecture](#architecture)
- [Components](#components)
- [Meeting Lifecycle](#meeting-lifecycle)
- [API Endpoints](#api-endpoints)
- [Meeting Identifier](#meeting-identifier)
- [Participant Management](#participant-management)
- [Session Logging](#session-logging)
- [Time-Based Scheduling](#time-based-scheduling)
- [Auto-End Worker](#auto-end-worker)
- [Validation Rules](#validation-rules)
- [Database Schema](#database-schema)
- [Security](#security)
- [Usage Examples](#usage-examples)
- [Monitoring](#monitoring)
- [Error Handling](#error-handling)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)
- [Quick Reference](#quick-reference)

## Architecture

```
┌─────────────────┐     ┌────────────────┐     ┌─────────────────┐
│   HTTP Client   │────▶│  Meeting API   │────▶│    Database     │
│  (Browser/App)  │     │  (Control)     │     │   (Meetings)    │
└─────────────────┘     └────────────────┘     └─────────────────┘
        │                       │                       │
        │                       ▼                       │
        │               ┌──────────────┐                │
        │               │ LiveKitService│               │
        │               │ (Join Token)  │               │
        │               └──────────────┘                │
        │                       │                       │
        │◀──────────────────────┼───────────────────────│
        │     Join Token + URL  │                       │
        │                       │                       │
        └───────────────────────┼───────────────────────▶
              WebRTC Media      │        LiveKit SFU
              Connection        │        (Audio/Video)
                                │
                                ▼
                        ┌──────────────┐
                        │  Auto-End    │
                        │   Worker     │
                        └──────────────┘
```

## Components

| Component          | Location                      | Purpose                       |
| ------------------ | ----------------------------- | ----------------------------- |
| `MeetingHandlers`  | `handlers/meetings.rs`        | HTTP request handlers         |
| `MeetingRoutes`    | `routes/meetings.rs`          | Route definitions             |
| `MeetingModel`     | `models/meetings.rs`          | Database entity               |
| `MeetingDTO`       | `dto/meetings.rs`             | Request/response types        |
| `ParticipantModel` | `models/participants.rs`      | Participant tracking          |
| `SessionLogs`      | `models/session_logs.rs`      | Event logging                 |
| `AutoEndWorker`    | `workers/meeting_auto_end.rs` | Automatic meeting termination |

## Folder Structure

```
backend/src/
├── handlers/
│   └── meetings.rs          # Meeting CRUD + join/end
├── routes/
│   └── meetings.rs          # Route configuration
├── models/
│   ├── meetings.rs          # Meeting entity
│   ├── participants.rs      # Participant entity
│   └── session_logs.rs      # Event logs
├── workers/
│   └── meeting_auto_end.rs  # Auto-end worker
└── dto/
    └── meetings.rs          # DTOs and validation
```

## Meeting Lifecycle

```
┌───────────┐     ┌─────────┐     ┌────────┐
│ SCHEDULED │────▶│ ONGOING │────▶│ ENDED  │
└───────────┘     └─────────┘     └────────┘
      │                                │
      ▼                                │
┌───────────┐                          │
│ CANCELLED │◀─────────────────────────┘
└───────────┘
```

### Status Transitions

| From        | To          | Trigger                     | Notes              |
| ----------- | ----------- | --------------------------- | ------------------ |
| `scheduled` | `ongoing`   | First participant joins     | Automatic          |
| `ongoing`   | `ended`     | Host ends OR scheduled time | Manual or Auto     |
| `scheduled` | `cancelled` | Host cancels                | Manual             |
| `scheduled` | Deleted     | Host deletes                | Only for scheduled |

**Update/Delete Restrictions:**

- ❌ Cannot update `ongoing` meetings (must end first)
- ❌ Cannot update `ended` meetings
- ❌ Cannot delete `ongoing` or `ended` meetings

## API Endpoints

### List Meetings (Protected)

```
GET /api/v1/meetings
```

**Query Parameters:**

- `page` (optional): Page number (default: 1)
- `limit` (optional): Items per page (default: 20, max: 100)
- `status` (optional): Filter by status (`scheduled`, `live`, `ended`, `cancelled`)

**Response:**

```json
{
  "success": true,
  "data": [
    {
      "id": "uuid",
      "meeting_identifier": "ABCD1234",
      "host_id": "uuid",
      "title": "Team Standup",
      "metadata": null,
      "is_private": false,
      "start_time": "2026-01-22T10:00:00Z",
      "end_time": "2026-01-22T11:00:00Z",
      "status": "scheduled",
      "created_at": "2026-01-21T09:00:00Z",
      "updated_at": "2026-01-21T09:00:00Z"
    }
  ],
  "meta": {
    "page": 1,
    "limit": 20,
    "total": 1,
    "total_pages": 1
  }
}
```

### Create Meeting (Protected)

```
POST /api/v1/meetings
```

**Request:**

```json
{
  "title": "Team Standup",
  "start_time": "2026-01-22T10:00:00Z",
  "end_time": "2026-01-22T11:00:00Z",
  "is_private": false,
  "metadata": {
    "agenda": "Sprint planning"
  }
}
```

**Validation Rules:**

- `title`: 1-200 characters (required)
- `start_time`: Must be at least 1 minute in the future (required)
- `end_time`: Must be after `start_time` (optional)
- `duration`: Cannot exceed 24 hours
- `is_private`: Boolean (optional, default: false)

**Response:**

```json
{
  "success": true,
  "data": {
    "id": "uuid",
    "meeting_identifier": "ABCD1234",
    "host_id": "uuid",
    "title": "Team Standup",
    "metadata": { "agenda": "Sprint planning" },
    "is_private": false,
    "start_time": "2026-01-22T10:00:00Z",
    "end_time": "2026-01-22T11:00:00Z",
    "status": "scheduled",
    "created_at": "2026-01-21T09:00:00Z",
    "updated_at": "2026-01-21T09:00:00Z"
  }
}
```

### Get Meeting (Protected)

```
GET /api/v1/meetings/{id}
```

**Path Parameters:**

- `id`: Meeting UUID

**Response:**

```json
{
  "success": true,
  "data": {
    "id": "uuid",
    "meeting_identifier": "ABCD1234",
    "host_id": "uuid",
    "title": "Team Standup",
    "metadata": null,
    "is_private": false,
    "start_time": "2026-01-22T10:00:00Z",
    "end_time": "2026-01-22T11:00:00Z",
    "status": "scheduled",
    "created_at": "2026-01-21T09:00:00Z",
    "updated_at": "2026-01-21T09:00:00Z"
  }
}
```

**Notes:**

- Only the host can retrieve their meetings
- Returns 404 if meeting not found or user is not the host

### Update Meeting (Protected)

```
PUT /api/v1/meetings/{id}
```

**Request:**

```json
{
  "title": "Updated Title",
  "start_time": "2026-01-22T11:00:00Z",
  "end_time": "2026-01-22T12:00:00Z",
  "is_private": true,
  "metadata": { "updated": true }
}
```

**Validation Rules:**

- All fields are optional (partial updates supported)
- `title`: 1-200 characters (if provided)
- `start_time`: Must be in future for scheduled meetings
- `end_time`: Must be after final `start_time`
- `duration`: Cannot exceed 24 hours
- **Cannot update ongoing meetings** (meetings that have already started)
- Cannot update ended meetings
- Only host can update

**Response:**

```json
{
  "success": true,
  "data": {
    "id": "uuid",
    "meeting_identifier": "ABCD1234",
    "host_id": "uuid",
    "title": "Updated Title",
    "metadata": { "updated": true },
    "is_private": true,
    "start_time": "2026-01-22T11:00:00Z",
    "end_time": "2026-01-22T12:00:00Z",
    "status": "scheduled",
    "created_at": "2026-01-21T09:00:00Z",
    "updated_at": "2026-01-21T10:00:00Z"
  }
}
```

### Delete Meeting (Protected)

```
DELETE /api/v1/meetings/{id}
```

**Response:**

- Status: `204 No Content`

**Notes:**

- Can only delete scheduled meetings
- Cannot delete ongoing or ended meetings
- Use "end meeting" for ongoing meetings
- Only host can delete

### Join Meeting by UUID (Public)

```
POST /api/v1/meetings/{id}/join
```

**Path Parameters:**

- `id`: Meeting UUID

**Request:**

```json
{
  "user_id": "uuid", // Optional - null for guests
  "display_name": "Jane Doe"
}
```

**Response:**

```json
{
  "success": true,
  "data": {
    "participant_id": "uuid",
    "role": "participant",
    "join_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "livekit_url": "wss://your-project.livekit.cloud",
    "room_name": "ABCD1234",
    "access_token": "eyJhbGci...",
    "refresh_token": "eyJhbGci..."
  }
}
```

**Notes:**

- No authentication required (public endpoint)
- Guests can join by omitting `user_id`
- Private meetings require a registered user
- First join transitions meeting from `scheduled` to `ongoing`
- Creates participant record in database
- Generates LiveKit join token
- Logs join event in session logs

### Join Meeting by Code (Public)

```
POST /api/v1/meetings/join/{meeting_identifier}
```

**Path Parameters:**

- `meeting_identifier`: 8-character meeting code (e.g., "ABCD1234")

**Request:**

```json
{
  "user_id": "uuid", // Optional - null for guests
  "display_name": "Jane Doe"
}
```

**Response:**

```json
{
  "success": true,
  "data": {
    "participant_id": "uuid",
    "role": "participant",
    "join_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "livekit_url": "wss://your-project.livekit.cloud",
    "room_name": "ABCD1234",
    "access_token": "eyJhbGci...",
    "refresh_token": "eyJhbGci..."
  }
}
```

**Notes:**

- **More user-friendly** - Uses the 8-character code instead of UUID
- Same validation and behavior as UUID-based join
- Perfect for sharing meeting links with participants
- Example: `https://yourapp.com/join/ABCD1234`
- Case-sensitive (use uppercase)

**Join Validation (Both Endpoints):**

- Meeting must exist
- Meeting cannot be ended or cancelled
- **Cannot join scheduled meetings before scheduled start time**
- **Cannot join after scheduled end time has passed**
- Private meetings require authenticated user
- If `user_id` is provided, the user must exist in the database

### End Meeting (Protected)

```
POST /api/v1/meetings/{id}/end
```

**Response:**

```json
{
  "success": true,
  "data": {
    "status": "ended"
  }
}
```

**Notes:**

- Only host can end meeting
- Updates meeting status to `ended`
- Sets `end_time` to current time (overrides scheduled end)
- Marks all active participants as left
- Logs meeting end event
- Cannot end already ended meetings

## Meeting Identifier

Each meeting has two IDs:

1. **UUID** (`id`): Internal database identifier
2. **Meeting Identifier** (`meeting_identifier`): Human-friendly 8-character code

### Identifier Generation

```rust
fn generate_meeting_identifier() -> String {
    // Uses characters: A-Z (excluding I, O) and 2-9
    // Example: "ABCD1234", "XYZ98765"
    // Collision probability: ~1 in 2.8 trillion
}
```

**Properties:**

- 8 characters long
- Uppercase letters and numbers only
- Excludes confusing characters (I, O, 0, 1)
- Easy to share verbally or in text
- Used as LiveKit room name
- Unique per meeting

**Use Cases:**

- Shareable meeting links: `https://app.com/join/ABCD1234`
- Verbal sharing: "Join meeting A-B-C-D-1-2-3-4"
- QR codes: Shorter codes = simpler QR codes
- User-friendly alternative to UUIDs

## Participant Management

### Participant Roles

| Role          | Permissions                                     | Assigned To            |
| ------------- | ----------------------------------------------- | ---------------------- |
| `host`        | Full control, can end meeting, admin in LiveKit | Meeting creator        |
| `participant` | Can publish/subscribe media                     | Regular users          |
| `viewer`      | Can only subscribe (future)                     | Read-only participants |

### Participant Lifecycle

```
┌────────┐     ┌────────┐     ┌──────┐
│ JOINED │────▶│ ACTIVE │────▶│ LEFT │
└────────┘     └────────┘     └──────┘
```

**Participant Record:**

```rust
{
    id: Uuid,
    meeting_id: Uuid,
    user_id: Option<Uuid>,      // NULL for guests
    role: ParticipantRole,
    display_name: String,
    status: ParticipantStatus,
    join_time: DateTime,
    leave_time: Option<DateTime>,
    is_muted: bool,
    is_video_on: bool,
    is_screen_sharing: bool,
}
```

## Session Logging

All meeting events are logged to `session_logs` table:

### Event Types

| Event                | Trigger                   | Metadata                           |
| -------------------- | ------------------------- | ---------------------------------- |
| `meeting_start`      | First participant joins   | `started_by`, `display_name`       |
| `participant_join`   | User joins meeting        | `display_name`, `role`, `is_guest` |
| `participant_leave`  | User leaves meeting       | `display_name`, `duration`         |
| `meeting_end`        | Host ends OR auto-end     | `ended_by_host`, `auto_ended`      |
| `media_toggle`       | Mute/unmute, video on/off | `action`, `media_type`             |
| `screen_share_start` | Screen sharing starts     | `participant_id`                   |
| `screen_share_end`   | Screen sharing ends       | `duration`                         |

### Log Structure

```rust
{
    id: Uuid,
    meeting_id: Uuid,
    participant_id: Option<Uuid>,
    event_type: EventType,
    event_time: DateTime,
    metadata: Option<JsonValue>,
}
```

## Time-Based Scheduling

### Join Time Restrictions

**Early Join Prevention:**

- Users cannot join scheduled meetings **before** the scheduled start time
- Applies only to meetings with status = `scheduled`
- Returns helpful error message with countdown to start time
- **Wait Time**: Users must wait until `start_time` is reached

**Late Join Prevention:**

- Users cannot join after the scheduled `end_time` has passed
- Prevents zombie participants joining expired meetings
- Meeting must be manually or automatically ended

### Example Scenarios

**Scenario 1: Too Early**

```
Meeting: start=10:00 AM
Current: 09:59 AM
Result: ❌ "Meeting hasn't started yet. Please try again in 1 minute."
```

**Scenario 2: Join Window**

```
Meeting: start=10:00 AM
Current: 10:00 AM
Result: ✅ Allowed - Meeting transitions to ongoing
```

**Scenario 3: After End Time**

```
Meeting: end=11:00 AM
Current: 11:05 AM
Result: ❌ "This meeting has passed its scheduled end time..."
```

## Auto-End Worker

### Overview

A background worker that automatically ends meetings at their scheduled end time.

**Configuration:**

- **Poll Interval**: 60 seconds (checks every minute)
- **Batch Size**: 20 meetings per batch
- **Always Running**: Starts automatically with the server

### Behavior

**Finds meetings with:**

- Status = `ongoing`
- `end_time` is not null
- `end_time` <= current time

**For each meeting:**

- Updates status to `ended`
- Marks all active participants as left
- Logs auto-end event with metadata
- Keeps the original scheduled `end_time`

### Auto-End Event Metadata

```json
{
  "auto_ended": true,
  "reason": "scheduled_end_time_reached",
  "ended_at": "2026-01-22T11:00:00Z"
}
```

### Worker Logs

```
✅ Meeting auto-end worker started
Meeting auto-end worker started (polling every 60s)
Auto-ending 2 meeting(s)
Auto-ended meeting abc123 (ABCD1234) at scheduled end time
```

**Note:** Meetings without an `end_time` will NOT be auto-ended and can run indefinitely until the host manually ends them.

## Validation Rules

### Create Meeting

```rust
// 1. Title validation (from DTO)
#[validate(length(min = 1, max = 200))]
pub title: String,

// 2. Start time validation
if payload.start_time <= now + Duration::minutes(1) {
    return Err("Start time must be at least 1 minute in the future");
}

// 3. Time range validation
if let Some(end_time) = payload.end_time {
    if end_time <= payload.start_time {
        return Err("End time must be after start time");
    }

    // 4. Duration validation
    let duration = end_time - payload.start_time;
    if duration > Duration::hours(24) {
        return Err("Meeting duration cannot exceed 24 hours");
    }
}
```

### Update Meeting

```rust
// 1. Status check
if meeting.status == MeetingStatus::Ended {
    return Err("Cannot update an ended meeting");
}

// 2. Ongoing check
if meeting.status == MeetingStatus::Ongoing {
    return Err("Cannot update a meeting that has already started");
}

// 3. Determine final times (handles partial updates)
let final_start_time = payload.start_time
    .unwrap_or_else(|| naive_to_utc(meeting.start_time));
let final_end_time = payload.end_time
    .or_else(|| meeting.end_time.map(naive_to_utc));

// 4. Future time validation (scheduled meetings only)
if meeting.status == MeetingStatus::Scheduled && final_start_time <= now {
    return Err("Start time must be in the future for scheduled meetings");
}

// 5. Time range validation
if let Some(end_time) = final_end_time {
    if end_time <= final_start_time {
        return Err("End time must be after start time");
    }

    let duration = end_time - final_start_time;
    if duration > Duration::hours(24) {
        return Err("Meeting duration cannot exceed 24 hours");
    }
}
```

### Join Meeting

```rust
// 1. Meeting status check
if meeting.status == MeetingStatus::Ended {
    return Err("Meeting has ended");
}

// 2. Early join prevention (scheduled meetings only)
if meeting.status == MeetingStatus::Scheduled {
    if now < start_time {
        return Err("Meeting hasn't started yet");
    }
}

// 3. Late join prevention
if let Some(end_time) = meeting.end_time {
    if now > end_time {
        return Err("Meeting has passed its scheduled end time");
    }
}

// 4. Private meeting check
if meeting.is_private && payload.user_id.is_none() {
    return Err("Private meeting - guests not allowed");
}

// 5. User existence check
if let Some(user_id) = payload.user_id {
    if !user_exists(user_id) {
        return Err("User not found");
    }
}
```

## Database Schema

### Meetings Table

```sql
CREATE TABLE meetings (
    id UUID PRIMARY KEY,
    meeting_identifier VARCHAR(8) UNIQUE NOT NULL,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    host_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title VARCHAR(200) NOT NULL,
    metadata JSONB,
    is_private BOOLEAN NOT NULL DEFAULT false,
    start_time TIMESTAMP NOT NULL,
    end_time TIMESTAMP,
    status meeting_status NOT NULL DEFAULT 'scheduled',
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL
);

CREATE TYPE meeting_status AS ENUM (
    'scheduled',
    'ongoing',
    'ended',
    'cancelled'
);

-- Indexes
CREATE INDEX idx_meetings_host_id ON meetings(host_id);
CREATE INDEX idx_meetings_status ON meetings(status);
CREATE INDEX idx_meetings_start_time ON meetings(start_time);
CREATE INDEX idx_meetings_end_time ON meetings(end_time);
CREATE UNIQUE INDEX idx_meetings_identifier ON meetings(meeting_identifier);
```

### Participants Table

```sql
CREATE TABLE participants (
    id UUID PRIMARY KEY,
    meeting_id UUID NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    role participant_role NOT NULL,
    display_name VARCHAR(100) NOT NULL,
    status participant_status NOT NULL DEFAULT 'joined',
    join_time TIMESTAMP NOT NULL,
    leave_time TIMESTAMP,
    is_muted BOOLEAN NOT NULL DEFAULT false,
    is_video_on BOOLEAN NOT NULL DEFAULT true,
    is_screen_sharing BOOLEAN NOT NULL DEFAULT false
);

CREATE TYPE participant_role AS ENUM ('host', 'participant', 'viewer');
CREATE TYPE participant_status AS ENUM ('joined', 'active', 'left');

-- Indexes
CREATE INDEX idx_participants_meeting_id ON participants(meeting_id);
CREATE INDEX idx_participants_user_id ON participants(user_id);
CREATE INDEX idx_participants_status ON participants(status);
```

## Security

### Authentication

| Endpoint                                   | Auth Required | Notes                     |
| ------------------------------------------ | ------------- | ------------------------- |
| `GET /meetings`                            | ✅ Yes        | Bearer token              |
| `POST /meetings`                           | ✅ Yes        | Bearer token              |
| `GET /meetings/{id}`                       | ✅ Yes        | Bearer token              |
| `PUT /meetings/{id}`                       | ✅ Yes        | Bearer token + host check |
| `DELETE /meetings/{id}`                    | ✅ Yes        | Bearer token + host check |
| `POST /meetings/{id}/join`                 | ❌ No         | Public (guests allowed)   |
| `POST /meetings/join/{meeting_identifier}` | ❌ No         | Public (guests allowed)   |
| `POST /meetings/{id}/end`                  | ✅ Yes        | Bearer token + host check |

### Authorization

```rust
// Host-only operations
if meeting.host_id != user_id {
    return Err(ApiError::Forbidden("Only the host can perform this action"));
}
```

### Private Meetings

```rust
// Private meeting validation on join
if meeting.is_private && payload.user_id.is_none() {
    return Err(ApiError::Forbidden("This is a private meeting. Guests cannot join."));
}
```

## Usage Examples

### Creating a Meeting

```bash
curl -X POST 'http://localhost:8080/api/v1/meetings' \
  -H 'Authorization: Bearer YOUR_TOKEN' \
  -H 'Content-Type: application/json' \
  -d '{
    "title": "Team Standup",
    "start_time": "2026-01-22T10:00:00Z",
    "end_time": "2026-01-22T11:00:00Z",
    "is_private": false
  }'
```

### Joining as Guest

```bash
curl -X POST 'http://localhost:8080/api/v1/meetings/MEETING_ID/join' \
  -H 'Content-Type: application/json' \
  -d '{
    "display_name": "Guest User"
  }'
```

### Joining by Meeting Code

```bash
curl -X POST 'http://localhost:8080/api/v1/meetings/join/ABCD1234' \
  -H 'Content-Type: application/json' \
  -d '{
    "display_name": "Guest User"
  }'
```

### Joining as Authenticated User

```bash
curl -X POST 'http://localhost:8080/api/v1/meetings/MEETING_ID/join' \
  -H 'Content-Type: application/json' \
  -d '{
    "user_id": "USER_UUID",
    "display_name": "Jane Doe"
  }'
```

### Updating a Meeting

```bash
curl -X PUT 'http://localhost:8080/api/v1/meetings/MEETING_ID' \
  -H 'Authorization: Bearer YOUR_TOKEN' \
  -H 'Content-Type: application/json' \
  -d '{
    "title": "Updated Title",
    "start_time": "2026-01-22T11:00:00Z"
  }'
```

### Ending a Meeting

```bash
curl -X POST 'http://localhost:8080/api/v1/meetings/MEETING_ID/end' \
  -H 'Authorization: Bearer YOUR_TOKEN'
```

## Monitoring

### Check Active Meetings

```sql
SELECT id, meeting_identifier, title, status, start_time, end_time
FROM meetings
WHERE status = 'ongoing'
ORDER BY start_time DESC;
```

### Check Meetings to Auto-End

```sql
SELECT id, meeting_identifier, title, end_time, status
FROM meetings
WHERE status = 'ongoing'
  AND end_time IS NOT NULL
  AND end_time <= NOW()
ORDER BY end_time ASC;
```

### Check Participant Count

```sql
SELECT m.meeting_identifier, m.title, COUNT(p.id) as participant_count
FROM meetings m
LEFT JOIN participants p ON m.id = p.meeting_id AND p.leave_time IS NULL
WHERE m.status = 'ongoing'
GROUP BY m.id, m.meeting_identifier, m.title;
```

### Check Meeting Duration

```sql
SELECT
    meeting_identifier,
    title,
    start_time,
    end_time,
    EXTRACT(EPOCH FROM (end_time - start_time))/60 as duration_minutes
FROM meetings
WHERE status = 'ended'
ORDER BY end_time DESC
LIMIT 10;
```

### Session Event Logs

```sql
SELECT
    sl.event_type,
    sl.event_time,
    sl.metadata,
    m.meeting_identifier
FROM session_logs sl
JOIN meetings m ON sl.meeting_id = m.id
WHERE m.id = 'MEETING_UUID'
ORDER BY sl.event_time ASC;
```

### Check Auto-End Events

```sql
SELECT
    m.meeting_identifier,
    m.title,
    sl.event_time,
    sl.metadata
FROM session_logs sl
JOIN meetings m ON sl.meeting_id = m.id
WHERE sl.event_type = 'meeting_end'
  AND sl.metadata->>'auto_ended' = 'true'
ORDER BY sl.event_time DESC
LIMIT 10;
```

## Error Handling

### Common Errors

| Error Code | Scenario                | Message                                                                                         |
| ---------- | ----------------------- | ----------------------------------------------------------------------------------------------- |
| 400        | Invalid start time      | "Start time must be at least 1 minute in the future"                                            |
| 400        | Invalid time range      | "End time must be after start time"                                                             |
| 400        | Duration too long       | "Meeting duration cannot exceed 24 hours"                                                       |
| 400        | Invalid user_id on join | "User not found. Please provide a valid user_id or join as a guest."                            |
| 401        | No auth token           | "Unauthorized"                                                                                  |
| 403        | Not the host            | "Only the host can perform this action"                                                         |
| 403        | Guest joining private   | "This is a private meeting. Guests cannot join."                                                |
| 404        | Meeting not found       | "Meeting not found" or "Meeting with code 'ABCD1234' not found"                                 |
| 409        | Too early to join       | "Meeting hasn't started yet. You can join up to 15 minutes before..."                           |
| 409        | Too late to join        | "This meeting has passed its scheduled end time and is no longer accepting participants."       |
| 409        | Meeting ended           | "Meeting has ended"                                                                             |
| 409        | Meeting cancelled       | "Meeting has been cancelled"                                                                    |
| 409        | Update ongoing meeting  | "Cannot update a meeting that has already started. Please end the meeting first if you need..." |
| 409        | Update ended meeting    | "Cannot update an ended meeting"                                                                |
| 409        | Delete ongoing/ended    | "Can only delete scheduled meetings. Use cancel or end instead."                                |

## Best Practices

### 1. Always Validate Times

```rust
// DO: Validate both individual times and the range
if end_time <= start_time {
    return Err("Invalid time range");
}

// DON'T: Assume client sends valid times
```

### 2. Handle Partial Updates

```rust
// DO: Determine final values before validation
let final_start = payload.start_time.unwrap_or(existing.start_time);
let final_end = payload.end_time.or(existing.end_time);

// DON'T: Validate only the new values
```

### 3. Log Important Events

```rust
// DO: Log meeting lifecycle events
session_logs::ActiveModel {
    event_type: Set(EventType::MeetingStart),
    metadata: Set(Some(json!({"started_by": participant_id}))),
    // ...
}.insert(&db).await?;
```

### 4. Use Transactions for State Changes

```rust
// DO: Use transactions for multi-step operations
let txn = db.begin().await?;
// Update meeting status
// Create participant record
// Log event
txn.commit().await?;
```

### 5. Use Meeting Identifiers for User-Facing Features

```rust
// DO: Share meeting codes with users
let share_url = format!("https://app.com/join/{}", meeting.meeting_identifier);

// DON'T: Expose UUIDs in user-facing URLs
let share_url = format!("https://app.com/join/{}", meeting.id);
```

### 6. Set Reasonable End Times

```rust
// DO: Set explicit end times for auto-end functionality
{
  "start_time": "2026-01-22T10:00:00Z",
  "end_time": "2026-01-22T11:00:00Z"  // Will auto-end at 11:00
}

// DON'T: Leave end_time null unless intentionally indefinite
{
  "start_time": "2026-01-22T10:00:00Z",
  "end_time": null  // Will run until manually ended
}
```

## Troubleshooting

### Meeting Not Found (404)

**Cause**: Route parameter syntax was incorrect (`{id}` instead of `:id`)

**Solution**: Routes now use `:id` syntax:

```rust
.route("/:id", get(meetings::get_meeting))
```

### Cannot Update Meeting Times

**Cause**: Meeting has already started (status = `ongoing`)

**Solution**:

- End the meeting first
- Create a new meeting with updated details
- Updates are only allowed for `scheduled` meetings

### Cannot Join - Too Early

**Cause**: Trying to join more than 15 minutes before start time

**Solution**: Wait until within 15 minutes of the scheduled start time

### Cannot Join - Too Late

**Cause**: Trying to join after the scheduled end time

**Solution**: Meeting has ended. Contact the host to create a new meeting.

### Guest Cannot Join Private Meeting

**Expected Behavior**: Private meetings require authenticated users

**Solution**: Either:

1. Make the meeting public (`is_private: false`)
2. Join with a registered user account

### Foreign Key Constraint Error on Join (500)

**Error Message**: `"insert or update on table \"participants\" violates foreign key constraint \"fk_participants_user\""`

**Cause**: Attempting to join with a `user_id` that doesn't exist in the database

**Solution**:

1. **Join as a guest** (omit `user_id`):

   ```json
   {
     "display_name": "Guest User"
   }
   ```

2. **Use a valid user_id** from an existing user account

3. **Verify the user exists**:
   ```sql
   SELECT id, username, email FROM users WHERE id = 'YOUR_USER_ID';
   ```

**Note**: This validation is now handled server-side and returns a proper 400 error instead of 500.

### Meeting Not Auto-Ending

**Cause**: Meeting doesn't have an `end_time` set

**Solution**:

- Set an `end_time` when creating the meeting
- Or manually end the meeting using the end endpoint

**Check Worker Status**:

```bash
# Look for worker logs
grep "Meeting auto-end worker" logs/app.log
```

## Quick Reference

### Meeting Status Values

| Status      | Description       | Can Join? | Can Update? | Can Delete? |
| ----------- | ----------------- | --------- | ----------- | ----------- |
| `scheduled` | Not started yet   | ✅ Yes\*  | ✅ Yes      | ✅ Yes      |
| `ongoing`   | Currently active  | ✅ Yes\*  | ❌ No       | ❌ No       |
| `ended`     | Finished          | ❌ No     | ❌ No       | ❌ No       |
| `cancelled` | Cancelled by host | ❌ No     | ❌ No       | ❌ No       |

\*Subject to time restrictions (15 min before start, not after end)

### Participant Roles

| Role          | Can Publish | Can Subscribe | Can End Meeting |
| ------------- | ----------- | ------------- | --------------- |
| `host`        | ✅ Yes      | ✅ Yes        | ✅ Yes          |
| `participant` | ✅ Yes      | ✅ Yes        | ❌ No           |
| `viewer`      | ❌ No       | ✅ Yes        | ❌ No           |

### Time Constraints

| Constraint        | Value      | Applies To      |
| ----------------- | ---------- | --------------- |
| Min future time   | 1 minute   | Create meeting  |
| Max duration      | 24 hours   | Create/Update   |
| Early join window | 15 minutes | Join meeting    |
| Late join cutoff  | end_time   | Join meeting    |
| Auto-end check    | 60 seconds | Worker interval |

### Modified Files (Implementation History)

| File                              | Purpose                              |
| --------------------------------- | ------------------------------------ |
| `src/handlers/meetings.rs`        | Join validation, update restrictions |
| `src/routes/meetings.rs`          | Route parameter fix, join by code    |
| `src/workers/meeting_auto_end.rs` | Auto-end worker implementation       |
| `src/workers/mod.rs`              | Worker module exports                |
| `src/main.rs`                     | Worker startup                       |
| `docs/MEETINGS.md`                | This comprehensive documentation     |

---

**Last Updated**: 2026-01-21  
**Version**: 2.0  
**Status**: Production Ready

For LiveKit integration details, see [LIVEKIT.md](./LIVEKIT.md)  
For email notifications, see [EMAIL_QUEUE.md](./EMAIL_QUEUE.md)
