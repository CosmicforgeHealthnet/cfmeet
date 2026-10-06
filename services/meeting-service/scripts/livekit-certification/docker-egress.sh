#!/bin/sh
# EGRESS_BIN shim for run.sh: runs the livekit/egress container with EGRESS_CONFIG_FILE.
# EGRESS_IMAGE overrides the image; the default is the exact image certified in Phase 2
# Part G (livekit/egress v1.14.1, pinned by digest). The container runs as a non-root
# user, so the config is copied somewhere it can read.
cfg_dir="$(mktemp -d /tmp/cfmeet-egress.XXXXXX)"
chmod 755 "$cfg_dir"
cp "$EGRESS_CONFIG_FILE" "$cfg_dir/egress.yaml"
chmod 644 "$cfg_dir/egress.yaml"
docker rm -f cfmeet-cert-egress >/dev/null 2>&1
trap 'docker rm -f cfmeet-cert-egress >/dev/null 2>&1; rm -rf "$cfg_dir"' TERM INT EXIT
docker run --rm --name cfmeet-cert-egress --network host --cap-add SYS_ADMIN --shm-size 1g \
  -e EGRESS_CONFIG_FILE=/cfg/egress.yaml -v "$cfg_dir":/cfg \
  "${EGRESS_IMAGE:-livekit/egress@sha256:bf2b648b947349c3e9ff7aa8c718f00378d5c06af7624652a3653318e00333ce}" &
wait $!
