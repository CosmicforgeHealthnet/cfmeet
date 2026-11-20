# CF Meet Admin Bridge (ADR 0068, docs/admin/cfmeet-admin-bridge.md). Build context: the
# REPOSITORY ROOT (the bridge depends on the shared packages/service-auth crate):
#
#   docker build -f services/admin-bridge/Dockerfile -t cfmeet-admin-bridge .
#
# Only packages/service-auth, packages/migration-guard and services/admin-bridge enter the
# build context (Dockerfile.dockerignore).
#
# The image contains the bridge (`admin-bridge`), the migration runner
# (`admin-bridge-migration`) and the operator tool (`admin-bridge-admin`). Everything is
# OFF by default (ADMIN_BRIDGE_ENABLED=false): only the loopback ops listener runs.
# DATABASE_URL, TLS files, HMAC key files and the policy key are supplied at runtime only;
# nothing secret is baked in. Not deployed by any workflow yet.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
# SEC-015 destructive migration subcommand guard (path dependency of the migration crate).
COPY packages/migration-guard packages/migration-guard
COPY services/admin-bridge services/admin-bridge
WORKDIR /src/services/admin-bridge
RUN cargo build --release --locked --workspace --bins

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin admin-bridge
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/admin-bridge/target/release/admin-bridge ./admin-bridge
COPY --from=builder /src/services/admin-bridge/target/release/admin-bridge-admin ./admin-bridge-admin
COPY --from=builder /src/services/admin-bridge/target/release/admin-bridge-migration ./admin-bridge-migration
USER admin-bridge
# The HQ listener binds the private interface given at deploy time; the ops listener is
# loopback-only (enforced by configuration).
ENV ADMIN_BRIDGE_HQ_BIND_ADDRESS=0.0.0.0:8443 \
    LOG_FORMAT=json
EXPOSE 8443
CMD ["./admin-bridge"]
