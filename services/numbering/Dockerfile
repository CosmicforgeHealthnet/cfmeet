# CF Meet Numbering (Part IV Workstreams C-D, ADR 0033). Build context: the REPOSITORY
# ROOT (the service depends on the shared packages/service-auth crate):
#
#   docker build -f services/numbering/Dockerfile -t cfmeet-numbering .
#
# Only the shared packages Numbering builds from (service-auth, object-storage, webhooks,
# its own contracts — the served OpenAPI document is compiled in) and services/numbering
# enter the build context (services/numbering/Dockerfile.dockerignore); no other service's
# source is copied.
#
# The image contains the service (`numbering`) and its migration runner
# (`numbering-migration`). Configuration — DATABASE_URL, the service signing key FILE,
# the trust bundle FILE and the Platform Access URL — is supplied at runtime only;
# nothing secret is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
# Part VI §55 unified usage facts (shared fact model / exporter).
COPY packages/usage-facts packages/usage-facts
COPY packages/object-storage packages/object-storage
COPY packages/webhooks packages/webhooks
COPY packages/contracts/numbering packages/contracts/numbering
COPY services/numbering services/numbering
WORKDIR /src/services/numbering
RUN cargo build --release --locked -p numbering-migration --bin numbering-migration \
 && cargo build --release --locked -p numbering --bin numbering

FROM debian:bookworm-slim
# Apply pending Debian security updates at build time; nothing else is installed.
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin numbering
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/numbering/target/release/numbering ./numbering
COPY --from=builder /src/services/numbering/target/release/numbering-migration ./numbering-migration
USER numbering
ENV NUMBERING_BIND_ADDRESS=0.0.0.0:8095 \
    LOG_FORMAT=json
EXPOSE 8095
CMD ["./numbering"]
