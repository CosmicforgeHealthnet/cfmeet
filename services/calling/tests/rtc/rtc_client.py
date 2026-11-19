"""Real WebRTC client for the Calling RTC certification suite (Part C §49).

Driven by the Rust test over stdin/stdout, one JSON object per line. It connects to a
real LiveKit server with a Calling-issued join credential and reports what the media
server actually allows — it never decides anything itself.

Commands:
  {"cmd": "connect", "url": ..., "token": ...}         -> {"ok": true, "identity": ...}
  {"cmd": "publish", "kind": "audio"|"video"|"screen"} -> {"ok": bool, "error"?: ...}
                                                          (screen: a video track with the
                                                           SCREEN_SHARE source; ok only once
                                                           the media server receives it)
  {"cmd": "sources"}                                   -> {"ok": true, "sources": [TrackSource, ...]}
  {"cmd": "status"}                                    -> {"connected": bool, "subscribed": [...],
                                                           "remote": [...], "publications": [...],
                                                           "disconnect_reason": ...,
                                                           "can_publish": bool, "can_subscribe": bool}
  {"cmd": "wait_subscribed", "count": n, "timeout": s} -> {"ok": bool, "subscribed": [...]}
  {"cmd": "refresh_issuers"}                           -> {"ok": true, "issuers": [<key name>, ...]}
  {"cmd": "wait_disconnected", "timeout": s}           -> {"ok": bool, "reason": ...}
  {"cmd": "wait_permission", "publish": bool, "timeout": s} -> {"ok": bool}
  {"cmd": "token"}                                     -> {"token": <latest token the SDK holds>,
                                                           "refreshes": n}
  {"cmd": "wait_refreshed", "publish": bool, "timeout": s}
                                                       -> {"ok": bool, "token": <first refreshed
                                                           token whose grant has canPublish == publish>}
  {"cmd": "disconnect"}                                -> {"ok": true}
  {"cmd": "quit"}
"""

import asyncio
import math
import base64
import json
import logging
import sys

from livekit import rtc
from livekit.rtc._ffi_client import FfiClient
from livekit.rtc._proto import ffi_pb2 as proto_ffi


async def publish_track_acked(local, track, options):
    """LocalParticipant.publish_track of the pinned SDK (livekit 1.1.20), except that every
    room-queue item is acknowledged. The SDK's version raises on a refused publication
    without task_done(), and a cancelled wait leaves queued items unacknowledged; either
    blocks the room's event loop for good (no later events, not even "disconnected"), so
    the certification would observe the client instead of the server."""
    req = proto_ffi.FfiRequest()
    req.publish_track.track_handle = track._ffi_handle.handle
    req.publish_track.local_participant_handle = local._ffi_handle.handle
    req.publish_track.options.CopyFrom(options)
    queue = local._room_queue.subscribe()
    try:
        resp = FfiClient.instance.request(req)
        cb = await queue.wait_for(
            lambda e: e.publish_track.async_id == resp.publish_track.async_id
        )
        queue.task_done()
        if cb.publish_track.error:
            raise RuntimeError(cb.publish_track.error)
        publication = rtc.LocalTrackPublication(cb.publish_track.publication)
        publication._track = track
        track._info.sid = publication.sid
        local._track_publications[publication.sid] = publication
        return publication
    finally:
        local._room_queue.unsubscribe(queue)
        while not queue.empty():
            queue.get_nowait()
            queue.task_done()


