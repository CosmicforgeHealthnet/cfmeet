# CF Meet SMS Gateway (Part IV Workstreams M-O, ADR 0042). Build context: the REPOSITORY
# ROOT (the service depends on the shared packages/service-auth and packages/webhooks
# crates):
#
#   docker build -f services/sms-gateway/Dockerfile -t cfmeet-sms-gateway .
#
# Only the shared packages the gateway builds from (service-auth, webhooks, its own
# contracts — the served OpenAPI document is compiled in) and services/sms-gateway enter
# the build context (services/sms-gateway/Dockerfile.dockerignore); no other service's
# source is copied.
#
# The image contains the service (`sms-gateway`) and its migration runner
# (`sms-gateway-migration`). Configuration — DATABASE_URL, the service signing key FILE,
# the trust bundle FILE, the content / fingerprint key FILES, the provider callback secret
# FILE and the Platform Access / Numbering / Calling URLs — is supplied at runtime only;
# nothing secret is baked in. Docker is never required for local development.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/webhooks packages/webhooks
COPY packages/contracts/sms-gateway packages/contracts/sms-gateway
COPY services/sms-gateway services/sms-gateway
WORKDIR /src/services/sms-gateway
RUN cargo build --release --locked -p sms-gateway-migration --bin sms-gateway-migration \
 && cargo build --release --locked -p sms-gateway --bin sms-gateway

FROM debian:bookworm-slim
# Apply pending Debian security updates at build time; nothing else is installed.
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin smsgateway
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/sms-gateway/target/release/sms-gateway ./sms-gateway
COPY --from=builder /src/services/sms-gateway/target/release/sms-gateway-migration ./sms-gateway-migration
USER smsgateway
ENV SMS_GATEWAY_BIND_ADDRESS=0.0.0.0:8096 \
    LOG_FORMAT=json
EXPOSE 8096
CMD ["./sms-gateway"]
