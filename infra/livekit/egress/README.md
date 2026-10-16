# LiveKit Egress — Calling application capture

Calling's application (RTC) voicemail and call recording run through LiveKit Egress
(ADR 0027). This directory holds the **certification-only** configuration; it is neutral
media infrastructure and shares nothing with the Meeting Service.

| File | Purpose |
|---|---|
| `livekit.calling-egress-cert.yaml` | LiveKit for the capture certification: `../livekit.calling-cert.yaml` + the Redis bus Egress needs + a private (non-loopback) media address for the headless-browser room composite |
| `egress.calling-cert.yaml` | The Egress worker: same Redis, the test LiveKit API key, uploads to the private test bucket |

Local certification (the values are well-known test-only credentials, not secrets):

```sh
redis-server --port 6379 --bind 127.0.0.1 --save '' &
livekit-server --config infra/livekit/egress/livekit.calling-egress-cert.yaml &
# an S3-compatible test store on 127.0.0.1:9100 with bucket calling-cert-artifacts
docker run -d --network host --cap-add SYS_ADMIN \
  -e EGRESS_CONFIG_FILE=/cfg/egress.yaml \
  -v "$PWD/infra/livekit/egress/egress.calling-cert.yaml:/cfg/egress.yaml:ro" \
  livekit/egress:v1.10.0
# then services/calling: CALLING_RTC_CERT_* and CALLING_RTC_CERT_EGRESS_S3_* (see
# tests/rtc_certification.rs) and `cargo test --test rtc_certification`.
```

Production: inject the whole Egress configuration as `EGRESS_CONFIG_BODY` from the secret
manager (API key / secret and bucket credentials are never committed); the Egress bucket
credentials may only write the capture staging prefix of the protected artifact bucket.
Calling is configured with `CALLING_CAPTURE_PROVIDER=livekit-egress` and
`CALLING_CAPTURE_STAGING_PREFIX` (default `capture-staging`). Clients never see Egress,
its credentials or its identifiers.
