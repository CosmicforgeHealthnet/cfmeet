import asyncio
import json
import math
from pathlib import Path
import time
import urllib.error
import urllib.request
import uuid
import sys
from livekit import rtc
from websockets.asyncio.client import connect

CTX=json.loads(Path('/result/context.private.json').read_text())
TOKEN=CTX['actors']['token']; PEOPLE=CTX['actors']['principals']
REPORT={'scope':'production-Calling-API-isolated-authorized-principals','checks':{},'phase':'devices'}
CALLS=[]; DEVICES=[]; SOCKETS=[]; ROOMS=[]; TASKS=[]
TERMINAL=('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED')


class MediaClient:
    def __init__(self,process):self.process=process;self.closed=False

    @classmethod
    async def start(cls):
        log=open('/result/rtc-'+uuid.uuid4().hex+'.private.log','wb')
        process=await asyncio.create_subprocess_exec(sys.executable,'/rtc_client.py',stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,stderr=log)
        log.close();return cls(process)

    async def cmd(self,command,**body):
        self.process.stdin.write((json.dumps({'cmd':command,**body})+'\n').encode());await self.process.stdin.drain()
        line=await asyncio.wait_for(self.process.stdout.readline(),35)
        assert line,'RTC client stopped unexpectedly'
        return json.loads(line)

    async def disconnect(self):
        if self.closed:return
        try:await self.cmd('quit')
        finally:
            try:await asyncio.wait_for(self.process.wait(),8)
            except asyncio.TimeoutError:self.process.kill();await self.process.wait()
            self.closed=True


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
    REPORT['join_index']=index
    room=await MediaClient.start();ROOMS.append(room)
    room.url=credential['url']
    assert (await room.cmd('connect',url=credential['url'],token=credential['token'])).get('ok'), 'Real client media join failed'
    return room,session['sessionId']


async def publish(room,video=False):
    # Wait for Calling's server-side admission, never inject provider events or grants.
    assert (await room.cmd('wait_permission',publish=True,timeout=15)).get('ok'), 'Calling did not admit media permissions after real join'
    assert (await room.cmd('publish',kind='audio')).get('ok'), 'Authorized microphone publishing failed'
    if video:
        assert (await room.cmd('publish',kind='video')).get('ok'), 'Authorized camera publishing failed'


