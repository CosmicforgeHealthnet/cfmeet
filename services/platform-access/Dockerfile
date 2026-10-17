# CF Meet Platform Access (the shared identity authority, ADR 0032). Build context: the
# REPOSITORY ROOT (the service depends on the shared packages/service-auth crate):
#
#   docker build -f services/platform-access/Dockerfile -t cfmeet-platform-access .
#
# Only packages/service-auth and services/platform-access enter the build context
# (services/platform-access/Dockerfile.dockerignore); no other service's source is copied.
#
# The image contains the service (`platform-access`), its migration runner
# (`platform-access-migration`) and the operator CLI (`platform-access-admin`).
# Configuration — DATABASE_URL, API_CREDENTIAL_PEPPER (or _FILE), the service signing key
# FILE and the trust bundle FILE — is supplied at runtime only; nothing secret is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY services/platform-access services/platform-access
WORKDIR /src/services/platform-access
RUN cargo build --release --locked -p platform-access-migration --bin platform-access-migration \
 && cargo build --release --locked -p platform-access --bin platform-access --bin platform-access-admin

FROM debian:bookworm-slim
# Apply pending Debian security updates at build time; nothing else is installed.
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin platform-access
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/platform-access/target/release/platform-access ./platform-access
COPY --from=builder /src/services/platform-access/target/release/platform-access-migration ./platform-access-migration
COPY --from=builder /src/services/platform-access/target/release/platform-access-admin ./platform-access-admin
USER platform-access
ENV PLATFORM_ACCESS_BIND_ADDRESS=0.0.0.0:8094 \
    LOG_FORMAT=json
EXPOSE 8094
CMD ["./platform-access"]
