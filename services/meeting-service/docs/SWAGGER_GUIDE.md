# Swagger Documentation Guide

This guide explains how the CosmicForge RTC API documentation system works and how to extend it.

## Overview

The API uses **utoipa** for OpenAPI documentation with compile-time validation:

- Documentation is defined via Rust macros alongside handler code
- Swagger UI is served at `/swagger-ui`
- OpenAPI spec available at `/api-docs/openapi.json`

## How It Works

### 1. Handler Documentation

Each handler uses `#[utoipa::path]` macro to define its OpenAPI spec:

```rust
// In handlers/auth.rs
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered", body = RegisterResponse),
        (status = 400, description = "Validation error"),
        (status = 409, description = "User already exists")
    ),
    tag = "Authentication"
)]
pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> impl IntoResponse {
    // Implementation
}
```

### 2. Schema Documentation

DTOs use `#[derive(ToSchema)]` to generate schema definitions:

```rust
// In dto/auth.rs
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct RegisterRequest {
    /// Username for the new account
    #[schema(example = "john_doe", min_length = 3, max_length = 50)]
    pub username: String,

    /// Email address
    #[schema(example = "john@example.com")]
    pub email: String,

    /// Password (min 8 characters)
    #[schema(example = "securePassword123", min_length = 8)]
    pub password: String,
}
```

### 3. OpenAPI Configuration

The main API doc struct is in `src/swagger/mod.rs`:

```rust
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::handlers::auth::register,
        crate::handlers::auth::verify_email,
        crate::handlers::auth::login,
        crate::handlers::auth::refresh_token,
        crate::handlers::auth::logout,
    ),
    components(
        schemas(
            crate::dto::RegisterRequest,
            crate::dto::RegisterResponse,
            crate::dto::LoginRequest,
            crate::dto::LoginResponse,
            // ... other schemas
        )
    ),
    tags(
        (name = "Authentication", description = "User authentication endpoints")
    ),
    info(
        title = "CosmicForge RTC API",
        version = "1.0.0",
        description = "Real-Time Communication API"
    )
)]
pub struct ApiDoc;
```

### 4. Swagger UI Router

```rust
pub fn swagger_router() -> Router {
    SwaggerUi::new("/swagger-ui")
        .url("/api-docs/openapi.json", ApiDoc::openapi())
        .into()
}
```

## Adding a New Endpoint

### Step 1: Create the Handler

```rust
// In handlers/meetings.rs

#[utoipa::path(
    get,
    path = "/api/v1/meetings/{id}",
    params(
        ("id" = Uuid, Path, description = "Meeting ID")
    ),
    responses(
        (status = 200, description = "Meeting found", body = MeetingResponse),
        (status = 404, description = "Meeting not found")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Meetings"
)]
pub async fn get_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // Implementation
}
```

### Step 2: Create the DTOs

```rust
// In dto/meetings.rs

#[derive(Serialize, Deserialize, ToSchema)]
pub struct MeetingResponse {
    /// Meeting UUID
    pub id: Uuid,

    /// Human-readable meeting code
    #[schema(example = "MTG-12345")]
    pub meeting_identifier: String,

    /// Meeting title
    #[schema(example = "Team Standup")]
    pub title: String,

    /// Meeting status
    pub status: MeetingStatus,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub enum MeetingStatus {
    Scheduled,
    Ongoing,
    Ended,
    Cancelled,
}
```

### Step 3: Register in OpenAPI Config

Update `src/swagger/mod.rs`:

```rust
#[derive(OpenApi)]
#[openapi(
    paths(
        // Existing paths...
        crate::handlers::meetings::get_meeting,  // Add new path
    ),
    components(
        schemas(
            // Existing schemas...
            crate::dto::MeetingResponse,  // Add new schema
            crate::dto::MeetingStatus,
        )
    ),
    tags(
        (name = "Authentication", description = "User authentication endpoints"),
        (name = "Meetings", description = "Meeting management endpoints"),  // Add tag
    ),
    // ...
)]
pub struct ApiDoc;
```

### Step 4: Add Route

```rust
// In routes/meetings.rs

pub fn meetings_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(list_meetings).post(create_meeting))
        .route("/:id", get(get_meeting).put(update_meeting).delete(delete_meeting))
        .with_state(state)
}
```

## Common Patterns

### Query Parameters

```rust
#[utoipa::path(
    get,
    path = "/api/v1/meetings",
    params(
        ("page" = Option<u32>, Query, description = "Page number"),
        ("limit" = Option<u32>, Query, description = "Items per page"),
        ("status" = Option<String>, Query, description = "Filter by status")
    ),
    responses(
        (status = 200, description = "Meeting list", body = PaginatedMeetings)
    )
)]
```

### Request Body

```rust
#[utoipa::path(
    post,
    path = "/api/v1/meetings",
    request_body = CreateMeetingRequest,
    responses(
        (status = 201, description = "Meeting created", body = MeetingResponse)
    )
)]
```

### Security (JWT)

```rust
#[utoipa::path(
    // ...
    security(
        ("bearer_auth" = [])
    ),
)]
```

### Path Parameters

```rust
#[utoipa::path(
    delete,
    path = "/api/v1/meetings/{id}",
    params(
        ("id" = Uuid, Path, description = "Meeting ID to delete")
    ),
    // ...
)]
```

## Schema Annotations

### Field Examples

```rust
#[derive(ToSchema)]
pub struct User {
    #[schema(example = "john@example.com")]
    pub email: String,

    #[schema(example = 25, minimum = 0, maximum = 150)]
    pub age: i32,

    #[schema(example = "2026-01-17T10:00:00Z")]
    pub created_at: DateTime<Utc>,
}
```

### Enums

```rust
#[derive(ToSchema, Serialize, Deserialize)]
pub enum UserRole {
    User,
    Admin,
}
```

### Optional Fields

```rust
#[derive(ToSchema)]
pub struct UpdateRequest {
    #[schema(nullable = true)]
    pub name: Option<String>,
}
```

## Viewing Documentation

1. **Start the server**:
   ```bash
   cargo run
   ```

2. **Access Swagger UI**:
   ```
   http://localhost:8080/swagger-ui
   ```

3. **Get OpenAPI JSON**:
   ```
   http://localhost:8080/api-docs/openapi.json
   ```

## Troubleshooting

### "Schema not found" Error

Ensure all schemas used in responses are registered in `#[openapi(components(schemas(...)))]`.

### Handler Not Appearing

1. Check the `#[utoipa::path]` macro is present
2. Verify the path is added to `paths(...)` in the OpenApi derive
3. Ensure the function is `pub`

### Schema Fields Not Showing

Add `#[schema(...)]` attributes for examples and documentation.

## Best Practices

1. **Add examples** - Use `#[schema(example = "...")]` for all fields
2. **Document all responses** - Include error responses (400, 401, 404, 500)
3. **Use tags** - Group related endpoints with tags
4. **Add descriptions** - Use doc comments (`///`) on structs and fields
5. **Keep DTOs separate** - Don't use database models directly in API

---

**Last Updated**: 2026-01-17
