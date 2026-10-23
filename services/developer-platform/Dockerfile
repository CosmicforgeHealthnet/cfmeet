# CF Meet Developer Platform (Part VI, ADR 0055). Build context: the REPOSITORY ROOT (the
# service depends on the shared packages/service-auth crate and compiles in its own
# contract):
#
#   docker build -f services/developer-platform/Dockerfile -t cfmeet-developer-platform .
#
# Only packages/service-auth, packages/webhooks (Section 53: the shared webhook
# primitives), packages/contracts/platform-developer and services/developer-platform enter
# the build context (Dockerfile.dockerignore).
#
# The image contains the API (`developer-platform`), the worker
# (`developer-platform-worker`) and the migration runner (`developer-platform-migration`).
# DATABASE_URL, the signing key FILE, the trust bundle FILE, the Platform Access URL and the
# webhook sealing key (DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_FILE) are supplied at runtime
# only; nothing secret is baked in. Not deployed by any workflow yet.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/webhooks packages/webhooks
COPY packages/contracts/platform-developer packages/contracts/platform-developer
COPY services/developer-platform services/developer-platform
WORKDIR /src/services/developer-platform
RUN cargo build --release --locked --workspace --bins

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin devplat
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/developer-platform/target/release/developer-platform ./developer-platform
COPY --from=builder /src/services/developer-platform/target/release/developer-platform-worker ./developer-platform-worker
COPY --from=builder /src/services/developer-platform/target/release/developer-platform-migration ./developer-platform-migration
USER devplat
ENV DEVELOPER_PLATFORM_BIND_ADDRESS=0.0.0.0:8098 \
    LOG_FORMAT=json
EXPOSE 8098
CMD ["./developer-platform"]
