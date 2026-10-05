# Real self-hosted LiveKit certification (Phase 2 Parts E–I and M)

Certifies the production `LiveKitProvider` against a real `livekit-server` and a real WebRTC
participant. No Docker.

1. Build `livekit-server` (e.g. from the Go module `github.com/livekit/livekit-server`,
   `go build ./cmd/server`) and the probe: `cd rtcprobe && CGO_ENABLED=0 go build -o rtcprobe .`
2. Start PostgreSQL and Redis; export `TEST_DATABASE_URL` and `TEST_REDIS_URL`.
3. `LIVEKIT_SERVER_BIN=/path/livekit-server RTCPROBE_BIN=/path/rtcprobe ./run.sh`

`livekit.yaml` binds to loopback only and uses a local certification key — it is not a
production configuration. Each step prints `CERT <capability>: PASS`.

Part F (`real_livekit_part_f_media_controls`) prints `CERT F <capability>: PASS` for host audio
mute, audio and video permission reduction, permission restore, screen-share policy enforcement,
the screen-share provider event (real `track_published` webhook) and host stop screen share. The
probe accepts `mic,camera,screen` as its publish list and a stdin line `publish <source>` to
attempt another publication on the live connection.

Make sure no earlier `livekit-server` is still shutting down (ports 7880/7881) before re-running.

## Part G (real Egress + S3) and Part M additions

Set `EGRESS_BIN` and `MINIO_BIN` to run the recording / object-storage certification. On a
host with Docker, the shims in this directory run the official containers:

```sh
EGRESS_BIN=$PWD/docker-egress.sh MINIO_BIN=$PWD/docker-minio.sh \
LIVEKIT_SERVER_BIN=/path/livekit-server RTCPROBE_BIN=/path/rtcprobe ./run.sh
```

`EGRESS_IMAGE` / `MINIO_IMAGE` override the images (default `livekit/egress:latest`,
`minio/minio:latest`; any S3-compatible MinIO build works).

Part M real scenarios (`CERT M …` lines):

- `real_livekit_m_waiting_subscription_ban` — waiting room → admit → real connect, real
  **subscription** of the host's audio + video (RTP received, counted by `rtcprobe`), ban →
  real disconnect, reconnect with the old credential evicted, no new credential.
- `real_livekit_m_media_load_and_large_audience` — media load in a TOWN_HALL conference:
  `MEDIA_LOAD_PUBLISHERS` presenters (capped stage) publish audio + video,
  `MEDIA_LOAD_SUBSCRIBERS` VIEWER attendees subscribe only, for `MEDIA_LOAD_SECS`.
  Prints `MEDIA_LOAD {json}` (join latency, failures, disconnects, subscriptions, RTP packets
  and gaps, livekit-server CPU / RSS) and writes `media-load-PxS.json` to `CFMEET_CERT_OUT`.
  Media delivery is reported as CLEAN (0 RTP gaps) or OBSERVED LIMIT; set
  `MEDIA_LOAD_MAX_GAP_PCT` to enforce an approved ceiling. Select it alone with
  `CERT_FILTER=real_livekit_m_media_load`.

`rtcprobe` additionally reports `track_subscribed` and periodic `stats`
(`subscribed`, `rtpPackets`, `rtpLost`, `bytes`).
