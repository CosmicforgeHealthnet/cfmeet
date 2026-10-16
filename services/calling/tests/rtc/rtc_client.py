"""Real WebRTC client for the Calling RTC certification suite (Part C §49).

Driven by the Rust test over stdin/stdout, one JSON object per line. It connects to a
real LiveKit server with a Calling-issued join credential and reports what the media
server actually allows — it never decides anything itself.

Commands:
  {"cmd": "connect", "url": ..., "token": ...}         -> {"ok": true, "identity": ...}
  {"cmd": "publish", "kind": "audio"|"video"}          -> {"ok": bool, "error"?: ...}
  {"cmd": "status"}                                    -> {"connected": bool, "subscribed": [...],
                                                           "remote": [...], "disconnect_reason": ...,
                                                           "can_publish": bool, "can_subscribe": bool}
  {"cmd": "wait_subscribed", "count": n, "timeout": s} -> {"ok": bool, "subscribed": [...]}
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
            self.subscribed.append({"kind": int(track.kind), "from": participant.identity})

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
            track = rtc.LocalVideoTrack.create_video_track("cert-cam", source)
            opts = rtc.TrackPublishOptions()
            opts.source = rtc.TrackSource.SOURCE_CAMERA

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
        return {"ok": True}

    def connected(self):
        try:
            return bool(self.room is not None and self.room.isconnected() and not self.disconnected.is_set())
        except Exception:
            return False

    def status(self):
        can_publish, can_subscribe = self.permission()
        remote = []
        if self.room:
            try:
                for p in self.room.remote_participants.values():
                    remote.append(p.identity)
            except Exception:
                pass
        return {
            "connected": self.connected(),
            "subscribed": self.subscribed,
            "remote": remote,
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
            elif cmd == "wait_subscribed":
                out = await client.wait_subscribed(msg["count"], msg["timeout"])
            elif cmd == "wait_disconnected":
                out = await client.wait_disconnected(msg["timeout"])
            elif cmd == "wait_permission":
                out = await client.wait_permission(msg["publish"], msg["timeout"])
            elif cmd == "token":
                out = client.latest_token()
            elif cmd == "wait_refreshed":
                out = await client.wait_refreshed(msg["publish"], msg["timeout"])
            elif cmd == "disconnect":
                out = await client.disconnect()
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
