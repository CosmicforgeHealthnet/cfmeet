# Email Queue System

Async email delivery with retry, dead-letter queue, and idempotency.

## Architecture

```
┌─────────────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────┐
│  API Handler    │────▶│ EmailService │────▶│  email_jobs  │◀────│Worker│────▶ SMTP
│ (register, etc) │     │  (enqueue)   │     │   (table)    │     │      │
└─────────────────┘     └──────────────┘     └──────────────┘     └──────┘
```

## Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `EmailService` | `services/email.rs` | High-level API for sending emails |
| `EmailQueue` | `queues/email.rs` | Enqueues jobs to database |
| `EmailWorker` | `workers/email.rs` | Background processor |
| `email_jobs` | `models/email_jobs.rs` | Database model |
| Templates | `templates/email/` | HTML/text email templates |

## Folder Structure

```
backend/src/
├── queues/           # Job queues
│   ├── mod.rs
│   └── email.rs      # Email queue
├── workers/          # Background workers
│   ├── mod.rs
│   └── email.rs      # Email worker
├── services/
│   └── email.rs      # EmailService (uses queue)
├── models/
│   └── email_jobs.rs # Database entity
└── templates/
    └── email/        # Email templates
```

## Usage

### Sending an Email (from handler)

```rust
// Emails are queued, not sent immediately
let job_id = state.email_service
    .send_verification_email(&email, &username, &token)
    .await?;
```

### Available Methods

```rust
// Verification email
send_verification_email(to_email, username, token) -> Uuid

// Password reset
send_password_reset_email(to_email, username, token) -> Uuid

// Welcome email (after verification)
send_welcome_email(to_email, username) -> Uuid

// Generic notification
send_notification_email(to_email, username, subject, message, notification_id) -> Uuid
```

## Adding a New Email Type

### 1. Create Template

Create `backend/src/templates/email/my_email.rs`:

```rust
use super::base::{button, wrap_html, wrap_text};
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
    unique_id: &str,  // For idempotency
) -> ApiResult<Uuid> {
    let template = my_email(username, data);

    // Idempotency key prevents duplicate sends
    let idempotency_key = format!("my_email:{}:{}", to_email, unique_id);

    self.queue.enqueue(
        &idempotency_key,
        to_email,
        Some(username),
        "Subject Line",
        &template.html,
        &template.text,
    ).await
}
```

## Job Lifecycle

```
┌─────────┐     ┌────────────┐     ┌──────┐
│ PENDING │────▶│ PROCESSING │────▶│ SENT │
└─────────┘     └────────────┘     └──────┘
                      │
                      ▼ (on failure)
                ┌────────┐
                │ FAILED │──────┐
                └────────┘      │ (retry with backoff)
                      ▲         │
                      └─────────┘
                      │
                      ▼ (max retries exceeded)
               ┌─────────────┐
               │ DEAD_LETTER │
               └─────────────┘
```

## Retry & Backoff

- **Max retries**: 3 (configurable per job)
- **Backoff formula**: `60 * 2^retry_count` seconds
  - Retry 1: 2 minutes
  - Retry 2: 4 minutes
  - Retry 3: 8 minutes (then dead-letter)

## Idempotency

Each email type generates a unique `idempotency_key`:

| Email Type | Key Format | Duplicates Prevented |
|------------|------------|---------------------|
| Verification | `verification:{email}:{token}` | Per token |
| Password Reset | `password_reset:{email}:{token}` | Per token |
| Welcome | `welcome:{email}` | One per user ever |
| Notification | `notification:{email}:{id}` | Per notification |

If a job with the same key exists, `enqueue()` returns the existing job ID.

## Database Schema

```sql
CREATE TABLE email_jobs (
    id UUID PRIMARY KEY,
    idempotency_key VARCHAR(255) UNIQUE NOT NULL,
    to_email VARCHAR(255) NOT NULL,
    to_name VARCHAR(255),
    subject VARCHAR(500) NOT NULL,
    html_body TEXT NOT NULL,
    text_body TEXT NOT NULL,
    status email_job_status NOT NULL DEFAULT 'pending',
    retry_count INTEGER NOT NULL DEFAULT 0,
    max_retries INTEGER NOT NULL DEFAULT 3,
    next_retry_at TIMESTAMP,
    last_error TEXT,
    created_at TIMESTAMP NOT NULL,
    updated_at TIMESTAMP NOT NULL,
    sent_at TIMESTAMP
);

-- Indexes
CREATE INDEX idx_email_jobs_status_next_retry ON email_jobs(status, next_retry_at);
CREATE INDEX idx_email_jobs_idempotency_key ON email_jobs(idempotency_key);
```

## Configuration

Required environment variables for email sending:

```env
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=noreply@cosmicforge.com
SMTP_FROM_NAME=CosmicForge
APP_URL=https://yourapp.com
```

**Note**: If SMTP is not configured:
- Emails will still be queued to the database
- Worker will not start
- Jobs remain in `pending` status

## Worker Configuration

In `workers/email.rs`:

```rust
const POLL_INTERVAL_SECS: u64 = 5;   // Check for jobs every 5 seconds
const BATCH_SIZE: u64 = 10;           // Process up to 10 jobs per batch
const BASE_BACKOFF_SECS: i64 = 60;    // Base retry delay
```

Also mention:  
- These constants control polling, batch size, and retry backoff.
- Can be changed for tuning performance.

---

### Concurrency

Email jobs are processed concurrently with a limit of 3 jobs per batch:

```rust
iter(jobs)
    .for_each_concurrent(3, |job| async move { worker.process_job(job).await })
    .await;

- **Backoff formula**: `BASE_BACKOFF_SECS * 2^retry_count` seconds
  - Retry 1: 2 minutes
  - Retry 2: 4 minutes
  - Retry 3: 8 minutes (then dead-letter)



## MonitoringPENDING -> PROCESSING -> SENT
                  |
                  ▼ (on failure)
                FAILED -> (retry with exponential backoff)
                  |
                  ▼ (max retries exceeded)
               DEAD_LETTER

## Idempotency

Each email type generates a unique `idempotency_key`:

| Email Type    | Key Format                  | Prevents Duplicates          |
|---------------|----------------------------|------------------------------|
| Verification  | verification:{email}:{token} | Per token                    |
| Password Reset| password_reset:{email}:{token} | Per token                   |
| Welcome       | welcome:{email}             | One per user ever            |
| Notification  | notification:{email}:{id}  | Per notification             |

### Sending an Email

Emails are queued, not sent immediately:

```rust
let job_id = state.email_service
    .send_verification_email(&email, &username, &token)
    .await?;

SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USERNAME=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM_EMAIL=noreply@cosmicforge.com
SMTP_FROM_NAME=CosmicForge
APP_URL=https://yourapp.com


### Check pending jobs

```sql
SELECT COUNT(*) FROM email_jobs WHERE status = 'pending';
```

### Check failed jobs

```sql
SELECT * FROM email_jobs
WHERE status = 'failed'
ORDER BY updated_at DESC;
```

### Check dead-letter queue

```sql
SELECT * FROM email_jobs
WHERE status = 'dead_letter'
ORDER BY updated_at DESC;
```

### Retry a dead-letter job

```sql
UPDATE email_jobs
SET status = 'pending',
    retry_count = 0,
    next_retry_at = NOW()
WHERE id = 'job-uuid-here';
```

## Testing

Without SMTP configured, emails queue but don't send. Check the database:

```sql
SELECT id, to_email, subject, status, created_at
FROM email_jobs
ORDER BY created_at DESC
LIMIT 10;
```
