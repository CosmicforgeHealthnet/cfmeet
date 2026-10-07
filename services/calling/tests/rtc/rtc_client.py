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
  {"cmd": "disconnect"}                                -> {"ok": true}
  {"cmd": "quit"}
"""

import asyncio
import json
import sys

from livekit import rtc


class Client:
    def __init__(self):
        self.room = None
        self.subscribed = []
        self.disconnected = asyncio.Event()
        self.disconnect_reason = None
        self.tasks = []

    def _wire(self, room):
        @room.on("track_subscribed")
        def _on_sub(track, publication, participant):
            self.subscribed.append({"kind": int(track.kind), "from": participant.identity})

        @room.on("disconnected")
        def _on_disc(reason):
            self.disconnect_reason = str(reason)
            self.disconnected.set()

    async def connect(self, url, token):
        self.room = rtc.Room()
        self._wire(self.room)
        opts = rtc.RoomOptions(auto_subscribe=True)
        await asyncio.wait_for(self.room.connect(url, token, opts), 20)
        return {"ok": True, "identity": self.room.local_participant.identity}

    def permission(self):
        p = self.room.local_participant.permissions if self.room else None
        if p is None:
            return False, False
        return bool(p.can_publish), bool(p.can_subscribe)

    async def publish(self, kind):
        if kind == "audio":
            source = rtc.AudioSource(48000, 1)
            track = rtc.LocalAudioTrack.create_audio_track("cert-mic", source)
            opts = rtc.TrackPublishOptions()
            opts.source = rtc.TrackSource.SOURCE_MICROPHONE

            async def feed():
                frame = rtc.AudioFrame.create(48000, 1, 480)
                while True:
                    await source.capture_frame(frame)
                    await asyncio.sleep(0.01)
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
            await asyncio.wait_for(self.room.local_participant.publish_track(track, opts), 8)
        except Exception as e:  # denied publications time out or raise
            return {"ok": False, "error": type(e).__name__}
        self.tasks.append(asyncio.create_task(feed()))
        return {"ok": True}

    def status(self):
        can_publish, can_subscribe = self.permission()
        remote = []
        if self.room:
            for p in self.room.remote_participants.values():
                remote.append(p.identity)
        return {
            "connected": bool(self.room) and self.room.isconnected() and not self.disconnected.is_set(),
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

    async def disconnect(self):
        for t in self.tasks:
            t.cancel()
        if self.room:
            await self.room.disconnect()
        return {"ok": True}


async def main():
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