class Client:
    def __init__(self):
        self.room = None
        self.subscribed = []
        self.disconnected = asyncio.Event()
        self.disconnect_reason = None
        self.tasks = []
        self.token = None
        self.refreshed = []

    def _wire(self, room):
        @room.on("track_subscribed")
        def _on_sub(track, publication, participant):
            self.subscribed.append({"kind": int(track.kind), "from": participant.identity,
                                    "source": int(publication.source)})

        @room.on("disconnected")
        def _on_disc(reason):
            self.disconnect_reason = str(reason)
            self.disconnected.set()

        # The SDK replaces its token with every refresh the server sends; capture each.
        @room.on("token_refreshed")
        def _on_refresh():
            self.refreshed.append(room._token)

    async def connect(self, url, token):
        self.token = token
        self.room = rtc.Room()
        self._wire(self.room)
        opts = rtc.RoomOptions(auto_subscribe=True)
        await asyncio.wait_for(self.room.connect(url, token, opts), 20)
        return {"ok": True, "identity": self.room.local_participant.identity}

    def permission(self):
        try:
            p = self.room.local_participant.permissions if self.room else None
        except Exception:  # never connected: there is no local participant
            p = None
        if p is None:
            return False, False
        return bool(p.can_publish), bool(p.can_subscribe)

    def sources(self):
        """The effective LiveKit publish sources of this participant (TrackSource numbers:
        1 camera, 2 microphone, 3 screen share, 4 screen share audio), as the server last
        granted them."""
        try:
            p = self.room.local_participant.permissions if self.room else None
        except Exception:  # never connected: there is no local participant
            p = None
        return {"ok": True, "sources": sorted(int(s) for s in (p.can_publish_sources if p else []))}

    async def publish(self, kind):
        if kind == "audio":
            source = rtc.AudioSource(48000, 1)
            track = rtc.LocalAudioTrack.create_audio_track("cert-mic", source)
            opts = rtc.TrackPublishOptions()
            opts.source = rtc.TrackSource.SOURCE_MICROPHONE

            # A 440 Hz tone (one second = 100 frames of 10 ms, an exact number of
            # cycles), so a capture of this microphone is verifiably not silence.
            tone = []
            for f in range(100):
                frame = rtc.AudioFrame.create(48000, 1, 480)
                for i in range(480):
                    frame.data[i] = int(6000 * math.sin(2 * math.pi * 440 * (f * 480 + i) / 48000))
                tone.append(frame)

            async def feed():
                n = 0
                while True:
                    await source.capture_frame(tone[n % 100])
                    n += 1
                    await asyncio.sleep(0.005)
        else:
            source = rtc.VideoSource(64, 64)
            screen = kind == "screen"
            track = rtc.LocalVideoTrack.create_video_track(
                "cert-screen" if screen else "cert-cam", source)
            opts = rtc.TrackPublishOptions()
            opts.source = (rtc.TrackSource.SOURCE_SCREENSHARE if screen
                           else rtc.TrackSource.SOURCE_CAMERA)

            async def feed():
                frame = rtc.VideoFrame(64, 64, rtc.VideoBufferType.RGBA, bytearray(64 * 64 * 4))
                while True:
                    source.capture_frame(frame)
                    await asyncio.sleep(0.05)
        try:
            await asyncio.wait_for(publish_track_acked(self.room.local_participant, track, opts), 8)
        except Exception as e:  # denied publications time out or raise
            return {"ok": False, "error": type(e).__name__}
        self.tasks.append(asyncio.create_task(feed()))
        if not await self.wait_sending(track):
            return {"ok": False, "error": "PublicationNeverNegotiated"}
        return {"ok": True}

    async def wait_sending(self, track, timeout=10.0):
        """Waits until the media server actually receives the published track: its RTP
        sender has sent packets, which needs a negotiated m-section on the (single) peer
        connection.

        Works around a defect of the pinned SDK's debounced publisher negotiation (livekit
        Rust SDK `RtcSession::debounce_negotiation`, still present in 0.12): a negotiation
        request that arrives after the 150 ms debounce timer fired but before that offer is
        created is silently dropped (`Debouncer::call` still succeeds on the running task).
        The debounced path is the one used here because Calling's join credential carries no
        media permission (join-then-admit), so LiveKit's join response has
        `fast_publish=false`. In single peer connection mode the server also asks for
        renegotiations itself (`MediaSectionsRequirement`, e.g. right after Calling's
        admission grants subscribe / publish), so a publish ~150 ms later can lose its
        negotiation: the new sender is never offered, LiveKit never sees the track
        ("publish time out") and no other participant can subscribe to it. The JS SDK of
        CF Meet's own clients serialises offers and re-runs a negotiation requested during an
        in-flight one (`PCTransport.negotiate` / `renegotiate`), so this is a test-client
        artefact, not a Calling or LiveKit server behaviour.

        When nothing is sent, ask the SDK for another publisher negotiation: registering an
        RPC method does exactly that (`LocalParticipant::register_rpc_method` ->
        `publisher_negotiation_needed`) and has no other effect on the session."""
        loop = asyncio.get_event_loop()
        end = loop.time() + timeout
        nudge_at = loop.time() + 1.0
        nudges = 0
        local = self.room.local_participant
        while loop.time() < end:
            try:
                stats = await track.get_stats()
            except Exception:
                stats = []
            if any(s.WhichOneof("stats") == "outbound_rtp" and s.outbound_rtp.sent.packets_sent > 0
                   for s in stats):
                if nudges:
                    logging.getLogger("rtc_client").warning(
                        "publication negotiated after %d renegotiation request(s)", nudges)
                return True
            if loop.time() >= nudge_at:
                nudges += 1
                method = f"cert-renegotiate-{nudges}"
                local.register_rpc_method(method, lambda _data: "")
                local.unregister_rpc_method(method)
                nudge_at = loop.time() + 1.0
            await asyncio.sleep(0.1)
        return False

    def connected(self):
        try:
            return bool(self.room is not None and self.room.isconnected() and not self.disconnected.is_set())
        except Exception:
            return False

    def status(self):
        can_publish, can_subscribe = self.permission()
        remote = []
        publications = []
        if self.room:
            try:
                for p in self.room.remote_participants.values():
                    remote.append(p.identity)
                    for pub in p.track_publications.values():
                        publications.append({"from": p.identity, "sid": pub.sid,
                                             "source": int(pub.source),
                                             "subscribed": bool(pub.subscribed)})
            except Exception:
                pass
        return {
            "connected": self.connected(),
            "subscribed": self.subscribed,
            "remote": remote,
            "publications": publications,
            "disconnect_reason": self.disconnect_reason,
            "can_publish": can_publish,
            "can_subscribe": can_subscribe,
        }

    async def wait_subscribed(self, count, timeout):
        loop = asyncio.get_event_loop()
        end = loop.time() + timeout
        while loop.time() < end:
            if len(self.subscribed) >= count:
                return {"ok": True, "subscribed": self.subscribed}
            await asyncio.sleep(0.1)
        return {"ok": False, "subscribed": self.subscribed}

    async def wait_disconnected(self, timeout):
        try:
            await asyncio.wait_for(self.disconnected.wait(), timeout)
            return {"ok": True, "reason": self.disconnect_reason}
        except asyncio.TimeoutError:
            return {"ok": False, "reason": None}

    async def wait_permission(self, publish, timeout):
        loop = asyncio.get_event_loop()
        end = loop.time() + timeout
        while loop.time() < end:
            if self.permission()[0] == publish:
                return {"ok": True}
            await asyncio.sleep(0.1)
        return {"ok": False}

    def latest_token(self):
        return {"token": self.refreshed[-1] if self.refreshed else self.token,
                "refreshes": len(self.refreshed)}

    def refresh_issuers(self):
        """Key NAMES (JWT iss) that signed each refreshed token; never the tokens."""
        issuers = []
        for token in self.refreshed:
            payload = token.split(".")[1]
            payload += "=" * (-len(payload) % 4)
            issuers.append(json.loads(base64.urlsafe_b64decode(payload)).get("iss"))
        return {"ok": True, "issuers": issuers}

    @staticmethod
    def grant(token):
        payload = token.split(".")[1]
        payload += "=" * (-len(payload) % 4)
        return json.loads(base64.urlsafe_b64decode(payload)).get("video", {})

    async def wait_refreshed(self, publish, timeout):
        loop = asyncio.get_event_loop()
        end = loop.time() + timeout
        while loop.time() < end:
            for token in self.refreshed:
                if bool(self.grant(token).get("canPublish")) == publish:
                    return {"ok": True, "token": token}
            await asyncio.sleep(0.1)
        return {"ok": False, "refreshes": len(self.refreshed)}

    async def disconnect(self):
        for t in self.tasks:
            t.cancel()
        if self.room:
            await self.room.disconnect()
        return {"ok": True}

    async def verify_received(self, kind, timeout):
        async def receive():
            wanted = rtc.TrackKind.KIND_VIDEO if kind == "video" else rtc.TrackKind.KIND_AUDIO
            while True:
                track = next((p.track for who in self.room.remote_participants.values()
                              for p in who.track_publications.values() if p.track and p.track.kind == wanted), None)
                if track:
                    break
                await asyncio.sleep(0.1)
            stream = rtc.VideoStream(track) if kind == "video" else rtc.AudioStream(track)
            try:
                async for event in stream:
                    if kind == "video" and event.frame.width > 0 and event.frame.height > 0:
                        return {"ok": True}
                    if kind == "audio" and max(abs(int(v)) for v in event.frame.data) > 100:
                        return {"ok": True}
            finally:
                await stream.aclose()
        return await asyncio.wait_for(receive(), timeout)


