"""A synthetic meeting through the public app API, real RTC, Egress and object store.

Only short-lived controlled user tokens enter this disposable client. No SFU admin
key, MinIO key, real participant media or recording URL is printed or retained.
"""
import asyncio
import datetime
import hashlib
import json
import math
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

BASE = 'https://api.cfmeet.com'
OUT = Path('/result')
REPORT = {'result': 'FAIL', 'checks': {}, 'phase': 'setup'}
TOKENS = {}


def api(method, path, body=None, actor='owner'):
    headers = {'Content-Type': 'application/json', 'Authorization': 'Bearer ' + TOKENS[actor]}
    request = urllib.request.Request(BASE + '/api/v2' + path,
        data=json.dumps(body).encode() if body is not None else None, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            data = response.read(1048577)
            assert len(data) <= 1048576, 'oversized API response'
            return response.status, json.loads(data) if data else None
    except urllib.error.HTTPError as error:
        return error.code, None


def success(method, path, body=None):
    status, value = api(method, path, body)
    assert 200 <= status < 300, 'meeting API action failed'
    return value


async def wait_artifact(path, expected, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = await asyncio.to_thread(success, 'GET', path)
        assert value['status'] not in ('FAILED', 'ABORTED'), 'capture or finalization failed'
        if value['status'] == expected:
            return value
        await asyncio.sleep(2)
    raise RuntimeError('artifact state timed out')


async def main():
    global TOKENS
    TOKENS = json.loads((OUT / 'credentials.private.json').read_text())
    from livekit import rtc
    room, feed_task = rtc.Room(), None
    space = conference = recording = None
    try:
        space = await asyncio.to_thread(success, 'POST', '/meeting-spaces',
            {'title': 'Recording acceptance ' + uuid.uuid4().hex, 'accessPolicy': 'PUBLIC',
             'recordingPolicy': 'HOST_OPTIONAL'})
        sid = str(uuid.UUID(space['id']))
        conference = await asyncio.to_thread(success, 'POST', '/meeting-spaces/' + sid + '/conferences', {})
        cid = str(uuid.UUID(conference['id']))
        (OUT / 'cleanup.json').write_text(json.dumps(dict(space=sid, conference=cid)))
        base = '/conferences/' + cid
        await asyncio.to_thread(success, 'POST', base + '/start', {})
        joined = await asyncio.to_thread(success, 'POST', base + '/join', {})
        assert joined['rtcStatus'] == 'ISSUED', 'RTC credential not issued'
        endpoint = joined['rtc']['url']
        parsed = urllib.parse.urlsplit(endpoint or '')
        assert parsed.scheme == 'wss' and parsed.hostname in ('api.cfmeet.com', 'livekit.cfmeet.com'), 'unexpected RTC endpoint'
        REPORT['phase'] = 'synthetic_media'
        await asyncio.wait_for(room.connect(endpoint, joined['rtc']['credential']), 30)
        source = rtc.AudioSource(48000, 1)
        track = rtc.LocalAudioTrack.create_audio_track('synthetic-recording-acceptance', source)
        await asyncio.wait_for(room.local_participant.publish_track(track,
            rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE)), 15)
        video_source = rtc.VideoSource(320, 180)
        video_track = rtc.LocalVideoTrack.create_video_track('synthetic-recording-video', video_source)
        await asyncio.wait_for(room.local_participant.publish_track(video_track,
            rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_CAMERA)), 15)
        async def tone():
            n = 0
            while True:
                frame = rtc.AudioFrame.create(48000, 1, 480)
                for i in range(480):
                    frame.data[i] = int(6000 * math.sin(2 * math.pi * 440 * (n * 480 + i) / 48000))
                await source.capture_frame(frame)
                if n % 10 == 0:
                    color = bytearray(((n // 10) % 256, 96, 192, 255)) * (320 * 180)
                    video_source.capture_frame(rtc.VideoFrame(320, 180, rtc.VideoBufferType.RGBA, color))
                n += 1
                await asyncio.sleep(.01)
        feed_task = asyncio.create_task(tone())
        REPORT['checks']['real_rtc_synthetic_media'] = 'PASS'
        REPORT['phase'] = 'capture'
        result = await asyncio.to_thread(success, 'POST', base + '/recordings', {'idempotencyKey': uuid.uuid4().hex})
        recording = str(uuid.UUID(result['recording']['id']))
        (OUT / 'cleanup.json').write_text(json.dumps(dict(space=sid, conference=cid, recording=recording)))
        artifact = base + '/artifacts/' + recording
        await wait_artifact(artifact, 'RECORDING', 90)
        await asyncio.sleep(8)
        await asyncio.to_thread(success, 'POST', base + '/recordings/' + recording + '/stop', {})
        ready = await wait_artifact(artifact, 'READY', 180)
        retention = datetime.datetime.fromisoformat(ready['retentionAt'].replace('Z', '+00:00'))
        remaining = (retention - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
        assert 29 * 86400 < remaining <= 31 * 86400, 'standard recording retention differs from 30 days'
        REPORT['checks']['default_retention_30_days'] = 'PASS'
        REPORT['checks']['host_start_stop_and_object_finalization'] = 'PASS'
        REPORT['phase'] = 'authorized_download'
        denied, _ = await asyncio.to_thread(api, 'POST', artifact + '/access', {}, 'outsider')
        assert denied in (403, 404), 'another account obtained recording access'
        access = await asyncio.to_thread(success, 'POST', artifact + '/access', {})
        url = urllib.parse.urlsplit(access['url'])
        assert url.scheme == 'https' and url.hostname == 'api.cfmeet.com' and url.path.startswith('/cfmeet-meeting-artifacts/'), 'unexpected recording store'
        assert 0 < access['expiresInSeconds'] <= 900, 'recording URL lifetime exceeds limit'
        with urllib.request.urlopen(access['url'], timeout=30) as response:
            data = response.read(20 * 1024 * 1024 + 1)
        assert 0 < len(data) <= 20 * 1024 * 1024, 'unexpected recording size'
        assert hashlib.sha256(data).hexdigest() == ready['checksumSha256'], 'recording checksum differs'
        movie = Path('/tmp/recording.mp4')
        movie.write_bytes(data)
        result = subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'format=duration',
                                 '-show_entries', 'stream=codec_type', '-of', 'json', str(movie)],
                                capture_output=True, text=True, timeout=20)
        media = json.loads(result.stdout)
        assert result.returncode == 0 and float(media['format']['duration']) >= 3, 'recording is not playable media'
        assert {'audio', 'video'} <= {stream['codec_type'] for stream in media['streams']}, 'synthetic audio or video missing'
        REPORT['checks']['signed_download_checksum_and_playable_audio_video'] = 'PASS'
        REPORT['checks']['unrelated_account_denied'] = 'PASS'
        try:
            urllib.request.urlopen(urllib.parse.urlunsplit(url._replace(query='')), timeout=15)
            raise AssertionError('unsigned recording was readable')
        except urllib.error.HTTPError as error:
            assert error.code == 403, 'unexpected unsigned recording response'
        REPORT['checks']['anonymous_storage_denied'] = 'PASS'
        REPORT['phase'] = 'deletion'
        await asyncio.to_thread(success, 'DELETE', artifact)
        await wait_artifact(artifact, 'DELETED', 90)
        try:
            urllib.request.urlopen(access['url'], timeout=15)
            raise AssertionError('deleted recording bytes were still readable')
        except urllib.error.HTTPError as error:
            assert error.code == 404, 'deleted recording object absence was not confirmed'
        REPORT['checks']['authenticated_recording_deletion'] = 'PASS'
        REPORT['result'] = 'PASS'
    finally:
        if feed_task:
            feed_task.cancel()
            try:
                await feed_task
            except asyncio.CancelledError:
                pass
        await room.disconnect()
        if conference:
            base = '/conferences/' + str(uuid.UUID(conference['id']))
            if recording:
                await asyncio.to_thread(api, 'POST', base + '/recordings/' + recording + '/stop', {})
                await asyncio.to_thread(api, 'DELETE', base + '/artifacts/' + recording)
            code, _ = await asyncio.to_thread(api, 'POST', base + '/end', {})
            assert code in (200, 202, 409), 'controlled conference cleanup failed'
        if space:
            code, _ = await asyncio.to_thread(api, 'POST', '/meeting-spaces/' + str(uuid.UUID(space['id'])) + '/archive', {})
            assert code in (200, 202), 'controlled space cleanup failed'


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except Exception as exc:
        REPORT['errorClass'] = type(exc).__name__
        REPORT['result'] = 'FAIL'
    (OUT / 'report.json').write_text(json.dumps(REPORT))
    raise SystemExit(0 if REPORT['result'] == 'PASS' else 1)
