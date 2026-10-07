# infra/livekit

LiveKit configuration.

| File | Purpose |
|------|---------|
| `livekit.dev.yaml` | **Local development only.** Used by the root `docker-compose.yml` (`livekit` service). Uses LiveKit's public dev key pair (`devkey` / `secret`), binds to 127.0.0.1, and uses UDP 50000–50100. |
| `livekit.calling-cert.yaml` | **Calling RTC certification only** (`services/calling/tests/rtc_certification.rs`, `.github/workflows/calling-rtc-certification.yml`). Loopback, `auto_create: false` (only Calling creates rooms), a well-known test-only key pair, and signed webhooks to the certification test's listener (`127.0.0.1:18089`). Never used by a deployment. |

The core API (`services/core-api`) talks to LiveKit only by signing join tokens
with `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` and returning `LIVEKIT_URL` to
clients. Production LiveKit (self-hosted or cloud) is configured through those
environment variables. Production LiveKit server configuration will go in this
directory when it is version-controlled.

## Calling service

`services/calling` has its own, independent LiveKit integration (Part C §24): its own
`CALLING_LIVEKIT_*` configuration, its own `cfc-<call id>` room namespace, join-only
credentials and server-side admission. It shares nothing with the Meeting Service's
LiveKit usage. Calling's clients never connect to LiveKit's signaling port directly:
they connect to Calling's media connection gateway, which admits every connection
against Calling's database and relays it (ADR 0007 §5); media (WebRTC) still flows
between clients and LiveKit's RTC ports. Do not publish LiveKit's signaling port to
clients (defence in depth — security does not depend on it: no client holds a token
LiveKit accepts). To run the Calling certification locally without Docker, start any
LiveKit server binary with `livekit.calling-cert.yaml`, install
`services/calling/tests/rtc/requirements.txt` into a Python environment and run
`cargo test --test rtc_certification` with the `CALLING_RTC_CERT_*` variables documented
at the top of that test (`CALLING_RTC_CERT_REQUIRED=1` makes an unreachable server a
failure instead of a skip).
