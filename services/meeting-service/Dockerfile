# Build Stage
FROM rust:1-bookworm AS builder

WORKDIR /app

# libssl-dev and pkg-config are needed for verifying TLS certificates (lettre, reqwest)
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app/services/meeting-service

# Build context is the repository root: include only this service and the
# checked-in platform OpenAPI contracts shown in its Swagger service selector.
COPY services/meeting-service/ ./
COPY packages/contracts/ /app/packages/contracts/
COPY packages/service-auth/ /app/packages/service-auth/
# SEC-015 destructive migration subcommand guard (path dependency of the migration crate).
COPY packages/migration-guard/ /app/packages/migration-guard/
COPY packages/communications-policy /app/packages/communications-policy
# Part VI §55 unified usage facts (shared fact model / exporter).
COPY packages/usage-facts/ /app/packages/usage-facts/
# Dev-dependency of the end-to-end tests only (Cargo resolves every path manifest).
COPY packages/messaging-crypto/ /app/packages/messaging-crypto/

# Build for release
# Build the migration binary from the migration package
RUN cargo build --release --jobs 1 -p migration --bin migration --bin account-security-migration --bin meetings-calendar-migration --bin profile-notifications-migration --bin account-communications-migration
# Build the backend binary (and the read-only organization export used by the Blueprint
# Section 4 cutover runbook) from the root package
RUN cargo build --release --jobs 1 -p backend --bin backend --bin export_organizations

# Runtime Stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy binaries from builder
COPY --from=builder /app/services/meeting-service/target/release/migration ./migration
COPY --from=builder /app/services/meeting-service/target/release/account-security-migration ./account-security-migration
COPY --from=builder /app/services/meeting-service/target/release/meetings-calendar-migration ./meetings-calendar-migration
COPY --from=builder /app/services/meeting-service/target/release/profile-notifications-migration ./profile-notifications-migration
COPY --from=builder /app/services/meeting-service/target/release/account-communications-migration ./account-communications-migration
COPY --from=builder /app/services/meeting-service/target/release/backend ./server
COPY --from=builder /app/services/meeting-service/target/release/export_organizations ./export-organizations

# Expose the application port
EXPOSE 8080

# Environment variables should be passed at runtime using --env-file
# CMD to run the server
CMD ["./server"]
