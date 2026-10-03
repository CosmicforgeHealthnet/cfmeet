# CosmicForge RTC - Third-Party API Integration Guide

This guide explains how third-party developers can integrate with the CosmicForge RTC API to create and manage video meetings programmatically.

---

## Table of Contents

1. [Introduction](#introduction)
2. [Getting Started](#getting-started)
3. [Authentication](#authentication)
4. [API Endpoints](#api-endpoints)
5. [Integration Flow](#integration-flow)
6. [API Key Management](#api-key-management)
7. [Error Handling](#error-handling)
8. [Security Best Practices](#security-best-practices)
9. [Code Examples](#code-examples)

---

## Introduction

### What is CosmicForge API?

CosmicForge RTC provides a REST API that allows third-party applications to:

- Create video meetings programmatically
- Generate join links for users
- Manage meeting lifecycle

### Use Cases

- **SaaS Platforms**: Embed video meetings in your application
- **Scheduling Tools**: Auto-create meetings from calendar events
- **CRM Systems**: Add video call capabilities to customer interactions
- **Internal Tools**: Automate meeting creation for teams

### Authentication Methods

| Method        | Use Case                          | Header                          |
| ------------- | --------------------------------- | ------------------------------- |
| **JWT Token** | Logged-in users in CosmicForge UI | `Authorization: Bearer {token}` |
| **API Key**   | Third-party server integrations   | `Api-Key: {key}`                |

**This guide focuses on API Key authentication for third-party integrations.**

---

## Getting Started

### Prerequisites

1. A CosmicForge account
2. A backend server (Node.js, Python, Go, etc.)
3. HTTPS support for production

### Step 1: Create an API Key

First, authenticate with your CosmicForge account and create an API key:

```bash
# Login to get JWT token
curl -X POST https://api.cosmicforge.com/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "your@email.com",
    "password": "yourpassword"
  }'

# Response: { "access_token": "eyJhbGciOiJIUzI1NiIs..." }
```

```bash
# Create API key using the JWT token
curl -X POST https://api.cosmicforge.com/api/v1/api-keys \
  -H "Authorization: Bearer eyJhbGciOiJIUzI1NiIs..." \
  -H "Content-Type: application/json" \
  -d '{
    "usage_limit": 10000,
    "expires_at": "2027-01-01T00:00:00Z"
  }'
```

**Response:**

```json
{
  "success": true,
  "data": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "api_key": "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789AB",
    "usage_limit": 10000,
    "used_count": 0,
    "expires_at": "2027-01-01T00:00:00Z",
    "status": "active",
    "created_at": "2026-01-24T10:00:00Z"
  }
}
```

> **Important**: Save the `api_key` value immediately. It is only shown once and cannot be retrieved later.

### Step 2: Store the API Key Securely

Store the API key in your server's environment variables:

```bash
# .env file (NEVER commit this to git)
COSMICFORGE_API_KEY=ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789AB
COSMICFORGE_API_URL=https://api.cosmicforge.com
```

---

## Authentication

### Using the Api-Key Header

All API requests must include the `Api-Key` header:

```
Api-Key: your-64-character-api-key
```

### Example Request

```bash
curl -X POST https://api.cosmicforge.com/api/v1/api/meetings \
  -H "Api-Key: ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789AB" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Team Standup",
    "start_time": "2026-01-25T10:00:00Z"
  }'
```

### Authentication Errors

| Status | Error                          | Description                |
| ------ | ------------------------------ | -------------------------- |
| 401    | `Missing Api-Key header`       | No Api-Key header provided |
| 401    | `Invalid API key`              | Key not found in database  |
| 401    | `API key has been revoked`     | Key was revoked by owner   |
| 401    | `API key has expired`          | Key passed expiration date |
| 403    | `API key usage limit exceeded` | Usage count reached limit  |

---

## API Endpoints

### Create Meeting

Creates a new meeting and returns a join URL for the hosted UI.

```
POST /api/v1/api/meetings
```

**Headers:**

```
Api-Key: {your-api-key}
Content-Type: application/json
```

**Request Body:**

```json
{
  "title": "Team Standup",
  "start_time": "2026-01-25T10:00:00Z",
  "end_time": "2026-01-25T10:30:00Z",
  "is_private": false,
  "metadata": {
    "department": "engineering",
    "project": "alpha"
  }
}
```

| Field        | Type     | Required | Description                                 |
| ------------ | -------- | -------- | ------------------------------------------- |
| `title`      | string   | Yes      | Meeting title (1-200 chars)                 |
| `start_time` | ISO 8601 | Yes      | Must be at least 1 minute in future         |
| `end_time`   | ISO 8601 | No       | Must be after start_time, max 24h duration  |
| `is_private` | boolean  | No       | Default: false. If true, guests cannot join |
| `metadata`   | object   | No       | Custom key-value data                       |

**Response (201 Created):**

```json
{
  "success": true,
  "data": {
    "id": "660e8400-e29b-41d4-a716-446655440000",
    "meeting_identifier": "ABCD1234",
    "host_id": "550e8400-e29b-41d4-a716-446655440000",
    "title": "Team Standup",
    "metadata": {
      "department": "engineering",
      "project": "alpha"
    },
    "is_private": false,
    "start_time": "2026-01-25T10:00:00Z",
    "end_time": "2026-01-25T10:30:00Z",
    "status": "scheduled",
    "join_url": "https://meet.cosmicforge.com/join/ABCD1234",
    "created_at": "2026-01-24T10:00:00Z",
    "updated_at": "2026-01-24T10:00:00Z"
  }
}
```

**Key Fields:**

- `join_url` - Redirect users here to join the meeting
- `meeting_identifier` - 8-character code for sharing
- `id` - UUID for API operations

---

### Join Meeting

Joins a meeting and returns a LiveKit token for RTC connection.

```
POST /api/v1/api/meetings/{id}/join
```

**Headers:**

```
Api-Key: {your-api-key}
Content-Type: application/json
```

**URL Parameters:**

- `{id}` - Meeting UUID

**Request Body:**

```json
{
  "display_name": "John Doe"
}
```

| Field          | Type   | Required | Description                                    |
| -------------- | ------ | -------- | ---------------------------------------------- |
| `display_name` | string | Yes      | Name shown to other participants (1-100 chars) |

**Response (200 OK):**

```json
{
  "success": true,
  "data": {
    "participant_id": "770e8400-e29b-41d4-a716-446655440000",
    "role": "host",
    "join_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
    "livekit_url": "wss://livekit.cosmicforge.com",
    "room_name": "ABCD1234",
    "access_token": "eyJhbG.. (for guests)",
    "refresh_token": "eyJhbG.. (for guests)"
  }
}
```

**Key Fields:**

- `join_token` - Short-lived token for LiveKit WebSocket connection
- `livekit_url` - WebSocket URL for LiveKit server
- `role` - Either "host" or "participant"

**Validation Rules:**

- Users cannot join scheduled meetings **before** the start time
- Users cannot join **after** the scheduled end time

> **Note**: For most integrations, redirect users to `join_url` instead of using this endpoint directly. This endpoint is for advanced integrations building custom UIs.

---

### Get Public Meeting Info

Retrieves public meeting information without authentication. Used by the hosted UI before joining.

```
GET /api/v1/meetings/public/{meeting_identifier}
```

**No authentication required.**

**URL Parameters:**

- `{meeting_identifier}` - 8-character meeting code (e.g., ABCD1234)

**Response (200 OK):**

```json
{
  "success": true,
  "data": {
    "meeting_identifier": "ABCD1234",
    "title": "Team Standup",
    "is_private": false,
    "start_time": "2026-01-25T10:00:00Z",
    "end_time": "2026-01-25T10:30:00Z",
    "status": "scheduled",
    "join_url": "https://meet.cosmicforge.com/join/ABCD1234"
  }
}
```

---

## Integration Flow

### Visual Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                        YOUR APPLICATION                              │
│                                                                      │
│  ┌──────────────────┐              ┌──────────────────────────────┐ │
│  │   Your Frontend  │              │      Your Backend            │ │
│  │   (Browser)      │              │      (Server)                │ │
│  │                  │              │                              │ │
│  │  1. User clicks  │              │  .env:                       │ │
│  │     "New Meeting"│──────────────│  COSMICFORGE_API_KEY=xxx     │ │
│  │                  │   Request    │                              │ │
│  │                  │              │  2. Call CosmicForge API     │ │
│  │                  │              │     with Api-Key header      │ │
│  └──────────────────┘              └──────────────┬───────────────┘ │
│                                                   │                  │
└───────────────────────────────────────────────────┼──────────────────┘
                                                    │
                                                    ▼
                                    ┌───────────────────────────────┐
                                    │     CosmicForge API           │
                                    │                               │
                                    │  POST /api/v1/api/meetings    │
                                    │  Api-Key: xxx                 │
                                    │                               │
                                    │  Returns: { join_url: "..." } │
                                    └───────────────┬───────────────┘
                                                    │
                                                    ▼
┌───────────────────────────────────────────────────────────────────────┐
│                                                                       │
│  3. Your backend returns join_url to your frontend                    │
│                                                                       │
│  4. Your frontend redirects user to join_url                          │
│     window.location.href = "https://meet.cosmicforge.com/join/ABCD1234"│
│                                                                       │
└───────────────────────────────────────────────────────────────────────┘
                                                    │
                                                    ▼
                                    ┌───────────────────────────────┐
                                    │   CosmicForge Hosted UI       │
                                    │                               │
                                    │  - Prompts for display name   │
                                    │  - Handles video/audio        │
                                    │  - Manages participants       │
                                    │  - Chat, screen share, etc.   │
                                    └───────────────────────────────┘
```

### Step-by-Step Walkthrough

1. **User Action**: User clicks "Create Meeting" in your app
2. **Your Frontend**: Sends request to your backend
3. **Your Backend**: Calls CosmicForge API with `Api-Key` header
4. **CosmicForge API**: Returns meeting data including `join_url`
5. **Your Backend**: Returns `join_url` to your frontend
6. **Your Frontend**: Redirects user to `join_url`
7. **Hosted UI**: CosmicForge handles the entire meeting experience

---

## API Key Management

### List Your API Keys

```bash
curl -X GET https://api.cosmicforge.com/api/v1/api-keys \
  -H "Authorization: Bearer {jwt-token}"
```

**Response:**

```json
{
  "success": true,
  "data": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "api_key_masked": "********...AB",
      "usage_limit": 10000,
      "used_count": 42,
      "expires_at": "2027-01-01T00:00:00Z",
      "status": "active",
      "created_at": "2026-01-24T10:00:00Z",
      "updated_at": "2026-01-24T12:00:00Z"
    }
  ]
}
```

### Revoke an API Key

```bash
curl -X DELETE https://api.cosmicforge.com/api/v1/api-keys/{id} \
  -H "Authorization: Bearer {jwt-token}"
```

**Response:**

```json
{
  "success": true,
  "message": "API key revoked successfully"
}
```

---

## Error Handling

### Common Error Responses

```json
{
  "success": false,
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Invalid API key"
  }
}
```

### Error Codes

| HTTP Status | Code               | Description                             |
| ----------- | ------------------ | --------------------------------------- |
| 400         | `VALIDATION_ERROR` | Invalid request body                    |
| 401         | `UNAUTHORIZED`     | Missing or invalid authentication       |
| 403         | `FORBIDDEN`        | API key limit exceeded or access denied |
| 404         | `NOT_FOUND`        | Meeting not found                       |
| 409         | `CONFLICT`         | Meeting already ended or invalid state  |
| 500         | `INTERNAL_ERROR`   | Server error                            |

### Handling Errors in Code

```javascript
async function createMeeting(title, startTime) {
  const response = await fetch(`${API_URL}/api/v1/api/meetings`, {
    method: "POST",
    headers: {
      "Api-Key": process.env.COSMICFORGE_API_KEY,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ title, start_time: startTime }),
  });

  const data = await response.json();

  if (!response.ok) {
    switch (response.status) {
      case 401:
        throw new Error("API key is invalid or expired");
      case 403:
        throw new Error("API key usage limit exceeded");
      case 400:
        throw new Error(`Validation error: ${data.error.message}`);
      default:
        throw new Error(`API error: ${data.error.message}`);
    }
  }

  return data.data;
}
```

---

## Security Best Practices

### 1. Never Expose API Keys in Frontend

```javascript
// ❌ WRONG - Never do this
const API_KEY = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef...";
fetch("/api/meetings", { headers: { "Api-Key": API_KEY } });

// ✅ CORRECT - Call your own backend
fetch("/your-backend/create-meeting", { method: "POST" });
```

### 2. Use Environment Variables

```bash
# .env (never commit to git)
COSMICFORGE_API_KEY=your-key-here

# .gitignore
.env
```

### 3. Rotate Keys Periodically

- Create a new API key before the old one expires
- Update your server configuration
- Revoke the old key

### 4. Monitor Usage

Regularly check your API key usage:

- Watch for unexpected spikes
- Set appropriate usage limits
- Use different keys for different environments (dev/staging/prod)

### 5. Use HTTPS

Always use HTTPS in production to encrypt API keys in transit.

---

## Code Examples

### Node.js / Express

```javascript
// server.js
const express = require("express");
const app = express();

const COSMICFORGE_API_KEY = process.env.COSMICFORGE_API_KEY;
const COSMICFORGE_API_URL =
  process.env.COSMICFORGE_API_URL || "https://api.cosmicforge.com";

app.use(express.json());

// Create meeting endpoint for your frontend
app.post("/api/meetings", async (req, res) => {
  try {
    const response = await fetch(`${COSMICFORGE_API_URL}/api/v1/api/meetings`, {
      method: "POST",
      headers: {
        "Api-Key": COSMICFORGE_API_KEY,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        title: req.body.title,
        start_time:
          req.body.start_time || new Date(Date.now() + 5 * 60000).toISOString(),
        end_time: req.body.end_time,
        is_private: req.body.is_private || false,
      }),
    });

    const data = await response.json();

    if (!response.ok) {
      return res.status(response.status).json(data);
    }

    // Return only what the frontend needs
    res.json({
      meeting_id: data.data.id,
      meeting_code: data.data.meeting_identifier,
      join_url: data.data.join_url,
      title: data.data.title,
      start_time: data.data.start_time,
    });
  } catch (error) {
    console.error("CosmicForge API error:", error);
    res.status(500).json({ error: "Failed to create meeting" });
  }
});

app.listen(3001, () => {
  console.log("Server running on port 3001");
});
```

### Python / Flask

```python
# app.py
import os
import requests
from flask import Flask, request, jsonify

app = Flask(__name__)

COSMICFORGE_API_KEY = os.environ.get('COSMICFORGE_API_KEY')
COSMICFORGE_API_URL = os.environ.get('COSMICFORGE_API_URL', 'https://api.cosmicforge.com')

@app.route('/api/meetings', methods=['POST'])
def create_meeting():
    try:
        data = request.get_json()

        response = requests.post(
            f'{COSMICFORGE_API_URL}/api/v1/api/meetings',
            headers={
                'Api-Key': COSMICFORGE_API_KEY,
                'Content-Type': 'application/json'
            },
            json={
                'title': data.get('title'),
                'start_time': data.get('start_time'),
                'end_time': data.get('end_time'),
                'is_private': data.get('is_private', False)
            }
        )

        result = response.json()

        if not response.ok:
            return jsonify(result), response.status_code

        return jsonify({
            'meeting_id': result['data']['id'],
            'meeting_code': result['data']['meeting_identifier'],
            'join_url': result['data']['join_url'],
            'title': result['data']['title'],
            'start_time': result['data']['start_time']
        })

    except Exception as e:
        return jsonify({'error': str(e)}), 500

if __name__ == '__main__':
    app.run(port=3001)
```

### cURL Examples

**Create a meeting:**

```bash
curl -X POST https://api.cosmicforge.com/api/v1/api/meetings \
  -H "Api-Key: YOUR_API_KEY_HERE" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Quick Sync",
    "start_time": "2026-01-25T15:00:00Z"
  }'
```

**Get public meeting info:**

```bash
curl -X GET https://api.cosmicforge.com/api/v1/meetings/public/ABCD1234
```

---

## Support

- **API Documentation**: `/swagger-ui` on your CosmicForge server
- **OpenAPI Spec**: `/api-docs/openapi.json`
- **Issues**: https://github.com/cosmicforge/rtc/issues

---

## Changelog

| Version | Date       | Changes             |
| ------- | ---------- | ------------------- |
| 1.0.0   | 2026-01-24 | Initial API release |
