import asyncio
import json
import math
from pathlib import Path
import time
import urllib.error
import urllib.request
import uuid
from livekit import rtc
from websockets.asyncio.client import connect

CTX=json.loads(Path('/result/context.private.json').read_text())
TOKEN=CTX['actors']['token']; PEOPLE=CTX['actors']['principals']
REPORT={'scope':'production-Calling-API-isolated-authorized-principals','checks':{},'phase':'devices'}
CALLS=[]; DEVICES=[]; SOCKETS=[]; ROOMS=[]; TASKS=[]
TERMINAL=('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED')


async def request(method,path,body=None,token=None,expected=(200,201)):
    REPORT['last_request']={'method':method,'path':path}
    def send():
        req=urllib.request.Request('https://api.cfmeet.com'+path,method=method,
            data=json.dumps(body).encode() if body is not None else None,
            headers={'Authorization':'Bearer '+(token or TOKEN),'Content-Type':'application/json','Idempotency-Key':str(uuid.uuid4())})
        try:
            with urllib.request.urlopen(req,timeout=25) as response: return response.status,json.load(response)
        except urllib.error.HTTPError as error:
            try: value=json.loads(error.read())
            except Exception:value={}
            return error.code,value
    status,value=await asyncio.to_thread(send)
    if status not in expected:
        REPORT['failed_request']={'method':method,'path':path,'status':status,'code':value.get('error',{}).get('code')}
        raise AssertionError('Calling API request failed')
    return value


async def call_view(call):return await request('GET','/v1/calls/'+call)


async def command(call,verb,extra=None):
    view=await call_view(call)
    return await request('POST','/v1/calls/'+call+'/'+verb,{'expectedVersion':view['version'],**(extra or {})})


async def dial(kind='VOICE',targets=None,extension=None):
    body=dict(kind=kind,initiatorPrincipalId=PEOPLE[0],initiatorEndpointId=DEVICES[0]['endpointId'])
    body.update({'targetExtension':extension} if extension else {'targetPrincipalIds':targets or [PEOPLE[1]]})
    value=await request('POST','/v1/calls',body);call=value['call']['callId'];CALLS.append(call);return call


async def finish(call):
    await command(call,'end')
    for _ in range(35):
        if (await call_view(call))['state'] in TERMINAL:return
        await asyncio.sleep(.5)
    raise AssertionError('Call cleanup did not reach a terminal state')


async def answer(call,index=1):
    for _ in range(25):
        invitations=await request('GET','/v1/calling/invitations',token=DEVICES[index]['token'])
        invitation=next((x for x in invitations['data'] if x['callId']==call and x['actionable']),None)
        if invitation:break
        await asyncio.sleep(.5)
    else:raise AssertionError('Incoming invitation missing')
    return await request('POST','/v1/calling/invitations/'+invitation['invitationId']+'/accept',
        {'expectedVersion':invitation['callVersion']},token=DEVICES[index]['token'])


async def join(call,index,video=False):
    view=await call_view(call)
    session=next(s for s in view['sessions'] if s['endpointId']==DEVICES[index]['endpointId'] and s['state']!='ENDED')
    credential=await request('POST','/v1/calling/sessions/'+session['sessionId']+'/media-credentials',{},token=DEVICES[index]['token'])
    room=rtc.Room();ROOMS.append(room)
    await asyncio.wait_for(room.connect(credential['url'],credential['token']),25)
    return room,session['sessionId']


async def publish(room,video=False):
    # Wait for Calling's server-side admission, never inject provider events or grants.
    for _ in range(40):
        if room.local_participant.permissions.can_publish:break
        await asyncio.sleep(.5)
    else:raise AssertionError('Calling did not admit media permissions after real join')
    source=rtc.AudioSource(48000,1);track=rtc.LocalAudioTrack.create_audio_track('controlled-tone',source)
    await asyncio.wait_for(room.local_participant.publish_track(track,rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE)),12)
    async def feed():
        n=0
        while True:
            frame=rtc.AudioFrame.create(48000,1,480)
            for i in range(480):frame.data[i]=int(6000*math.sin(2*math.pi*440*(n*480+i)/48000))
            await source.capture_frame(frame);n+=1;await asyncio.sleep(.01)
    TASKS.append(asyncio.create_task(feed()))
    if video:
        source_v=rtc.VideoSource(64,64)
        track_v=rtc.LocalVideoTrack.create_video_track('controlled-video',source_v)
        await asyncio.wait_for(room.local_participant.publish_track(track_v,rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_CAMERA)),12)
        async def frames():
            while True:
                source_v.capture_frame(rtc.VideoFrame(64,64,rtc.VideoBufferType.RGBA,bytearray([0,80,160,255])*4096));await asyncio.sleep(.1)
        TASKS.append(asyncio.create_task(frames()))


async def connected(call,kind='VOICE',target=None):
    REPORT['step']='answer'
    await answer(call,1)
    REPORT['step']='caller_media_join';a,_=await join(call,0,kind=='VIDEO')
    REPORT['step']='recipient_media_join';b,_=await join(call,1,kind=='VIDEO')
    REPORT['step']='publish'
    await publish(a,kind=='VIDEO');await publish(b,kind=='VIDEO')
    for _ in range(30):
        if (await call_view(call))['state']=='CONNECTED':break
        await asyncio.sleep(.5)
    else:raise AssertionError('Real media call did not become CONNECTED')
    await asyncio.sleep(2)
    assert any(p.track for r in (a,b) for p0 in r.remote_participants.values() for p in p0.track_publications.values())
    return a,b


