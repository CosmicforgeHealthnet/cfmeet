# Real self-hosted LiveKit certification (Phase 2 Part E)

Certifies the production `LiveKitProvider` against a real `livekit-server` and a real WebRTC
participant. No Docker.

1. Build `livekit-server` (e.g. from the Go module `github.com/livekit/livekit-server`,
   `go build ./cmd/server`) and the probe: `cd rtcprobe && CGO_ENABLED=0 go build -o rtcprobe .`
2. Start PostgreSQL and Redis; export `TEST_DATABASE_URL` and `TEST_REDIS_URL`.
3. `LIVEKIT_SERVER_BIN=/path/livekit-server RTCPROBE_BIN=/path/rtcprobe ./run.sh`

`livekit.yaml` binds to loopback only and uses a local certification key — it is not a
production configuration. Each step prints `CERT <capability>: PASS`.
