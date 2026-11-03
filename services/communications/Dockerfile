FROM rust:1-bookworm AS builder
WORKDIR /src
COPY packages/service-auth packages/service-auth
COPY packages/communications-policy packages/communications-policy
COPY services/communications services/communications
WORKDIR /src/services/communications
RUN cargo build --release --locked --bin communications \
 && cargo build --release --locked -p communications-migration --bin communications-migration

FROM debian:bookworm-slim
RUN apt-get update && apt-get -y --no-install-recommends upgrade \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin communications
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /src/services/communications/target/release/communications ./communications
COPY --from=builder /src/services/communications/target/release/communications-migration ./communications-migration
USER communications
ENV COMMUNICATIONS_BIND_ADDRESS=0.0.0.0:8080
EXPOSE 8080
CMD ["./communications"]
