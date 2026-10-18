# CF Meet Messaging (Part V Workstreams A–D, ADR 0045 / 0047). Build context: the
# REPOSITORY ROOT (the service depends on the shared packages/service-auth and
# packages/messaging-crypto crates):
#
#   docker build -f services/messaging/Dockerfile -t cfmeet-messaging .
#
# Only the shared packages Messaging builds from (service-auth, messaging-crypto, its own
# contracts — the served OpenAPI document is compiled in) and services/messaging enter the
# build context (services/messaging/Dockerfile.dockerignore); no other service's source is
# copied. The image is independent of every other service's image and release.
#
# The image contains the service (`messaging`) and its migration runner
# (`messaging-migration`). Configuration — DATABASE_URL, the service signing key FILE, the
# trust bundle FILE and the Platform Access URL — is supplied at runtime only; nothing
# secret is baked in. Docker is never required for local development.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/messaging-crypto packages/messaging-crypto
COPY packages/contracts/messaging packages/contracts/messaging
COPY services/messaging services/messaging
WORKDIR /src/services/messaging
RUN cargo build --release --locked -p messaging-migration --bin messaging-migration \
 && cargo build --release --locked -p messaging --bin messaging

FROM debian:bookworm-slim
# Apply pending Debian security updates at build time; nothing else is installed.
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin messaging
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/messaging/target/release/messaging ./messaging
COPY --from=builder /src/services/messaging/target/release/messaging-migration ./messaging-migration
USER messaging
ENV MESSAGING_BIND_ADDRESS=0.0.0.0:8097 \
    LOG_FORMAT=json
EXPOSE 8097
CMD ["./messaging"]
