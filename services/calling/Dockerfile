# CF Meet Calling service. Build context: the REPOSITORY ROOT (the service depends on
# the shared packages/service-auth, packages/object-storage and packages/webhooks crates):
#
#   docker build -f services/calling/Dockerfile -t cfmeet-calling .
#
# Only packages/service-auth, packages/object-storage, packages/webhooks and
# services/calling enter the
# build context (services/calling/Dockerfile.dockerignore); no other service's source is
# copied.
#
# The image contains the service (`calling`), its migration runner (`calling-migration`)
# and the operational CLI (`platform_admin`). Configuration — DATABASE_URL, the Platform
# Access base URL, the service signing key FILE and the trust bundle FILE — is
# supplied at runtime only; nothing secret is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/object-storage packages/object-storage
COPY packages/webhooks packages/webhooks
COPY services/calling services/calling
WORKDIR /src/services/calling
RUN cargo build --release --locked -p calling-migration --bin calling-migration \
 && cargo build --release --locked -p calling --bin calling --bin platform_admin

FROM debian:bookworm-slim
# Apply pending Debian security updates at build time (verified by the dependency-security
# workflow); nothing else is installed.
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
# No package installation beyond security updates: the CA bundle comes from the builder image.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin calling
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/calling/target/release/calling ./calling
COPY --from=builder /src/services/calling/target/release/calling-migration ./calling-migration
COPY --from=builder /src/services/calling/target/release/platform_admin ./platform_admin
USER calling
ENV CALLING_BIND_ADDRESS=0.0.0.0:8090 \
    LOG_FORMAT=json
EXPOSE 8090
CMD ["./calling"]
