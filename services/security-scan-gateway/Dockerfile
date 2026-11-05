# CosmicForge shared security scan gateway. Build context: the REPOSITORY ROOT:
#
#   docker build -f services/security-scan-gateway/Dockerfile -t cfmeet-security-scan-gateway .
#
# Only packages/service-auth, packages/scan-gateway-contract and this service enter the
# build context (Dockerfile.dockerignore). The image contains NO antivirus engine and NO
# signature database: it streams to an existing clamd. Keys, trust bundle and client
# registry are supplied at runtime as read-only files; nothing secret is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/scan-gateway-contract packages/scan-gateway-contract
COPY services/security-scan-gateway services/security-scan-gateway
WORKDIR /src/services/security-scan-gateway
RUN cargo build --release --locked --bin security-scan-gateway --bin security-scan-gateway-keygen --bin security-scan-gateway-probe

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10002 --no-create-home --shell /usr/sbin/nologin scan-gateway
WORKDIR /app
COPY --from=builder /src/services/security-scan-gateway/target/release/security-scan-gateway ./security-scan-gateway
COPY --from=builder /src/services/security-scan-gateway/target/release/security-scan-gateway-keygen ./security-scan-gateway-keygen
COPY --from=builder /src/services/security-scan-gateway/target/release/security-scan-gateway-probe ./security-scan-gateway-probe
USER scan-gateway
ENV SCAN_GATEWAY_BIND_ADDRESS=0.0.0.0:8099
EXPOSE 8099
CMD ["./security-scan-gateway"]
