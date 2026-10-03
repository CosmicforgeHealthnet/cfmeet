# LiveKit Integration

Real-time video/audio communication using LiveKit for WebRTC media handling.

## Architecture

```
┌─────────────────┐     ┌────────────────┐     ┌─────────────────┐
│   HTTP Client   │────▶│ CosmicForge API│────▶│  LiveKit Cloud  │
│  (Browser/App)  │     │   (Control)    │     │   (Media SFU)   │
└─────────────────┘     └────────────────┘     └─────────────────┘
        │                       │                       │
        │                       │ Generate Token        │
        │                       ▼                       │
        │               ┌──────────────┐                │
        │               │ LiveKitService│               │
        │               │ (Token Gen)   │               │
        │               └──────────────┘                │
        │                       │                       │
        │◀──────────────────────┼───────────────────────│
        │     Join Token + URL  │                       │
        │                       │                       │
        └───────────────────────┼───────────────────────▶
              WebRTC Media      │        (SFU handles
              Connection        │         audio/video)
```

## How It Works

1. **Control Plane (CosmicForge API)**: Handles meeting creation, participant management, authentication
2. **Data Plane (LiveKit)**: Handles actual audio/video streaming via WebRTC SFU
3. **Join Token**: Short-lived credential that authorizes a participant to connect to LiveKit

**Key Principle**: HTTP APIs control state, LiveKit handles media. They never mix.

## Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `LiveKitConfig` | `config/livekit.rs` | Configuration from environment |
| `LiveKitService` | `services/livekit.rs` | Token generation and room management |
| `JoinMeetingResponse` | `dto/meetings.rs` | Returns token to client |
| Meeting Handlers | `handlers/meetings.rs` | Integrates LiveKit on join |

## Folder Structure

```
backend/src/
├── config/
│   ├── mod.rs           # Exports LiveKitConfig
│   └── livekit.rs       # LiveKit configuration
├── services/
│   ├── mod.rs           # Exports livekit module
│   └── livekit.rs       # LiveKitService implementation
├── dto/
│   └── meetings.rs      # JoinMeetingResponse with token
├── handlers/
│   └── meetings.rs      # join_meeting generates token
└── state.rs             # AppState includes livekit_service
```

## Configuration

### Environment Variables

```env
# LiveKit Configuration (Required)
LIVEKIT_URL=wss://your-project.livekit.cloud
LIVEKIT_API_KEY=APIxxxxxxxxxxxxxxx
LIVEKIT_API_SECRET=your-api-secret-here
```

### Getting LiveKit Credentials

**Option 1: LiveKit Cloud (Recommended for Development)**

