# CF Meet Calling service. Build context: services/calling/
#
#   docker build -t cfmeet-calling services/calling
#
# The image contains the service (`calling`), its migration runner (`calling-migration`)
# and the operational CLI (`platform_admin`). Configuration — including DATABASE_URL and
# API_CREDENTIAL_PEPPER — is supplied at runtime only; nothing secret is baked in.

FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --locked -p calling-migration --bin calling-migration \
 && cargo build --release --locked -p calling --bin calling --bin platform_admin

FROM debian:bookworm-slim
# No package installation: the CA bundle comes from the builder image.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin calling
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
WORKDIR /app
COPY --from=builder /app/target/release/calling ./calling
COPY --from=builder /app/target/release/calling-migration ./calling-migration
COPY --from=builder /app/target/release/platform_admin ./platform_admin
USER calling
ENV CALLING_BIND_ADDRESS=0.0.0.0:8090 \
    LOG_FORMAT=json
EXPOSE 8090
CMD ["./calling"]
