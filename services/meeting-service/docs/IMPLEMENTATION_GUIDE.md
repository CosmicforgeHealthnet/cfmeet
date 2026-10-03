# CosmicForge RTC – How to Build the Project (Implementation Guide)

This document explains how to go about building CosmicForge RTC step by step, with special focus on the meeting lifecycle, which is the core of the platform. It is written as a practical execution guide, not an API spec.

## 1. How to Think About the System

CosmicForge RTC is an API-first, real-time system. The correct mindset is:

- **HTTP APIs** = control plane (create meetings, join, mute, kick, etc.)
- **WebSocket / RTC layer** = data plane (audio, video, screen, real-time events)
- **Database** = source of truth (users, meetings, participants, logs)

**You never mix these responsibilities.**

## 2. Recommended Build Order (Very Important)

Build the system in this order to avoid rework:

1.  **Database schema** (UUID-based)
2.  **Authentication & user identity**
3.  **Meeting lifecycle** (create → join → live → end)
4.  **Participant state management**
5.  **WebSocket event layer**
6.  **Media control APIs** (mute, video, screen share)
7.  **Chat**
8.  **API keys & integrations**
9.  **Webhooks**
10. **Admin APIs**

**Do not start with WebRTC media first.** Meetings and participants must exist and be stable before media comes in.

## 3. Core Domain Models (Mental Model)

### User

- Owns meetings
- Owns API keys
- Owns webhooks

### Meeting

- Created by exactly one user (host)
- Has a lifecycle state
- Exists even when no one is connected

### Participant

- Represents a connection to a meeting
- Can be a registered user or a guest
- Always short-lived

## 4. Meeting Lifecycle (MOST IMPORTANT PART)

This is the heart of CosmicForge RTC.

### 4.1 Meeting States

A meeting should behave like a state machine:

- `scheduled`
- `live`
- `ended`

**Rules:**

- Meetings start as `scheduled`.
- First successful join moves it to `live`.
- Host ending the meeting moves it to `ended`.
- No joins allowed after `ended`.
- **Never delete meetings immediately.** Ended meetings are valuable for logs, audits, and analytics.

### 4.2 Creating a Meeting

What happens internally when `POST /meetings` is called:

1.  **Authenticate** the user.
2.  **Validate** meeting payload (time range, title, privacy).
3.  **Generate**:
    - `meeting_id` (UUID)
    - `meeting_identifier` (short human-friendly code)
4.  **Store** meeting with status = `scheduled`.
5.  **Return** meeting metadata.

**Key rules:**

- The authenticated user becomes `host_id`.
- No participants exist yet.
- No RTC resources are allocated yet.
- This step is cheap and fast.

### 4.3 Joining a Meeting

Joining is where control-plane meets real-time.

What happens when `POST /meetings/:id/join` is called:

1.  **Validate** meeting exists and is not ended.
2.  **Determine participant type**:
    - Registered user → `user_id` present
    - Guest → `user_id` = null
3.  **Create a Participant record**.
4.  **Assign role** (host or participant).
5.  **Generate a short-lived join token**.
6.  **If meeting is scheduled**, transition it to `live`.
7.  **Return join token** to client.

**Important:**

- Join token is the **ONLY** thing allowed to open WebSocket/RTC.
- JWTs are never used directly for media connections.

### 4.4 Waiting Room Logic

Waiting room is not a separate system. It is a participant state.

For private meetings:

- New participants start as `waiting`.
- Host must explicitly admit them.

**Flow:**

1.  Participant joins → status = `waiting`
2.  Host admits → status = `active`
3.  Host denies → participant removed

Waiting room checks must be enforced **before** media negotiation.

### 4.5 Live Meeting Behavior

Once a meeting is live:

1.  Participants connect via WebSocket using **join token**.
2.  Server broadcasts events:
    - `participant_joined`
    - `participant_left`
    - `mute`/`unmute`
    - `role change`
    - `screen share start`/`stop`

The database tracks authoritative state.
WebSocket broadcasts mirror that state.
**Never trust client-side state alone.**

### 4.6 Ending a Meeting

Only the host can end a meeting.

What happens when `POST /meetings/:id/end` is called:

1.  **Validate** requester is host.
2.  **Update** meeting status to `ended`.
3.  **Disconnect** all active participants.
4.  **Persist** session logs.
5.  **Trigger** webhook events.

**After this:**

- No new joins allowed.
- Existing join tokens become invalid.

## 5. Participant Management Philosophy

Participants are **session-bound**, not user-bound.

**Rules:**

- A user can join the same meeting multiple times (new `participant_id` each time).
- Guest participants always have `user_id = null`.
- Participant actions are scoped to the meeting.

**Mute, kick, role changes are:**

1.  Server-validated
2.  Host-controlled
3.  Broadcast via WebSocket

## 6. Media Control Strategy

Media APIs do **NOT** move media.
They only control permissions and state.

**Example:**

1.  Muting someone updates DB state.
2.  WebSocket notifies clients.
3.  Client enforces mute locally.

This keeps the server scalable and simple.

## 7. Chat Design

Chat is:

- Meeting-scoped
- Session-based
- Optional persistence

**Recommended:**

- Store messages in memory or short-lived storage.
- Persist only if compliance requires it.

## 8. API Keys & External Usage

API keys:

- Belong to users.
- Cannot access media directly.
- Can only perform control-plane actions.

This keeps your RTC infrastructure protected.

## 9. Webhooks Philosophy

Webhooks are:

- User-owned
- Event-driven
- Asynchronous

They are not part of the critical path.
Failures should never break meetings.

## 10. Security Rules You Must Enforce

1.  Short-lived JWTs
2.  Join tokens for RTC
3.  Role checks everywhere
4.  Rate limiting on joins
5.  Status checks (deactivated users blocked)

## 11. User Profile APIs

These APIs allow users to manage their own identity and account status.

**Checklist:**

- [ ] Implement `GET /users/me`
- [ ] Implement `PATCH /users/me`
- [ ] Implement `POST /users/me/deactivate`
- [ ] Enforce auth-only access
- [ ] Block access for deactivated users

## 12. Final Mental Model (Summary)

- **Users** own meetings
- **Meetings** own participants
- **Participants** represent connections
- **HTTP** controls state
- **WebSocket** syncs state
- **Media** stays client-side

If you build CosmicForge RTC with this structure, it will:

- Scale cleanly
- Be easy to reason about
- Support SDKs and integrations naturally
- Avoid tight coupling between API and media

**This document should guide every technical decision you make while building CosmicForge RTC.**
