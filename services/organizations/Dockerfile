# CF Meet Organizations Service (Blueprint v1.0 Sections 3-5, ADR 0056). Build context:
# the REPOSITORY ROOT (the service depends on the shared packages/service-auth crate and
# compiles in its own contract):
#
#   docker build -f services/organizations/Dockerfile -t cfmeet-organizations .
#
# Only packages/service-auth, packages/contracts/organizations and services/organizations
# enter the build context (Dockerfile.dockerignore).
#
# The image contains the API (`organizations`, with its in-process maintenance loop), the
# migration runner (`organizations-migration`) and the migration import CLI
# (`organizations-import`). DATABASE_URL and the trust bundle FILE are supplied at runtime
# only; nothing secret is baked in. Not deployed by any workflow yet.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/contracts/organizations packages/contracts/organizations
COPY services/organizations services/organizations
WORKDIR /src/services/organizations
RUN cargo build --release --locked --workspace --bins

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin organizations
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/organizations/target/release/organizations ./organizations
COPY --from=builder /src/services/organizations/target/release/organizations-import ./organizations-import
COPY --from=builder /src/services/organizations/target/release/organizations-migration ./organizations-migration
USER organizations
ENV ORGANIZATIONS_BIND_ADDRESS=0.0.0.0:8101 \
    LOG_FORMAT=json
EXPOSE 8101
CMD ["./organizations"]
