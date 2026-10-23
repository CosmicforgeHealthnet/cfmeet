# CF Meet public API Gateway (Part VI, ADR 0055). Build context: the REPOSITORY ROOT:
#
#   docker build -f services/api-gateway/Dockerfile -t cfmeet-api-gateway .
#
# Only packages/service-auth, services/api-gateway, the generated route manifest
# (infra/api-gateway/route-manifest.json) and the generated public OpenAPI documents
# (packages/contracts/bundled/public) enter the build; the latter two are embedded in the
# binary at compile time. The image contains the gateway only (no database, no migrations). The signing key FILE and the
# Platform Access / upstream URLs are supplied at runtime; public routing is OFF unless
# API_GATEWAY_PUBLIC_ROUTING_ENABLED=true. Not deployed by any workflow yet; live Nginx
# routing is unchanged.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY services/api-gateway services/api-gateway
COPY infra/api-gateway/route-manifest.json infra/api-gateway/route-manifest.json
COPY packages/contracts/bundled/public packages/contracts/bundled/public
WORKDIR /src/services/api-gateway
RUN cargo build --release --locked --bin api-gateway

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin apigateway
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/api-gateway/target/release/api-gateway ./api-gateway
USER apigateway
ENV API_GATEWAY_BIND_ADDRESS=0.0.0.0:8099 \
    LOG_FORMAT=json
EXPOSE 8099
CMD ["./api-gateway"]
