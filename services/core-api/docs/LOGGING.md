# Logging System

Dual-output logging with terminal pretty-printing and JSON file rotation.

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌──────────────────┐
│  Application    │────▶│ tracing-subscriber│────▶│ Terminal (Pretty)│
│  (tracing logs) │     │   (EnvFilter)    │     └──────────────────┘
└─────────────────┘     └─────────────────┘
                               │
                               ▼
                        ┌──────────────────┐
                        │ File (JSON/Daily)│
                        └──────────────────┘
```

## Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `init_logging()` | `config/logging.rs` | Initializes logging system |
| `WorkerGuard` | Return value | Ensures logs flush on shutdown |
| `EnvFilter` | Runtime | Controls log levels via `RUST_LOG` |

## Folder Structure

```
backend/
├── src/
│   └── config/
│       ├── mod.rs
│       └── logging.rs     # Logging configuration
├── target/
│   └── logs/
│       └── dev/           # Development logs
│           └── backend.log.YYYY-MM-DD
└── logs/
    └── prod/              # Production logs
        └── backend.log.YYYY-MM-DD
```

## Output Formats

### Terminal (Development)

Pretty-printed, colored output for easy reading:

```
2026-01-18T10:30:45.123456Z  INFO backend::handlers::auth: User registered
    at src/handlers/auth.rs:56
    on ThreadId(7)
```

Features:
- ANSI colors
- Thread IDs
- Target modules
- File locations

### File (JSON)

Machine-readable JSON for log aggregators (ELK, Datadog, etc.):

```json
{"timestamp":"2026-01-18T10:30:45.123456Z","level":"INFO","target":"backend::handlers::auth","message":"User registered","thread_id":7}
```

Features:
- JSON format
- No ANSI codes
- Daily rotation
- Non-blocking writes

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `APP_ENV` | `dev` | Environment (`dev` or `prod`) |
| `RUST_LOG` | `backend=debug,tower_http=info` | Log level filter |

### Log Levels

```env
# Development (verbose)
RUST_LOG=debug

# Production (recommended)
RUST_LOG=backend=info,tower_http=warn

# Trace specific module
RUST_LOG=backend::handlers::auth=trace

# Multiple modules
RUST_LOG=backend=info,sea_orm=warn,tower_http=debug
```

### Log Level Hierarchy

| Level | Usage |
|-------|-------|
| `error` | Failures requiring attention |
| `warn` | Unexpected but recoverable issues |
| `info` | Normal operations (startup, requests) |
| `debug` | Detailed debugging information |
| `trace` | Very verbose, step-by-step tracing |

## Usage

### Initialization (main.rs)

```rust
use backend::config::logging;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging - keep guard alive for entire app lifetime
    let _guard = logging::init_logging();

    tracing::info!("Application started");

    // ... rest of application

    Ok(())
}
```

### Logging in Code

```rust
use tracing::{info, warn, error, debug, trace, instrument};

// Simple messages
tracing::info!("Server started on port {}", port);
tracing::warn!("Rate limit exceeded for user {}", user_id);
tracing::error!("Database connection failed: {}", err);

// Structured logging with fields
tracing::info!(
    user_id = %user.id,
    email = %user.email,
    "User logged in"
);

// Spans for request tracing
#[instrument(skip(state))]
pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> ApiResult<Json<RegisterResponse>> {
    tracing::debug!("Processing registration");
    // ...
}
```

### Log Output Examples

```rust
// Info level
tracing::info!("Email queued for {}", email);
// Output: 2026-01-18T10:30:45Z INFO backend::services::email: Email queued for user@example.com

// Error with context
tracing::error!(
    error = %e,
    user_id = %user.id,
    "Failed to send verification email"
);
// Output: 2026-01-18T10:30:45Z ERROR backend::handlers::auth: Failed to send verification email, error=SmtpError, user_id=550e8400-e29b-41d4-a716-446655440000
```

## File Rotation

Logs rotate daily with format: `backend.log.YYYY-MM-DD`

```
logs/prod/
├── backend.log.2026-01-16
├── backend.log.2026-01-17
└── backend.log.2026-01-18   # Current day
```

### Cleanup (Manual)

Remove logs older than 30 days:

```bash
# Linux/Mac
find logs/prod -name "backend.log.*" -mtime +30 -delete

# Windows PowerShell
Get-ChildItem logs\prod -Filter "backend.log.*" |
    Where-Object { $_.LastWriteTime -lt (Get-Date).AddDays(-30) } |
    Remove-Item
```

## Disabling SQLx Query Logging

SQLx query logging is disabled by default in `main.rs`:

```rust
let mut db_opts = ConnectOptions::new(&app_config.database_url);
db_opts.sqlx_logging(false);
let db = Database::connect(db_opts).await?;
```

To enable for debugging:

```rust
db_opts
    .sqlx_logging(true)
    .sqlx_logging_level(tracing::log::LevelFilter::Debug);
```

## Production Recommendations

### .env Configuration

```env
APP_ENV=prod
RUST_LOG=backend=info,tower_http=warn
```

### Log Aggregation

JSON logs can be shipped to:
- **ELK Stack** (Elasticsearch, Logstash, Kibana)
- **Datadog**
- **Grafana Loki**
- **AWS CloudWatch**

Example Filebeat config:

```yaml
filebeat.inputs:
  - type: log
    paths:
      - /app/logs/prod/backend.log.*
    json.keys_under_root: true
    json.add_error_key: true
```

## Dependencies

```toml
# Cargo.toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json"] }
tracing-appender = "0.2"
```

## Troubleshooting

### Logs not appearing

1. Check `RUST_LOG` is set correctly
2. Ensure `_guard` is kept alive (not dropped early)
3. Verify log directory exists and is writable

### Missing file logs

1. Check `APP_ENV` value (`dev` or `prod`)
2. Verify directory permissions:
   - Dev: `target/logs/dev/`
   - Prod: `logs/prod/`

### Performance issues

The file writer is non-blocking. If you see slowdowns:
1. Check disk I/O
2. Consider reducing log verbosity in production
3. Ensure log aggregator is consuming logs
