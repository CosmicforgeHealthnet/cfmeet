"""Controlled production media infrastructure probe; not Calling user acceptance.

Only joins its own unique room, publishes a synthetic tone, captures it and removes
its own staging objects. No application principal or consent checks are bypassed:
this probe never claims to certify the Calling application's authorization path.
"""
import asyncio
import base64
import datetime
import hashlib
import hmac
import json
import math
from pathlib import Path
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from livekit import rtc

S = Path('/run/secrets')
OUT = Path('/result')
BUCKET = 'cfmeet-calling-artifacts'
ROOM = 'cfmeet-production-probe-'+uuid.uuid4().hex
RESULT = {'room':ROOM, 'scope':'infrastructure_media_only', 'checks':{}}


def read(name):
    return (S/name).read_text().strip()


def token(identity, grants):
    enc = lambda b: base64.urlsafe_b64encode(b).decode().rstrip('=')
    now = int(time.time())
    body = dict(iss=read('calling-livekit-api-key'),sub=identity,nbf=now-2,iat=now,exp=now+300,video=grants)
    t = enc(b'{"alg":"HS256","typ":"JWT"}')+'.'+enc(json.dumps(body).encode())
    return t+'.'+enc(hmac.new(read('calling-livekit-api-secret').encode(),t.encode(),hashlib.sha256).digest())


def rpc(service, method, body, grants):
    req = urllib.request.Request('https://api.cfmeet.com:8446/twirp/livekit.'+service+'/'+method,
        data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+token('production-probe',grants),'Content-Type':'application/json'})
    with urllib.request.urlopen(req,timeout=45) as r:
        return json.load(r)


def s3(method, key, data=b'', egress=False, encryption=False, public=False):
    access = read('minio-egress-access-key' if egress else 'minio-service-access-key')
    password = read('minio-egress-secret' if egress else 'calling-artifact-store-secret')
    host = 'api.cfmeet.com' if public else 'cfmeet-calling-artifacts:9000'
    endpoint = ('https://' if public else 'http://')+host
    path = '/'+BUCKET+'/'+urllib.parse.quote(key,safe='/')
    now = datetime.datetime.now(datetime.timezone.utc)
    date, stamp = now.strftime('%Y%m%d'), now.strftime('%Y%m%dT%H%M%SZ')
    digest = hashlib.sha256(data).hexdigest()
    headers = {'host':host,'x-amz-content-sha256':digest,'x-amz-date':stamp}
    if encryption:
        headers['x-amz-server-side-encryption'] = 'AES256'
    signed = ';'.join(sorted(headers))
    canonical = method+'\n'+path+'\n\n'+''.join(k+':'+headers[k]+'\n' for k in sorted(headers))+'\n'+signed+'\n'+digest
    scope = date+'/us-east-1/s3/aws4_request'
    text = 'AWS4-HMAC-SHA256\n'+stamp+'\n'+scope+'\n'+hashlib.sha256(canonical.encode()).hexdigest()
    signing = ('AWS4'+password).encode()
    for part in (date,'us-east-1','s3','aws4_request'):
        signing = hmac.new(signing,part.encode(),hashlib.sha256).digest()
    headers['Authorization'] = 'AWS4-HMAC-SHA256 Credential='+access+'/'+scope+', SignedHeaders='+signed+', Signature='+hmac.new(signing,text.encode(),hashlib.sha256).hexdigest()
    req = urllib.request.Request(endpoint+path,data=data if method=='PUT' else None,method=method,headers=headers)
    try:
        with urllib.request.urlopen(req,timeout=20) as r:
            return r.status,r.read(),dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code,e.read(),dict(e.headers)


