# CF Meet Telephony service. Build context: the REPOSITORY ROOT (the service depends on
# the shared packages/service-auth crate):
#
#   docker build -f services/telephony/Dockerfile -t cfmeet-telephony .
#
# Only packages/service-auth and services/telephony enter the build context
# (services/telephony/Dockerfile.dockerignore); no other service's source is copied.
#
# The image contains the service (`telephony`), its migration runner
# (`telephony-migration`) and the operator CLI (`telephony-admin`). It does NOT contain
# the SIP / media edge (Asterisk runs as its own deployment, infra/telephony/) nor any
# trunk configuration. Configuration — DATABASE_URL, the service signing key FILE, the
# trust bundle FILE, the ARI credentials FILE and the kill switches — is supplied at
# runtime only; no key, trunk password or provider token is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY services/telephony services/telephony
WORKDIR /src/services/telephony
RUN cargo build --release --locked -p telephony-migration --bin telephony-migration \
 && cargo build --release --locked -p telephony --bin telephony --bin telephony-admin

FROM debian:bookworm-slim
# Apply pending Debian security updates at build time (verified by the dependency-security
# workflow); nothing else is installed.
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
# No package installation beyond security updates: the CA bundle comes from the builder image.
RUN useradd --system --uid 10002 --no-create-home --shell /usr/sbin/nologin telephony
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/telephony/target/release/telephony ./telephony
COPY --from=builder /src/services/telephony/target/release/telephony-migration ./telephony-migration
COPY --from=builder /src/services/telephony/target/release/telephony-admin ./telephony-admin
USER telephony
# Internal API only (never internet-exposed except the provider callback path through
# the ingress); metrics on their own listener when TELEPHONY_METRICS_ADDRESS is set.
ENV TELEPHONY_BIND_ADDRESS=0.0.0.0:8092 \
    LOG_FORMAT=json
EXPOSE 8092
CMD ["./telephony"]