async def main():
    try:
        for principal in PEOPLE:
            device=await request('POST','/v1/calling/endpoints',dict(principalId=principal,endpointKey='production-api-'+uuid.uuid4().hex,
                endpointKind='WEB',capabilities={'audio':True,'video':True}))
            token=await request('POST','/v1/calling/signaling-credentials',dict(principalId=principal,endpointId=device['endpointId']))
            device['token']=token['token'];DEVICES.append(device)
            ws=await connect('wss://api.cfmeet.com/v1/calling/signaling',subprotocols=['cfmeet.signaling.v1','bearer.'+token['token']],open_timeout=15)
            assert json.loads(await asyncio.wait_for(ws.recv(),10))['type']=='signaling.ready';SOCKETS.append(ws)
            async def drain(socket):
                async for _ in socket:pass
            TASKS.append(asyncio.create_task(drain(ws)))
            async def heartbeat(socket):
                while True:
                    await asyncio.sleep(20);await socket.send(json.dumps({'type':'ping'}))
            TASKS.append(asyncio.create_task(heartbeat(ws)))
        REPORT['checks']['authorized_endpoint_signaling']='PASS'
        for kind in ('VOICE','VIDEO'):
            REPORT['phase']=kind.lower()
            call=await dial(kind);a,b=await connected(call,kind)
            REPORT['checks'][kind.lower()+'_real_media']='PASS'
            if kind=='VOICE':
                await command(call,'hold');assert (await call_view(call))['state']=='HELD'
                await command(call,'resume');assert (await call_view(call))['state']=='CONNECTED'
                REPORT['checks']['hold_resume']='PASS'
            await finish(call)
            await a.disconnect();await b.disconnect()
            REPORT['checks'][kind.lower()+'_end']='PASS'
        REPORT['phase']='enterprise_configuration'
        number=100000+int(uuid.uuid4().hex[:8],16)%800000
        suffix=''.join(chr(97+int(x,16)) for x in uuid.uuid4().hex[:12])
        extension=await request('POST','/v1/enterprise/extensions',dict(extensionNumber=str(number),displayLabel='Acceptance alpha',destination={'type':'PRINCIPAL','id':PEOPLE[1]}))
        group=await request('POST','/v1/enterprise/ring-groups',dict(name='Acceptance ring group '+suffix,strategy='SIMULTANEOUS',memberRingSeconds=10,groupTimeoutSeconds=20))
        await request('PUT','/v1/enterprise/ring-groups/'+group['ringGroupId']+'/members',
            {'expectedVersion':group['version'],'members':[{'type':'PRINCIPAL','id':PEOPLE[1]},{'type':'PRINCIPAL','id':PEOPLE[2]}]})
        queue=await request('POST','/v1/enterprise/queues',dict(name='Acceptance queue '+suffix,maxWaitSeconds=60,offerTimeoutSeconds=20,capacity=5))
        await request('PUT','/v1/enterprise/queues/'+queue['queueId']+'/members',{'expectedVersion':queue['version'],'members':[{'principalId':PEOPLE[1]}]})
        for offset,kind,id in [(1,'RING_GROUP',group['ringGroupId']),(2,'QUEUE',queue['queueId'])]:
            await request('POST','/v1/enterprise/extensions',dict(extensionNumber=str(number+offset),displayLabel='Acceptance route',destination={'type':kind,'id':id}))
        for offset,label in [(0,'extensions'),(1,'ring_groups'),(2,'queues')]:
            REPORT['phase']=label;call=await dial(extension=str(number+offset));a,b=await connected(call)
            REPORT['checks'][label]='PASS';await finish(call);await a.disconnect();await b.disconnect()
        await request('GET','/v1/enterprise/extensions/'+extension['extensionId'],token=CTX['other']['token'],expected=(404,))
        REPORT['checks']['enterprise_tenant_isolation']='PASS'
    except Exception as error:
        REPORT['error_type']=type(error).__name__
        REPORT['failure_message']=str(error)[:200] if isinstance(error,AssertionError) else ''
        raise
    finally:
        for call in CALLS:
            try:
                view=await call_view(call)
                if view['state'] not in TERMINAL:
                    await command(call,'end' if view['state'] in ('CONNECTED','HELD','ANSWERED','TRANSFERRING') else 'cancel')
            except Exception:pass
        for room in ROOMS:
            try:await room.disconnect()
            except Exception:pass
        for task in TASKS:task.cancel()
        for socket in SOCKETS:await socket.close()
        for device in DEVICES:
            try:await request('DELETE','/v1/calling/endpoints/'+device['endpointId'])
            except Exception:pass
        REPORT['timestamp']=int(time.time());Path('/result/api-result.json').write_text(json.dumps(REPORT,indent=2))


if __name__=='__main__':
    try:asyncio.run(main())
    except Exception:raise SystemExit(1)
