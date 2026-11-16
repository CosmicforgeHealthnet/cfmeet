# CF Meet Unified Notification Service (docs/audits/notification-service, roadmap N2-N13).
# Build context: the REPOSITORY ROOT (shared packages and its contracts):
#
#   docker build -f services/notifications/Dockerfile -t cfmeet-notifications .
#
# Only the shared packages it depends on, packages/contracts/notifications and
# services/notifications enter the build context (Dockerfile.dockerignore).
#
# The image contains the API with its in-process workers (`notifications`) and the
# migration runner (`notifications-migration`). DATABASE_URL, the trust bundle, the data
# key and every provider credential are supplied at runtime as files; nothing secret is
# baked in. Deployed dark (every route OFF, no provider) by the coordinated release.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/migration-guard packages/migration-guard
COPY packages/notification-client packages/notification-client
COPY packages/push-transport packages/push-transport
COPY packages/webhooks packages/webhooks
COPY packages/communications-policy packages/communications-policy
COPY packages/contracts/notifications packages/contracts/notifications
COPY services/notifications services/notifications
WORKDIR /src/services/notifications
RUN cargo build --release --locked --workspace --bins

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin notifications
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/notifications/target/release/notifications ./notifications
COPY --from=builder /src/services/notifications/target/release/notifications-migration ./notifications-migration
USER notifications
ENV NOTIFICATIONS_BIND_ADDRESS=0.0.0.0:8102 \
    LOG_FORMAT=json
EXPOSE 8102
CMD ["./notifications"]
