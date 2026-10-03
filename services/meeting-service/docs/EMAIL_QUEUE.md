# Email Queue System

Async email delivery using Redis with automatic retry.

## Architecture

```
┌─────────────────┐     ┌──────────────┐     ┌─────────────┐     ┌────────┐
│  API Handler    │────>│ EmailService │────>│ Redis Queue │<────│ Worker │───> SMTP
│ (register, etc) │     │  (enqueue)   │     │ (LPUSH)     │     │ (BRPOP)│
└─────────────────┘     └──────────────┘     └─────────────┘     └────────┘
```

## Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `EmailService` | `services/email.rs` | High-level API for sending emails |
| `RedisEmailQueue` | `queues/redis_email.rs` | Enqueues jobs to Redis |
| `EmailWorker` | `workers/email.rs` | Background processor |
| Templates | `templates/email/` | HTML/text email templates |

## Folder Structure

```
services/core-api/src/
├── queues/
│   ├── mod.rs
│   └── redis_email.rs    # Redis queue (LPUSH/BRPOP)
├── workers/
│   ├── mod.rs
│   └── email.rs          # Email worker
├── services/
│   └── email.rs          # EmailService (uses queue)
└── templates/
    └── email/
        ├── mod.rs
        ├── base.rs           # Base template wrapper
        ├── verification.rs   # Email verification
        ├── password_reset.rs # Password reset
        ├── welcome.rs        # Welcome email
        └── notification.rs   # Generic notifications
```

## Usage

### Sending an Email

Emails are queued to Redis, not sent immediately:

```rust
let job_id = state.email_service
    .send_verification_email(&email, &username, &code)
    .await?;
```

### Available Methods

```rust
// Verification email (with 6-digit code)
send_verification_email(to_email, username, verification_code) -> Uuid

// Password reset (with 6-digit code)
send_password_reset_email(to_email, username, reset_code) -> Uuid

// Welcome email (after verification)
send_welcome_email(to_email, username) -> Uuid

// Generic notification
send_notification_email(to_email, username, subject, message) -> Uuid
```

## Adding a New Email Type

### 1. Create Template

Create `services/core-api/src/templates/email/my_email.rs`:

```rust
use super::base::{wrap_html, wrap_text};
use super::EmailTemplate;

pub fn my_email(username: &str, data: &str) -> EmailTemplate {
    let html_content = format!(
        r#"<h2>Title</h2>
<p>Hi <strong>{}</strong>,</p>
<p>{}</p>"#,
        username, data
    );

    let text_content = format!("Hi {}, {}", username, data);

    EmailTemplate {
        html: wrap_html("Email Title", &html_content),
        text: wrap_text(&text_content),
    }
}
```

### 2. Export Template

In `templates/email/mod.rs`:

```rust
mod my_email;
pub use my_email::my_email;
```

### 3. Add Service Method

In `services/email.rs`:

```rust
pub async fn send_my_email(
    &self,
    to_email: &str,
    username: &str,
    data: &str,
) -> ApiResult<Uuid> {
    let template = my_email(username, data);

    self.queue
        .enqueue(
            to_email,
            Some(username),
            "Subject Line",
            &template.html,
            &template.text,
        )
        .await
}
```

## Job Lifecycle

```
┌─────────┐          ┌──────┐
│ QUEUED  │─────────>│ SENT │
└─────────┘          └──────┘
     │
     │ (on failure)
     ▼
┌─────────┐
│ RETRY   │──────┐
└─────────┘      │ (re-queue)
     ▲           │
     └───────────┘
     │
     ▼ (max retries = 3)
┌─────────┐
│ DROPPED │
└─────────┘
```

## Retry Mechanism

- **Max retries**: 3
- Failed jobs are immediately re-queued with incremented retry count
- After 3 failures, job is dropped and logged

## Redis Queue Details

| Key | Type | Purpose |
|-----|------|---------|
| `email:queue` | List | Job queue (LPUSH to add, BRPOP to pop) |

### Job Structure

```json
{
  "id": "uuid",
  "to_email": "user@example.com",
  "to_name": "Username",
  "subject": "Email Subject",
  "html_body": "<html>...</html>",
  "text_body": "Plain text...",
  "retry_count": 0
}
```

## Configuration

Required environment variables:

```env
# SMTP Settings
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=noreply@cosmicforge.com
SMTP_FROM_NAME=CosmicForge HealthNet

# Redis
REDIS_URL=redis://127.0.0.1:6379

# App URL (for email links)
APP_URL=https://yourapp.com
```

**Note**: If SMTP is not configured, emails will still be queued but the worker won't start.

## Worker Behavior

- Uses blocking pop (`BRPOP`) with 5-second timeout
- Processes one job at a time
- Supports graceful shutdown via watch channel
- Logs all send successes and failures

## Monitoring

### Check queue length

```bash
redis-cli LLEN email:queue
```

### View queued jobs

```bash
redis-cli LRANGE email:queue 0 -1
```

### Clear the queue

```bash
redis-cli DEL email:queue
```

## Testing

Without SMTP configured, emails queue but don't send. Check Redis:

```bash
# Connect to Redis
docker-compose exec redis redis-cli

# Check queue length
LLEN email:queue

# View jobs
LRANGE email:queue 0 -1
```

## SMTP Port Configuration

| Port | Protocol | Usage |
|------|----------|-------|
| 587 | STARTTLS | Default, most providers |
| 465 | SSL/TLS | Direct SSL connection |

The worker automatically selects the correct protocol based on the port.