async def main():
    rooms = [rtc.Room(),rtc.Room()]
    tasks, egress_ids, objects = [], [], []
    try:
        for i, room in enumerate(rooms):
            await asyncio.wait_for(room.connect('wss://api.cfmeet.com:8446',token('probe-'+str(i),
                dict(roomJoin=True,room=ROOM,canPublish=True,canSubscribe=True))),30)
        source = rtc.AudioSource(48000,1)
        track = rtc.LocalAudioTrack.create_audio_track('synthetic-440hz',source)
        options = rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE)
        publication = await asyncio.wait_for(rooms[0].local_participant.publish_track(track,options),15)
        async def feed():
            n = 0
            while True:
                frame = rtc.AudioFrame.create(48000,1,480)
                for i in range(480):
                    frame.data[i] = int(6000*math.sin(2*math.pi*440*(n*480+i)/48000))
                await source.capture_frame(frame)
                n += 1
                await asyncio.sleep(.01)
        tasks.append(asyncio.create_task(feed()))
        await asyncio.sleep(2)
        assert any(p.track for p in rooms[1].remote_participants.values() for p in p.track_publications.values()), 'Remote audio subscription missing'
        RESULT['checks']['two_client_audio_subscription'] = 'PASS'
        for kind in ('track','composite'):
            key = 'capture-staging/'+ROOM+'-'+kind+'.ogg'
            objects.append(key)
            if kind == 'track':
                method,body = 'StartTrackEgress',dict(room_name=ROOM,track_id=publication.sid,file=dict(filepath=key,disable_manifest=True))
            else:
                method,body = 'StartRoomCompositeEgress',dict(room_name=ROOM,audio_only=True,
                    file_outputs=[dict(file_type='OGG',filepath=key,disable_manifest=True)])
            info = await asyncio.to_thread(rpc,'Egress',method,body,{'roomRecord':True})
            eid = info.get('egress_id',info.get('egressId'))
            assert eid
            egress_ids.append(eid)
            await asyncio.sleep(10)
            await asyncio.to_thread(rpc,'Egress','StopEgress',{'egress_id':eid},{'roomRecord':True})
            for _ in range(60):
                response = await asyncio.to_thread(rpc,'Egress','ListEgress',{'egress_id':eid},{'roomRecord':True})
                item = next(x for x in response.get('items',[]) if x.get('egress_id',x.get('egressId'))==eid)
                if item.get('status') in ('EGRESS_COMPLETE',3):
                    break
                assert item.get('status') not in ('EGRESS_FAILED','EGRESS_ABORTED',4,5), 'Egress capture failed'
                await asyncio.sleep(2)
            else:
                raise RuntimeError('Egress finalization timed out')
            code,audio,_ = await asyncio.to_thread(s3,'GET',key)
            assert code==200 and len(audio)>5000 and audio.startswith(b'OggS') and b'OpusHead' in audio[:256]
            (OUT/(kind+'.ogg')).write_bytes(audio)
            RESULT['checks'][kind+'_egress_audio_object'] = 'PASS'
        outside = 'production-probe-denied/'+ROOM
        try:
            code,_,_ = await asyncio.to_thread(s3,'PUT',outside,b'denied',True)
            assert code==403, 'Egress permission escaped staging prefix'
            RESULT['checks']['egress_outside_prefix_denied'] = 'PASS'
        finally:
            await asyncio.to_thread(s3,'DELETE',outside)
        code,_,_ = await asyncio.to_thread(s3,'GET',objects[0],b'',True)
        assert code==403
        RESULT['checks']['egress_read_denied'] = 'PASS'
        code,body,headers = await asyncio.to_thread(s3,'PUT','capture-staging/'+ROOM+'-encrypted',b'controlled encryption probe',False,True)
        objects.append('capture-staging/'+ROOM+'-encrypted')
        RESULT['checks']['encryption_at_rest'] = 'PASS' if code==200 and headers.get('X-Amz-Server-Side-Encryption',headers.get('x-amz-server-side-encryption'))=='AES256' else 'FAIL'
        # Record only stable server error code; never raw URLs, signatures or credentials.
        if code!=200:
            import re
            error = re.search(b'<Code>([^<]+)</Code>',body)
            RESULT['encryption_error_code'] = error[1].decode() if error else str(code)
        code,_,_ = await asyncio.to_thread(s3,'GET',objects[0],b'',False,False,True)
        RESULT['checks']['signed_https_storage_path'] = 'PASS' if code==200 else 'FAIL'
    except Exception as exc:
        RESULT['error_type'] = type(exc).__name__
        raise
    finally:
        for eid in egress_ids:
            try:
                await asyncio.to_thread(rpc,'Egress','StopEgress',{'egress_id':eid},{'roomRecord':True})
            except Exception:
                pass
        for task in tasks:
            task.cancel()
        for room in rooms:
            await room.disconnect()
        try:
            await asyncio.to_thread(rpc,'RoomService','DeleteRoom',{'room':ROOM},{'roomCreate':True})
        except Exception:
            pass
        cleanup = True
        for key in objects:
            try:
                code,_,_ = await asyncio.to_thread(s3,'DELETE',key)
                get_code,_,_ = await asyncio.to_thread(s3,'GET',key)
                cleanup = cleanup and code in (200,204) and get_code==404
            except Exception:
                cleanup = False
        RESULT['checks']['controlled_staging_objects_deleted'] = 'PASS' if cleanup else 'FAIL'
        (OUT/'media-result.json').write_text(json.dumps(RESULT,indent=2))


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except Exception as exc:
        print('MEDIA_PROBE_FAILED:',type(exc).__name__)
        raise SystemExit(1)