1. Sign up at [https://cloud.livekit.io](https://cloud.livekit.io)
2. Create a new project
3. Copy the API Key and API Secret from the dashboard
4. Use the WebSocket URL provided (e.g., `wss://your-project.livekit.cloud`)

**Option 2: Self-Hosted LiveKit**

1. Deploy LiveKit server using Docker or Kubernetes
2. Generate API keys using the LiveKit CLI
3. Configure your server URL

## Usage

### Token Generation (Internal)

The `LiveKitService` generates tokens for participants:

```rust
// In join_meeting handler
let join_token = state.livekit_service.generate_join_token(
    room_name,           // Meeting identifier (e.g., "ABCD1234")
    participant_identity, // Unique ID (participant UUID)
    participant_name,     // Display name
    is_host,             // Grants admin permissions if true
)?;
```

### Join Meeting Response

When a client calls `POST /meetings/{id}/join`, they receive:

```json
{
  "success": true,
  "data": {
    "participant_id": "uuid",
    "role": "participant",
    "join_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "livekit_url": "wss://your-project.livekit.cloud",
    "room_name": "ABCD1234"
  }
}
```

### Client-Side Connection

The client uses the token to connect to LiveKit:

```javascript
// JavaScript/TypeScript example
import { Room } from 'livekit-client';

const room = new Room();

await room.connect(livekit_url, join_token);

// Now connected - can publish/subscribe to tracks
await room.localParticipant.enableCameraAndMicrophone();
```

## Token Permissions

### Participant Token (Default)

```rust
VideoGrants {
    room_join: true,
    room: "ROOM_NAME",
    can_publish: true,
    can_subscribe: true,
    can_publish_data: true,
    room_admin: false,      // Cannot kick/mute others
    room_record: false,     // Cannot start recording
}
```

### Host Token (is_host = true)

```rust
VideoGrants {
    room_join: true,
    room: "ROOM_NAME",
    can_publish: true,
    can_subscribe: true,
    can_publish_data: true,
    room_admin: true,       // Can kick/mute participants
    room_record: true,      // Can start recording
}
```

## Token Lifecycle

```
┌─────────────────────────────────────────────────────────────────┐
│                        Token Lifecycle                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. User calls POST /meetings/{id}/join                         │
│                      │                                          │
│                      ▼                                          │
│  2. Server validates meeting (not ended, not cancelled)         │
│                      │                                          │
│                      ▼                                          │
│  3. Server creates Participant record in database               │
│                      │                                          │
│                      ▼                                          │
│  4. LiveKitService generates JWT token (6 hour TTL)             │
│                      │                                          │
│                      ▼                                          │
│  5. Token returned to client with LiveKit URL                   │
│                      │                                          │
│                      ▼                                          │
│  6. Client connects to LiveKit using token                      │
│                      │                                          │
│                      ▼                                          │
│  7. Token expires after 6 hours (or meeting ends)               │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Room Naming Convention

- **Room Name** = `meeting_identifier` (8-character code like `ABCD1234`)
- **Participant Identity** = `participant_id` (UUID)
- **Participant Name** = `display_name` (user-provided)

This ensures:
- Rooms are easy to share (short codes)
- Participants are uniquely identifiable
- Display names can be customized

## Security Considerations

### Token Security

1. **Short-lived**: Tokens have a 6-hour TTL by default
2. **Room-scoped**: Each token only works for one specific room
3. **Identity-bound**: Token is tied to a specific participant identity
4. **Server-side only**: API secret never leaves the server

### Best Practices

```rust
// DO: Generate token server-side
let token = state.livekit_service.generate_join_token(...)?;

// DON'T: Never expose API secret to clients
// DON'T: Never generate tokens client-side
// DON'T: Never use long-lived tokens
```

### Meeting State Enforcement

Tokens are only generated for valid meetings:

```rust
// Check meeting state before generating token
if meeting.status == MeetingStatus::Ended {
    return Err(ApiError::Conflict("Meeting has ended"));
}

if meeting.status == MeetingStatus::Cancelled {
    return Err(ApiError::Conflict("Meeting has been cancelled"));
}
```

## API Endpoints

### Join Meeting (Public - No Auth Required)

```
POST /api/v1/meetings/{id}/join
```

**Request:**
```json
{
  "user_id": "uuid",        // Optional - null for guests
  "display_name": "JaneDoe"
}
```

**Response:**
```json
{
  "success": true,
  "data": {
    "participant_id": "550e8400-e29b-41d4-a716-446655440000",
    "role": "participant",
    "join_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "livekit_url": "wss://your-project.livekit.cloud",
    "room_name": "ABCD1234"
  }
}
```

**Notes:**
- Guests can join by omitting `user_id`
- Private meetings require a registered user
- First join transitions meeting from `scheduled` to `live`

## LiveKit SDK Integration

### Supported Client SDKs

| Platform | SDK |
|----------|-----|
| Web | `livekit-client` (JavaScript/TypeScript) |
| React | `@livekit/components-react` |
| iOS | `livekit-ios` |
| Android | `livekit-android` |
| Flutter | `livekit_client` |
| Unity | `livekit-unity` |
| Rust | `livekit-rust` |

### Example: React Integration

```tsx
import { LiveKitRoom, VideoConference } from '@livekit/components-react';

function MeetingRoom({ joinToken, livekitUrl }) {
  return (
    <LiveKitRoom
      token={joinToken}
      serverUrl={livekitUrl}
      connect={true}
    >
      <VideoConference />
    </LiveKitRoom>
  );
}
```

### Example: Vanilla JavaScript

```javascript
import { Room, RoomEvent } from 'livekit-client';

async function joinMeeting(meetingId, displayName) {
  // 1. Get token from CosmicForge API
  const response = await fetch(`/api/v1/meetings/${meetingId}/join`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ display_name: displayName })
  });

  const { data } = await response.json();

  // 2. Connect to LiveKit
  const room = new Room();

  room.on(RoomEvent.TrackSubscribed, (track, publication, participant) => {
    // Handle new track from remote participant
    const element = track.attach();
    document.getElementById('video-container').appendChild(element);
  });

  await room.connect(data.livekit_url, data.join_token);

  // 3. Publish local tracks
  await room.localParticipant.enableCameraAndMicrophone();

  return room;
}
```

## Troubleshooting

### Token Generation Fails

```
Error: Failed to generate LiveKit token
```

**Causes:**
- Missing or invalid `LIVEKIT_API_KEY`
- Missing or invalid `LIVEKIT_API_SECRET`

**Solution:**
```bash
# Verify environment variables are set
echo $LIVEKIT_API_KEY
echo $LIVEKIT_API_SECRET
```

### Client Cannot Connect

```
Error: Connection failed
```

**Causes:**
- Invalid `LIVEKIT_URL`
- Token expired
- Network/firewall issues

**Solution:**
1. Verify the LiveKit URL is correct (should start with `wss://`)
2. Check token expiration (6-hour default TTL)
3. Ensure WebSocket connections are allowed

### Room Not Found

LiveKit creates rooms automatically on first join. If you get "room not found":

1. Check that the `room_name` matches between token and connection
2. Verify the meeting exists and is not ended

## Monitoring

### Server Logs

```rust
// Token generation is logged
tracing::info!("Generated LiveKit token for participant {}", participant_id);
```

### LiveKit Dashboard

Monitor room activity in the LiveKit Cloud dashboard:
- Active rooms
- Participant count
- Track statistics
- Connection quality

## Future Enhancements

Potential improvements to the LiveKit integration:

1. **Recording**: Integrate LiveKit Egress for meeting recordings
2. **Webhooks**: Handle LiveKit webhooks for participant events
3. **Room Service**: Pre-create rooms or configure room settings
4. **SIP Integration**: Allow phone dial-in to meetings
5. **Transcription**: Integrate LiveKit transcription services

---

## Quick Reference

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `LIVEKIT_URL` | Yes | WebSocket URL (wss://...) |
| `LIVEKIT_API_KEY` | Yes | API key from LiveKit |
| `LIVEKIT_API_SECRET` | Yes | API secret from LiveKit |

### Token Settings

| Setting | Value | Location |
|---------|-------|----------|
| TTL | 6 hours | `services/livekit.rs:TOKEN_TTL_SECS` |
| Service Token TTL | 60 seconds | `services/livekit.rs` |

### Files Modified/Created

| File | Purpose |
|------|---------|
| `config/livekit.rs` | Configuration |
| `services/livekit.rs` | Token generation |
| `dto/meetings.rs` | Response types |
| `handlers/meetings.rs` | Join integration |
| `state.rs` | Service registration |
| `.env.example` | Environment template |

---

**Last Updated**: 2026-01-19