async def main():
    # SDK diagnostics go to stderr (the test harness keeps one log file per client).
    logging.basicConfig(level=logging.INFO, stream=sys.stderr,
                        format="%(asctime)s %(levelname)s %(name)s %(message)s")
    client = Client()
    reader = asyncio.StreamReader()
    await asyncio.get_event_loop().connect_read_pipe(
        lambda: asyncio.StreamReaderProtocol(reader), sys.stdin
    )
    while True:
        line = await reader.readline()
        if not line:
            break
        msg = json.loads(line)
        cmd = msg.get("cmd")
        try:
            if cmd == "connect":
                out = await client.connect(msg["url"], msg["token"])
            elif cmd == "publish":
                out = await client.publish(msg["kind"])
            elif cmd == "status":
                out = client.status()
            elif cmd == "sources":
                out = client.sources()
            elif cmd == "wait_subscribed":
                out = await client.wait_subscribed(msg["count"], msg["timeout"])
            elif cmd == "wait_disconnected":
                out = await client.wait_disconnected(msg["timeout"])
            elif cmd == "wait_permission":
                out = await client.wait_permission(msg["publish"], msg["timeout"])
            elif cmd == "token":
                out = client.latest_token()
            elif cmd == "refresh_issuers":
                out = client.refresh_issuers()
            elif cmd == "wait_refreshed":
                out = await client.wait_refreshed(msg["publish"], msg["timeout"])
            elif cmd == "disconnect":
                out = await client.disconnect()
            elif cmd == "verify_received":
                out = await client.verify_received(msg["kind"], msg.get("timeout", 10))
            elif cmd == "quit":
                await client.disconnect()
                print(json.dumps({"ok": True}), flush=True)
                break
            else:
                out = {"ok": False, "error": "unknown command"}
        except Exception as e:
            out = {"ok": False, "error": f"{type(e).__name__}: {e}"[:200]}
        print(json.dumps(out), flush=True)


if __name__ == "__main__":
    asyncio.run(main())
