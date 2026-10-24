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
# Part VI §55 unified usage facts (shared fact model / exporter).
COPY packages/usage-facts/ /app/packages/usage-facts/
# Dev-dependency of the end-to-end tests only (Cargo resolves every path manifest).
COPY packages/messaging-crypto/ /app/packages/messaging-crypto/

# Build for release
# Build the migration binary from the migration package
RUN cargo build --release --jobs 1 -p migration --bin migration
# Build the backend binary from the root package
RUN cargo build --release --jobs 1 -p backend --bin backend

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
COPY --from=builder /app/services/meeting-service/target/release/backend ./server

# Expose the application port
EXPOSE 8080

# Environment variables should be passed at runtime using --env-file
# CMD to run the server
CMD ["./server"]
