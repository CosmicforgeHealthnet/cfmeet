# CF Meet Privacy Relay service. Build context: the REPOSITORY ROOT (the service depends
# on the shared packages/service-auth crate):
#
#   docker build -f services/privacy-relay/Dockerfile -t cfmeet-privacy-relay .
#
# Only packages/service-auth and services/privacy-relay enter the build context
# (services/privacy-relay/Dockerfile.dockerignore); no other service's source is copied.
#
# The image contains the service (`privacy-relay`) and its migration runner
# (`privacy-relay-migration`). Configuration — DATABASE_URL, the service signing key
# FILE and the trust bundle FILE — is supplied at runtime only; nothing secret is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY services/privacy-relay services/privacy-relay
WORKDIR /src/services/privacy-relay
RUN cargo build --release --locked -p privacy-relay-migration --bin privacy-relay-migration \
 && cargo build --release --locked -p privacy-relay --bin privacy-relay

FROM debian:bookworm-slim
# No package installation: the CA bundle comes from the builder image.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin privacy-relay
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/privacy-relay/target/release/privacy-relay ./privacy-relay
COPY --from=builder /src/services/privacy-relay/target/release/privacy-relay-migration ./privacy-relay-migration
USER privacy-relay
ENV PRIVACY_RELAY_BIND_ADDRESS=0.0.0.0:8091 \
    LOG_FORMAT=json
EXPOSE 8091
CMD ["./privacy-relay"]
