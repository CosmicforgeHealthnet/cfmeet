# Chat System

Real-time in-meeting chat with hybrid architecture using LiveKit Data Channels for instant delivery and REST API for message persistence.

## Table of Contents

- [Architecture](#architecture)
- [Components](#components)
- [Message Lifecycle](#message-lifecycle)
- [API Endpoints](#api-endpoints)
- [Frontend Implementation](#frontend-implementation)
- [LiveKit Data Channels](#livekit-data-channels)
- [Database Schema](#database-schema)
- [Security](#security)
- [Usage Examples](#usage-examples)
- [Error Handling](#error-handling)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)
- [Quick Reference](#quick-reference)

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              FRONTEND                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌─────────────┐         ┌──────────────────┐         ┌─────────────────┐ │
│   │  Send Msg   │────────▶│ 1. POST /chat    │────────▶│ Save to DB      │ │
│   │  Button     │         │    (Persist)     │         │ Get message ID  │ │
│   └─────────────┘         └──────────────────┘         └────────┬────────┘ │
│                                                                  │          │
│                                                                  ▼          │
│                           ┌──────────────────┐         ┌─────────────────┐ │
│                           │ 2. publishData() │◀────────│ Saved message   │ │
│                           │    (Broadcast)   │         │ with ID         │ │
│                           └────────┬─────────┘         └─────────────────┘ │
│                                    │                                        │
│                                    ▼                                        │
│   ┌─────────────┐         ┌──────────────────┐                             │
│   │  Chat UI    │◀────────│ DataReceived     │  (Other participants)       │
│   │  Display    │         │ Event            │                             │
│   └─────────────┘         └──────────────────┘                             │
│                                                                             │
│   ┌─────────────┐         ┌──────────────────┐         ┌─────────────────┐ │
│   │ Late Joiner │────────▶│ GET /chat        │────────▶│ Load history    │ │
│   │             │         │ (History)        │         │ from DB         │ │
│   └─────────────┘         └──────────────────┘         └─────────────────┘ │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                              BACKEND                                         │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌──────────────────┐    ┌──────────────────┐    ┌──────────────────────┐ │
│   │ POST /chat       │    │ GET /chat        │    │ Meeting End          │ │
│   │ - Validate       │    │ - Load messages  │    │ - Delete all msgs    │ │
│   │ - Store in DB    │    │ - Filter by time │    │ - Volatile cleanup   │ │
│   │ - Return saved   │    │ - Include names  │    │                      │ │
│   └──────────────────┘    └──────────────────┘    └──────────────────────┘ │
│            │                       │                        │               │
│            └───────────────────────┼────────────────────────┘               │
│                                    ▼                                        │
│                          ┌──────────────────┐                               │
│                          │   PostgreSQL     │                               │
│                          │   chat_messages  │                               │
│                          └──────────────────┘                               │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                           LIVEKIT SFU                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   publishData() ──────▶ Data Channel ──────▶ DataReceived (all clients)    │
│                                                                             │
│   - Reliable delivery (TCP-like)                                            │
│   - Instant broadcast to all participants                                   │
│   - No server processing needed                                             │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Components

| Component         | Location                      | Purpose                     |
| ----------------- | ----------------------------- | --------------------------- |
| `ChatHandlers`    | `handlers/chat.rs`            | HTTP request handlers       |
| `ChatRoutes`      | `routes/meetings.rs`          | Route definitions           |
| `ChatModel`       | `models/chat_messages.rs`     | Database entity             |
| `ChatDTO`         | `dto/meetings.rs`             | Request/response types      |
| `MeetingHandlers` | `handlers/meetings.rs`        | Message cleanup on end      |
| `AutoEndWorker`   | `workers/meeting_auto_end.rs` | Message cleanup on auto-end |

## Folder Structure

```
backend/src/
├── handlers/
│   ├── chat.rs              # Send and get messages
│   └── meetings.rs          # Message cleanup on end_meeting
├── routes/
│   └── meetings.rs          # Chat routes (/:id/chat)
├── models/
│   └── chat_messages.rs     # ChatMessage entity
├── workers/
│   └── meeting_auto_end.rs  # Cleanup on auto-end
└── dto/
    └── meetings.rs          # Chat DTOs
```

## Message Lifecycle

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   CREATED    │────▶│   STORED     │────▶│   DELETED    │
│  (POST /chat)│     │  (In DB)     │     │ (Meeting End)│
└──────────────┘     └──────────────┘     └──────────────┘
```

### Lifecycle Events

| Event           | Trigger                   | Action                        |
| --------------- | ------------------------- | ----------------------------- |
| Message Created | POST /chat                | Store in database             |
| Message Read    | GET /chat                 | Return from database          |
| Messages Deleted| Meeting ends (manual)     | DELETE all meeting messages   |
| Messages Deleted| Meeting ends (auto)       | DELETE all meeting messages   |

### Volatile Per Session

Messages are **intentionally volatile** - they exist only during the meeting session:

- ✅ Available while meeting is `ongoing`
- ✅ Persisted for late joiners to see history
- ❌ Deleted when meeting ends (manual or auto)
- ❌ Not available after meeting ends

## API Endpoints

### Send Message (Protected)

```
POST /api/v1/meetings/{meeting_id}/chat
```

**Authorization**: Bearer token required

**Request:**

```json
{
  "participant_id": "uuid",
  "message": "Hello everyone!"
}
```

**Validation Rules:**

- `participant_id`: Must be a valid participant in this meeting with status `joined`
- `message`: 1-2000 characters (required)
- Meeting must be `ongoing` (live)
- Authenticated user must own the participant record

**Response (201 Created):**

```json
{
  "success": true,
  "data": {
    "id": "msg-uuid",
    "participant_id": "participant-uuid",
    "display_name": "John Doe",
    "message": "Hello everyone!",
    "created_at": "2026-01-22T10:30:00Z"
  }
}
```

**Errors:**

| Code | Error                | Cause                                    |
| ---- | -------------------- | ---------------------------------------- |
| 400  | Validation error     | Message empty or > 2000 chars            |
| 401  | Unauthorized         | Missing or invalid token                 |
| 403  | Forbidden            | Not a joined participant / wrong user    |
| 404  | Not Found            | Meeting or participant not found         |
| 409  | Conflict             | Meeting is not live (not `ongoing`)      |

---

### Get Messages (Protected)

```
GET /api/v1/meetings/{meeting_id}/chat
```

**Authorization**: Bearer token required

**Query Parameters:**

| Parameter | Type     | Default | Max  | Description                          |
| --------- | -------- | ------- | ---- | ------------------------------------ |
| `after`   | ISO 8601 | -       | -    | Only return messages after this time |
| `limit`   | integer  | 100     | 500  | Max messages to return               |

**Response (200 OK):**

```json
{
  "success": true,
  "data": [
    {
      "id": "msg-uuid-1",
      "participant_id": "participant-uuid",
      "display_name": "John Doe",
      "message": "Hello everyone!",
      "created_at": "2026-01-22T10:30:00Z"
    },
    {
      "id": "msg-uuid-2",
      "participant_id": "participant-uuid-2",
      "display_name": "Jane Smith",
      "message": "Hi John!",
      "created_at": "2026-01-22T10:30:15Z"
    }
  ]
}
```

**Notes:**

- Messages ordered by `created_at` ascending (oldest first)
- Only meeting participants or host can view messages
- Use `after` parameter to avoid duplicate messages when polling

**Errors:**

| Code | Error        | Cause                              |
| ---- | ------------ | ---------------------------------- |
| 401  | Unauthorized | Missing or invalid token           |
| 403  | Forbidden    | Not a participant or host          |
| 404  | Not Found    | Meeting not found                  |

---

## Frontend Implementation

### Complete Setup Flow

```javascript
import { Room, RoomEvent } from 'livekit-client';

// ============================================================================
// STEP 1: Join Meeting and Connect to LiveKit
// ============================================================================

async function joinMeeting(meetingId, displayName, authToken) {
  // 1.1 Join via REST API
  const joinResponse = await fetch(`/api/v1/meetings/${meetingId}/join`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${authToken}`
    },
    body: JSON.stringify({
      user_id: currentUserId,  // or omit for guest
      display_name: displayName
    })
  });

  const { data } = await joinResponse.json();
  const { participant_id, join_token, livekit_url, room_name } = data;

  // 1.2 Connect to LiveKit
  const room = new Room();
  await room.connect(livekit_url, join_token);

  return { room, participant_id, room_name };
}

// ============================================================================
// STEP 2: Load Chat History (For Late Joiners)
// ============================================================================

async function loadChatHistory(meetingId, authToken) {
  const response = await fetch(
    `/api/v1/meetings/${meetingId}/chat?limit=500`,
    {
      headers: { 'Authorization': `Bearer ${authToken}` }
    }
  );

  const { data: messages } = await response.json();
  return messages;
}

// ============================================================================
// STEP 3: Listen for Real-Time Messages
// ============================================================================

function setupChatListener(room, onMessageReceived) {
  room.on(RoomEvent.DataReceived, (payload, participant) => {
    try {
      const decoder = new TextDecoder();
      const data = JSON.parse(decoder.decode(payload));

      if (data.type === 'chat_message') {
        onMessageReceived({
          id: data.id,
          participant_id: data.participant_id,
          display_name: data.display_name,
          message: data.message,
          created_at: data.created_at
        });
      }
    } catch (error) {
      console.error('Failed to parse chat message:', error);
    }
  });
}

// ============================================================================
// STEP 4: Send Messages
// ============================================================================

async function sendChatMessage(meetingId, participantId, messageText, room, authToken) {
  // 4.1 Persist to database
  const response = await fetch(`/api/v1/meetings/${meetingId}/chat`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${authToken}`
    },
    body: JSON.stringify({
      participant_id: participantId,
      message: messageText
    })
  });

  const { data: savedMessage } = await response.json();

  // 4.2 Broadcast via LiveKit for real-time delivery
  const payload = JSON.stringify({
    type: 'chat_message',
    id: savedMessage.id,
    participant_id: savedMessage.participant_id,
    display_name: savedMessage.display_name,
    message: savedMessage.message,
    created_at: savedMessage.created_at
  });

  const encoder = new TextEncoder();
  await room.localParticipant.publishData(encoder.encode(payload), {
    reliable: true  // Ensures delivery (like TCP)
  });

  return savedMessage;
}

// ============================================================================
// COMPLETE EXAMPLE: Chat Component
// ============================================================================

class MeetingChat {
  constructor(meetingId, authToken) {
    this.meetingId = meetingId;
    this.authToken = authToken;
    this.messages = [];
    this.room = null;
    this.participantId = null;
  }

  async initialize(displayName) {
    // Join meeting and get LiveKit connection
    const { room, participant_id } = await joinMeeting(
      this.meetingId,
      displayName,
      this.authToken
    );

    this.room = room;
    this.participantId = participant_id;

    // Load existing messages (for late joiners)
    this.messages = await loadChatHistory(this.meetingId, this.authToken);
    this.renderMessages();

    // Listen for new messages
    setupChatListener(this.room, (message) => {
      // Avoid duplicates (sender already has their message)
      if (!this.messages.find(m => m.id === message.id)) {
        this.messages.push(message);
        this.renderMessage(message);
      }
    });
  }

  async send(messageText) {
    if (!messageText.trim()) return;

    const savedMessage = await sendChatMessage(
      this.meetingId,
      this.participantId,
      messageText,
      this.room,
      this.authToken
    );

    // Add to local messages and render
    this.messages.push(savedMessage);
    this.renderMessage(savedMessage);
  }

  renderMessages() {
    this.messages.forEach(msg => this.renderMessage(msg));
  }

  renderMessage(message) {
    // Your UI rendering logic here
    console.log(`[${message.display_name}]: ${message.message}`);
  }
}

// Usage
const chat = new MeetingChat('meeting-uuid', 'auth-token');
await chat.initialize('John Doe');
await chat.send('Hello everyone!');
```

### React Hook Example

```javascript
import { useState, useEffect, useCallback } from 'react';
import { Room, RoomEvent } from 'livekit-client';

function useMeetingChat(room, meetingId, participantId, authToken) {
  const [messages, setMessages] = useState([]);
  const [isLoading, setIsLoading] = useState(true);

  // Load chat history on mount
  useEffect(() => {
    if (!meetingId || !authToken) return;

    async function loadHistory() {
      setIsLoading(true);
      try {
        const response = await fetch(
          `/api/v1/meetings/${meetingId}/chat?limit=500`,
          { headers: { 'Authorization': `Bearer ${authToken}` } }
        );
        const { data } = await response.json();
        setMessages(data);
      } catch (error) {
        console.error('Failed to load chat history:', error);
      } finally {
        setIsLoading(false);
      }
    }

    loadHistory();
  }, [meetingId, authToken]);

  // Listen for real-time messages
  useEffect(() => {
    if (!room) return;

    const handleDataReceived = (payload) => {
      try {
        const data = JSON.parse(new TextDecoder().decode(payload));
        if (data.type === 'chat_message') {
          setMessages(prev => {
            // Avoid duplicates
            if (prev.find(m => m.id === data.id)) return prev;
            return [...prev, data];
          });
        }
      } catch (error) {
        console.error('Failed to parse message:', error);
      }
    };

    room.on(RoomEvent.DataReceived, handleDataReceived);
    return () => room.off(RoomEvent.DataReceived, handleDataReceived);
  }, [room]);

  // Send message function
  const sendMessage = useCallback(async (text) => {
    if (!text.trim() || !room || !participantId) return;

    try {
      // Persist to DB
      const response = await fetch(`/api/v1/meetings/${meetingId}/chat`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${authToken}`
        },
        body: JSON.stringify({
          participant_id: participantId,
          message: text
        })
      });

      const { data: savedMessage } = await response.json();

      // Broadcast via LiveKit
      const payload = JSON.stringify({ type: 'chat_message', ...savedMessage });
      await room.localParticipant.publishData(
        new TextEncoder().encode(payload),
        { reliable: true }
      );

      // Add to local state
      setMessages(prev => [...prev, savedMessage]);

      return savedMessage;
    } catch (error) {
      console.error('Failed to send message:', error);
      throw error;
    }
  }, [room, meetingId, participantId, authToken]);

  return { messages, sendMessage, isLoading };
}

// Usage in component
function ChatPanel({ room, meetingId, participantId, authToken }) {
  const { messages, sendMessage, isLoading } = useMeetingChat(
    room, meetingId, participantId, authToken
  );
  const [input, setInput] = useState('');

  const handleSend = async () => {
    if (input.trim()) {
      await sendMessage(input);
      setInput('');
    }
  };

  if (isLoading) return <div>Loading chat...</div>;

  return (
    <div className="chat-panel">
      <div className="messages">
        {messages.map(msg => (
          <div key={msg.id} className="message">
            <strong>{msg.display_name}</strong>: {msg.message}
          </div>
        ))}
      </div>
      <div className="input-area">
        <input
          value={input}
          onChange={e => setInput(e.target.value)}
          onKeyPress={e => e.key === 'Enter' && handleSend()}
          placeholder="Type a message..."
        />
        <button onClick={handleSend}>Send</button>
      </div>
    </div>
  );
}
```

---

## LiveKit Data Channels

### Why Hybrid Architecture?

| Aspect              | REST API Only        | LiveKit Only          | Hybrid (Our Approach)  |
| ------------------- | -------------------- | --------------------- | ---------------------- |
| Real-time delivery  | ❌ Polling required  | ✅ Instant            | ✅ Instant             |
| Late joiner history | ✅ Full history      | ❌ Missed messages    | ✅ Full history        |
| Persistence         | ✅ Database storage  | ❌ Memory only        | ✅ Database storage    |
| Scalability         | ✅ Stateless servers | ✅ SFU handles it     | ✅ Best of both        |
| Complexity          | ⭐ Simple            | ⭐ Simple             | ⭐⭐ Moderate          |

### Data Channel Message Format

```javascript
{
  "type": "chat_message",        // Message type identifier
  "id": "msg-uuid",              // Unique message ID from database
  "participant_id": "p-uuid",    // Sender's participant ID
  "display_name": "John Doe",    // Sender's display name
  "message": "Hello!",           // Message content
  "created_at": "2026-01-22..."  // ISO 8601 timestamp
}
```

### Publishing Data

```javascript
// Reliable delivery (guaranteed, ordered - like TCP)
await room.localParticipant.publishData(payload, { reliable: true });

// Lossy delivery (faster, no guarantee - like UDP) - NOT recommended for chat
await room.localParticipant.publishData(payload, { reliable: false });
```

### Receiving Data

```javascript
room.on(RoomEvent.DataReceived, (payload, participant, kind) => {
  // payload: Uint8Array of the message
  // participant: RemoteParticipant who sent it (undefined if from server)
  // kind: DataPacket_Kind.RELIABLE or DataPacket_Kind.LOSSY

  const message = JSON.parse(new TextDecoder().decode(payload));
  // Handle message...
});
```

---

## Database Schema

### Chat Messages Table

```sql
CREATE TABLE chat_messages (
    id UUID PRIMARY KEY,
    meeting_id UUID NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    participant_id UUID NOT NULL REFERENCES participants(id) ON DELETE CASCADE,
    message TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

-- Indexes for efficient queries
CREATE INDEX idx_chat_messages_meeting_id ON chat_messages(meeting_id);
CREATE INDEX idx_chat_messages_created_at ON chat_messages(created_at);
CREATE INDEX idx_chat_messages_meeting_created
    ON chat_messages(meeting_id, created_at);
```

### Cascade Delete Behavior

```
meetings (deleted) ───▶ chat_messages (auto-deleted via CASCADE)
                   └──▶ participants (auto-deleted via CASCADE)
                                └──▶ chat_messages (auto-deleted via CASCADE)
```

### Entity Definition (Sea-ORM)

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "chat_messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub participant_id: Uuid,
    pub message: String,
    pub created_at: DateTime,
}
```

---

## Security

### Authentication & Authorization

| Endpoint         | Auth Required | Who Can Access                    |
| ---------------- | ------------- | --------------------------------- |
| `POST /chat`     | ✅ Yes        | Joined participants only          |
| `GET /chat`      | ✅ Yes        | Meeting participants or host      |

### Validation Checks

```rust
// 1. Meeting must exist and be live
if meeting.status != MeetingStatus::Ongoing {
    return Err("Chat is only available during live meetings");
}

// 2. Participant must exist and be joined
if participant.status != ParticipantStatus::Joined {
    return Err("Only joined participants can send messages");
}

// 3. Authenticated user must own the participant record
if participant.user_id != Some(authenticated_user_id) {
    return Err("You can only send messages as yourself");
}

// 4. Message validation
if message.len() < 1 || message.len() > 2000 {
    return Err("Message must be between 1 and 2000 characters");
}
```

### Input Sanitization

- Messages are stored as-is in the database
- **Frontend is responsible for XSS prevention** when rendering
- Recommended: Use a sanitization library or escape HTML entities

```javascript
// Example: Escape HTML before rendering
function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}
```

---

## Usage Examples

### Example 1: Basic Chat Flow

```bash
# 1. Join the meeting first
curl -X POST "http://localhost:8080/api/v1/meetings/${MEETING_ID}/join" \
  -H "Content-Type: application/json" \
  -d '{
    "user_id": "user-uuid",
    "display_name": "John Doe"
  }'

# Response includes participant_id
# {"data": {"participant_id": "p-123", "join_token": "...", ...}}

# 2. Send a chat message
curl -X POST "http://localhost:8080/api/v1/meetings/${MEETING_ID}/chat" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "participant_id": "p-123",
    "message": "Hello everyone!"
  }'

# 3. Get chat history
curl -X GET "http://localhost:8080/api/v1/meetings/${MEETING_ID}/chat" \
  -H "Authorization: Bearer ${TOKEN}"
```

### Example 2: Late Joiner Loading History

```bash
# Join meeting (as late joiner)
curl -X POST "http://localhost:8080/api/v1/meetings/${MEETING_ID}/join" \
  -H "Content-Type: application/json" \
  -d '{"display_name": "Late Larry"}'

# Load all previous messages
curl -X GET "http://localhost:8080/api/v1/meetings/${MEETING_ID}/chat?limit=500" \
  -H "Authorization: Bearer ${TOKEN}"

# Response contains all messages from the meeting so far
```

### Example 3: Incremental Loading (Reconnection)

```bash
# After reconnecting, load only new messages since last received
LAST_TIMESTAMP="2026-01-22T10:30:00Z"

curl -X GET "http://localhost:8080/api/v1/meetings/${MEETING_ID}/chat?after=${LAST_TIMESTAMP}" \
  -H "Authorization: Bearer ${TOKEN}"

# Response contains only messages created after the timestamp
```

---

## Error Handling

### Common Errors

| Code | Error            | Cause                                  | Solution                           |
| ---- | ---------------- | -------------------------------------- | ---------------------------------- |
| 400  | Validation error | Message empty or too long              | Check message length (1-2000)      |
| 401  | Unauthorized     | Missing or invalid JWT                 | Include valid Bearer token         |
| 403  | Forbidden        | Not a joined participant               | Join meeting first                 |
| 403  | Forbidden        | Sending as different user              | Use your own participant_id        |
| 404  | Not Found        | Meeting doesn't exist                  | Check meeting ID                   |
| 404  | Not Found        | Participant doesn't exist              | Check participant_id               |
| 409  | Conflict         | Meeting not live                       | Meeting must be `ongoing`          |

### Error Response Format

```json
{
  "success": false,
  "error": {
    "code": "FORBIDDEN",
    "message": "Only joined participants can send messages"
  }
}
```

### Frontend Error Handling

```javascript
async function sendMessage(text) {
  try {
    const response = await fetch(`/api/v1/meetings/${meetingId}/chat`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${token}`
      },
      body: JSON.stringify({ participant_id: participantId, message: text })
    });

    if (!response.ok) {
      const error = await response.json();

      switch (response.status) {
        case 401:
          // Token expired - refresh or redirect to login
          await refreshToken();
          return sendMessage(text); // Retry
        case 403:
          // Not allowed - show error to user
          showError('You cannot send messages. Please rejoin the meeting.');
          break;
        case 409:
          // Meeting ended - redirect to post-meeting screen
          showError('The meeting has ended.');
          redirectToMeetingEnded();
          break;
        default:
          showError(error.error?.message || 'Failed to send message');
      }
      return null;
    }

    return await response.json();
  } catch (networkError) {
    showError('Network error. Please check your connection.');
    return null;
  }
}
```

---

## Best Practices

### 1. Always Persist Before Broadcasting

```javascript
// ✅ DO: Persist first, then broadcast
const saved = await persistToDatabase(message);
await broadcastViaLiveKit(saved);  // Use saved message with ID

// ❌ DON'T: Broadcast without persisting
await broadcastViaLiveKit(message);  // No ID, no persistence
```

### 2. Include Message ID in Broadcasts

```javascript
// ✅ DO: Include the database ID for deduplication
const payload = {
  type: 'chat_message',
  id: savedMessage.id,  // Critical for deduplication
  ...savedMessage
};

// ❌ DON'T: Send without ID
const payload = {
  type: 'chat_message',
  message: text  // No way to deduplicate
};
```

### 3. Deduplicate Messages on Receive

```javascript
// ✅ DO: Check for duplicates before adding
room.on(RoomEvent.DataReceived, (payload) => {
  const msg = JSON.parse(decode(payload));
  if (!messages.find(m => m.id === msg.id)) {
    messages.push(msg);
    renderMessage(msg);
  }
});

// ❌ DON'T: Add blindly (causes duplicates for sender)
room.on(RoomEvent.DataReceived, (payload) => {
  messages.push(JSON.parse(decode(payload)));  // Sender sees duplicate
});
```

### 4. Use Reliable Delivery for Chat

```javascript
// ✅ DO: Use reliable delivery
await room.localParticipant.publishData(payload, { reliable: true });

// ❌ DON'T: Use lossy for important messages
await room.localParticipant.publishData(payload, { reliable: false });
```

### 5. Handle Network Reconnection

```javascript
// ✅ DO: Reload messages after reconnection
room.on(RoomEvent.Reconnected, async () => {
  const lastTimestamp = messages[messages.length - 1]?.created_at;
  const newMessages = await fetchMessages({ after: lastTimestamp });
  newMessages.forEach(msg => {
    if (!messages.find(m => m.id === msg.id)) {
      messages.push(msg);
    }
  });
});
```

### 6. Sanitize User Input on Display

```javascript
// ✅ DO: Escape HTML when rendering
function renderMessage(msg) {
  element.textContent = msg.message;  // Safe: textContent escapes HTML
}

// ❌ DON'T: Use innerHTML with user content
function renderMessage(msg) {
  element.innerHTML = msg.message;  // XSS vulnerability!
}
```

---

## Troubleshooting

### Messages Not Appearing in Real-Time

**Symptom**: Messages only appear after refresh

**Possible Causes:**
1. LiveKit data channel not set up
2. Event listener not attached
3. Message type mismatch

**Solution:**
```javascript
// Verify listener is set up correctly
room.on(RoomEvent.DataReceived, (payload) => {
  console.log('Data received:', payload);  // Debug log
  const data = JSON.parse(new TextDecoder().decode(payload));
  console.log('Parsed:', data);  // Verify structure

  if (data.type === 'chat_message') {
    // Handle message
  }
});

// Verify you're connected to LiveKit
console.log('Room state:', room.state);  // Should be 'connected'
```

### Duplicate Messages Appearing

**Symptom**: Same message appears twice

**Cause**: Sender sees message from both:
1. Their own `sendMessage` call
2. LiveKit `DataReceived` event

**Solution:**
```javascript
// Deduplicate by message ID
setMessages(prev => {
  if (prev.find(m => m.id === newMessage.id)) {
    return prev;  // Already have this message
  }
  return [...prev, newMessage];
});
```

### Late Joiner Missing Messages

**Symptom**: New participant doesn't see previous messages

**Cause**: Forgot to load chat history

**Solution:**
```javascript
// Load history immediately after joining
async function onJoinMeeting() {
  const { participant_id } = await joinMeeting();
  const history = await loadChatHistory();  // Don't forget this!
  setMessages(history);
  setupRealtimeListener();
}
```

### "Meeting is not live" Error (409)

**Symptom**: Cannot send messages, get 409 error

**Cause**: Meeting status is not `ongoing`

**Solution:**
- Verify meeting has been started (first participant joined)
- Check if meeting has ended
- Query meeting status:
```sql
SELECT status FROM meetings WHERE id = 'meeting-uuid';
```

### Messages Not Persisted

**Symptom**: Messages disappear on page refresh

**Cause**: Only broadcasting via LiveKit, not saving to DB

**Solution:**
```javascript
// Always save BEFORE broadcasting
const savedMessage = await fetch('/api/v1/meetings/.../chat', {
  method: 'POST',
  body: JSON.stringify({ participant_id, message })
}).then(r => r.json());

// Then broadcast
await room.localParticipant.publishData(
  encode(JSON.stringify(savedMessage)),
  { reliable: true }
);
```

---

## Quick Reference

### Message Constraints

| Constraint        | Value         | Notes                        |
| ----------------- | ------------- | ---------------------------- |
| Min length        | 1 character   | Cannot be empty              |
| Max length        | 2000 chars    | Enforced by validation       |
| History limit     | 500 messages  | Max per GET request          |
| Default limit     | 100 messages  | If limit not specified       |

### Endpoints Summary

```bash
# Send message (requires auth)
POST /api/v1/meetings/{id}/chat
Body: { "participant_id": "uuid", "message": "text" }

# Get messages (requires auth)
GET /api/v1/meetings/{id}/chat
GET /api/v1/meetings/{id}/chat?limit=500
GET /api/v1/meetings/{id}/chat?after=2026-01-22T10:00:00Z
GET /api/v1/meetings/{id}/chat?after=2026-01-22T10:00:00Z&limit=100
```

### LiveKit Data Payload

```javascript
{
  type: "chat_message",
  id: "uuid",                    // From database
  participant_id: "uuid",        // Sender
  display_name: "John Doe",      // Sender name
  message: "Hello!",             // Content
  created_at: "ISO-8601"         // Timestamp
}
```

### Implementation Checklist

- [ ] Set up LiveKit room connection
- [ ] Implement `RoomEvent.DataReceived` listener
- [ ] Load chat history on join (`GET /chat`)
- [ ] Persist messages before broadcast (`POST /chat`)
- [ ] Broadcast via LiveKit (`publishData`)
- [ ] Deduplicate messages by ID
- [ ] Handle reconnection (reload missed messages)
- [ ] Handle errors gracefully
- [ ] Sanitize messages before display

---

**Last Updated**: 2026-01-22
**Version**: 1.0
**Status**: Production Ready

For meeting management, see [MEETINGS.md](./MEETINGS.md)
For participant management, see [PARTICIPANTS.md](./PARTICIPANTS.md)
For LiveKit integration, see [LIVEKIT.md](./LIVEKIT.md)
