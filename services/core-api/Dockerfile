# Build Stage
FROM rust:1-bookworm AS builder

WORKDIR /app

# libssl-dev and pkg-config are needed for verifying TLS certificates (lettre, reqwest)
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app/backend

# Copy source code
COPY . .

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
COPY --from=builder /app/backend/target/release/migration ./migration
COPY --from=builder /app/backend/target/release/backend ./server

# Expose the application port
EXPOSE 8080

# Environment variables should be passed at runtime using --env-file
# CMD to run the server
CMD ["./server"]