async def received_media(room,video=False):
    assert (await room.cmd('verify_received',kind='video' if video else 'audio',timeout=12)).get('ok'), 'Remote playable media not received'


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
    await received_media(a);await received_media(b)
    if kind=='VIDEO':await received_media(a,True);await received_media(b,True)
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
        if len(sys.argv)>1 and sys.argv[1] in ('recording','voicemail'):
            import capture_scenarios
            await getattr(capture_scenarios,sys.argv[1])(sys.modules[__name__])
            return
        for kind in ('VOICE','VIDEO'):
            REPORT['phase']=kind.lower()
            call=await dial(kind);a,b=await connected(call,kind)
            REPORT['checks'][kind.lower()+'_real_media']='PASS'
            if kind=='VOICE':
                await command(call,'hold');assert (await call_view(call))['state']=='HELD'
                assert (await a.cmd('wait_permission',publish=False,timeout=10)).get('ok'), 'Hold did not revoke microphone publishing'
                assert (await b.cmd('wait_permission',publish=False,timeout=10)).get('ok'), 'Hold did not revoke recipient publishing'
                for _ in range(30):
                    view=await call_view(call)
                    if sum(s['state']=='CONNECTED' for s in view['sessions'])>=2:break
                    await asyncio.sleep(.5)
                else:raise AssertionError('Held media did not settle with two connected sessions')
                for attempt in range(20):
                    view=await call_view(call)
                    resumed=await request('POST','/v1/calls/'+call+'/resume',{'expectedVersion':view['version']},expected=(200,422))
                    if resumed.get('error',{}).get('code')=='CALL_EVIDENCE_REQUIRED':
                        await asyncio.sleep(.5);continue
                    assert 'error' not in resumed,'Resume was refused'
                    break
                assert (await call_view(call))['state']=='CONNECTED'
                REPORT['resume_evidence_retries']=attempt
                REPORT['checks']['hold_resume']='PASS'
                assert (await a.cmd('wait_permission',publish=True,timeout=15)).get('ok')
                refreshed=await b.cmd('wait_refreshed',publish=True,timeout=30)
                assert refreshed.get('ok'), 'No current server-refreshed reconnect token'
                gateway=b.url
                await b.disconnect();await asyncio.sleep(2)
                b=await MediaClient.start();ROOMS.append(b)
                assert (await b.cmd('connect',url=gateway,token=refreshed['token'])).get('ok'), 'Current session reconnect refused'
                await publish(b);await received_media(a);await received_media(b)
                REPORT['checks']['real_media_reconnection']='PASS'
                assert not (await a.cmd('publish',kind='video')).get('ok'), 'Voice call admitted an unauthorized camera track'
                REPORT['checks']['voice_camera_publish_denied']='PASS'
            await finish(call)
            await a.disconnect();await b.disconnect()
            REPORT['checks'][kind.lower()+'_end']='PASS'
        REPORT['phase']='invitations_cancel_decline'
        call=await dial();view=await call_view(call)
        invitation=next(x for x in view['invitations'] if x['endpointId']==DEVICES[1]['endpointId'] and x['status']=='RINGING')
        await request('POST','/v1/calling/invitations/'+invitation['invitationId']+'/decline',{'expectedVersion':view['version']},token=DEVICES[1]['token'])
        assert (await call_view(call))['state']=='DECLINED';REPORT['checks']['incoming_decline']='PASS'
        call=await dial();await command(call,'cancel');assert (await call_view(call))['state']=='CANCELLED'
        REPORT['checks']['cancel']='PASS'
        REPORT['phase']='missed_call_history';call=await dial()
        for _ in range(100):
            view=await call_view(call)
            if view['state'] in TERMINAL:break
            await asyncio.sleep(1)
        assert view['state']=='NO_ANSWER', 'Unanswered call did not terminate as NO_ANSWER'
        history=await request('GET','/v1/calls?principalId='+PEOPLE[1]+'&missed=true&limit=100')
        entry=next(x for x in history['data'] if x['callId']==call)
        assert entry['participation']['missed'] and entry['participation']['direction']=='INBOUND'
        REPORT['checks']['missed_calls_and_principal_history']='PASS'
        REPORT['phase']='group_call'
        call=await dial(targets=PEOPLE[1:]);await answer(call,1);await answer(call,2)
        group_rooms=[]
        for index in range(3):
            room,_=await join(call,index);group_rooms.append(room)
        for room in group_rooms:await publish(room)
        for room in group_rooms:await received_media(room)
        assert len((await call_view(call))['participants'])==3
        REPORT['checks']['group_real_media']='PASS';await finish(call)
        for room in group_rooms:await room.disconnect()
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
        await request('POST','/v1/enterprise/extensions',dict(extensionNumber=str(number+3),displayLabel='Acceptance transfer',destination={'type':'PRINCIPAL','id':PEOPLE[2]}))
        flow=await request('POST','/v1/enterprise/ivr-flows',{'name':'Acceptance IVR '+suffix})
        graph={'startNode':'menu','nodes':[
            {'id':'menu','kind':'MENU','prompt':{'builtin':'MENU'},'inputTimeoutSeconds':20,'maxRetries':1,
             'options':[{'digit':'1','next':'route'}],'onInvalid':'bye','onTimeout':'bye'},
            {'id':'route','kind':'ROUTE_EXTENSION','extensionId':extension['extensionId']},
            {'id':'bye','kind':'HANGUP','prompt':{'builtin':'GOODBYE'}}]}
        await request('POST','/v1/enterprise/ivr-flows/'+flow['ivrFlowId']+'/versions',{'graph':graph})
        flow=await request('GET','/v1/enterprise/ivr-flows/'+flow['ivrFlowId'])
        await request('PUT','/v1/enterprise/ivr-flows/'+flow['ivrFlowId']+'/activation',{'expectedVersion':flow['version'],'versionNumber':1})
        await request('POST','/v1/enterprise/extensions',dict(extensionNumber=str(number+4),displayLabel='Acceptance IVR route',destination={'type':'IVR','id':flow['ivrFlowId']}))
        REPORT['phase']='ivr_routing';call=await dial(extension=str(number+4));await asyncio.sleep(3)
        await request('POST','/v1/calls/'+call+'/ivr/input',{'endpointId':DEVICES[0]['endpointId'],'digit':'1'})
        a,b=await connected(call);REPORT['checks']['ivr_keypad_to_extension_real_media']='PASS'
        await finish(call);await a.disconnect();await b.disconnect()
        REPORT['phase']='internal_transfer';call=await dial(extension=str(number));a,b=await connected(call)
        view=await call_view(call);participant=next(x['participantId'] for x in view['participants'] if x['principalId']==PEOPLE[1])
        await command(call,'transfer',{'transferringParticipantId':participant,'targetExtension':str(number+3)})
        await answer(call,2);c,_=await join(call,2);await publish(c);await received_media(c)
        for _ in range(30):
            if (await call_view(call))['state']=='CONNECTED':break
            await asyncio.sleep(.5)
        else:raise AssertionError('Transfer did not return to CONNECTED')
        REPORT['checks']['internal_transfer_real_media']='PASS';await finish(call)
        for room in (a,b,c):await room.disconnect()
        for offset,label in [(0,'extensions'),(1,'ring_groups'),(2,'queues')]:
            REPORT['phase']=label;call=await dial(extension=str(number+offset));a,b=await connected(call)
            REPORT['checks'][label]='PASS';await finish(call);await a.disconnect();await b.disconnect()
        await request('GET','/v1/enterprise/extensions/'+extension['extensionId'],token=CTX['other']['token'],expected=(404,))
        REPORT['checks']['enterprise_tenant_isolation']='PASS'
    except Exception as error:
        REPORT['failure_request']=REPORT.get('last_request')
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
