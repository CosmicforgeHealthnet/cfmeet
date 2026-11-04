"""Real capture through Calling decisions, production Egress and encrypted storage."""
import asyncio
import array
import json
from datetime import datetime
from pathlib import Path
import subprocess
import urllib.request
import urllib.error
import uuid


async def download_and_decode(api,access,name):
    assert access['url'].startswith('https://api.cfmeet.com/') and 'capture-staging' not in access['url']
    def download():
        with urllib.request.urlopen(access['url'],timeout=20) as response:return response.read()
    audio=await asyncio.to_thread(download)
    assert audio.startswith(b'OggS') and len(audio)>5000
    decode=await asyncio.to_thread(subprocess.run,['ffmpeg','-v','error','-i','pipe:0','-ac','1','-ar','48000','-f','s16le','pipe:1'],input=audio,capture_output=True)
    assert decode.returncode==0
    pcm=array.array('h',decode.stdout)
    assert len(pcm)>=3*48000 and sum(int(x)**2 for x in pcm)/len(pcm)>100**2


async def deleted_object(access):
    def status():
        try:
            with urllib.request.urlopen(access['url'],timeout=15) as response:return response.status
        except urllib.error.HTTPError as error:return error.code
    for _ in range(60):
        if await asyncio.to_thread(status) in (403,404):return
        await asyncio.sleep(1)
    raise AssertionError('Deleted media remained readable through its original signed URL')


async def retention_expiry(a,kind,call,path,view,principal):
    access=await a.request('POST',path+'/media-access',{'principalId':principal})
    await download_and_decode(a,access,kind+'-retention')
    # Ask the host controller to advance only this isolated fixture's deadline.
    # This tests the real worker, without waiting a day or changing a global policy.
    pending=Path('/result/retention-request.tmp')
    pending.write_text(json.dumps({'kind':kind,'artifactId':view.get('recordingId',view.get('messageId')),'callId':call}))
    pending.replace('/result/retention-request.json')
    for _ in range(90):
        current=await a.request('GET',path+'?principalId='+principal)
        if current['state']=='DELETED':break
        await asyncio.sleep(1)
    else:raise AssertionError('Production retention worker did not delete the expired fixture')
    await a.request('POST',path+'/media-access',{'principalId':principal},expected=(404,409))
    await deleted_object(access)
    a.REPORT['checks'][kind+'_retention_worker_and_object_deletion']='PASS'


async def recording(a):
    a.REPORT['phase']='recording_policy'
    suffix=''.join(chr(97+int(x,16)) for x in uuid.uuid4().hex[:10])
    await a.request('POST','/v1/enterprise/recording-policies',dict(name='Acceptance recording '+suffix,mode='MANUAL_ALLOWED',
        noticeRequired=True,consentParticipants='ALL_PARTICIPANTS',jurisdictionRef='controlled-production-acceptance',
        retentionDays=1,maxRecordingSeconds=60,isDefault=True))
    number=str(100000+int(uuid.uuid4().hex[:8],16)%800000)
    await a.request('POST','/v1/enterprise/extensions',{'extensionNumber':number,'displayLabel':'Acceptance recording route',
        'destination':{'type':'PRINCIPAL','id':a.PEOPLE[1]}})
    call=await a.dial(extension=number);rooms=await a.connected(call)
    a.REPORT['phase']='recording_consent'
    await a.request('POST','/v1/calls/'+call+'/recordings',{'principalId':a.PEOPLE[0]},expected=(403,))
    a.REPORT['checks']['recording_without_consent_denied']='PASS'
    decisions=await a.request('GET','/v1/calls/'+call+'/recording-authorizations')
    assert decisions['authorizations'][-1]['reason']=='CONSENT_MISSING'
    for i in (0,1):
        await a.request('POST','/v1/calls/'+call+'/recording-consent',{'endpointId':a.DEVICES[i]['endpointId'],'decision':'GRANTED'})
    await a.request('POST','/v1/calls/'+call+'/recordings',{'principalId':a.PEOPLE[2]},expected=(403,404))
    a.REPORT['checks']['recording_nonparticipant_denied']='PASS'
    result=await a.request('POST','/v1/calls/'+call+'/recordings',{'principalId':a.PEOPLE[0]})
    recording=result['recording']['recordingId']
    decisions=await a.request('GET','/v1/calls/'+call+'/recording-authorizations')
    allowed=decisions['authorizations'][-1]
    assert allowed['decision']=='ALLOWED' and allowed['notice']['required']
    assert len(allowed['notice']['deliveries'])==2
    assert all(p['decision']=='GRANTED' for p in allowed['consent']['participants'])
    a.REPORT['checks']['server_notice_and_participant_consent_evidence']='PASS'
    a.REPORT['phase']='recording_real_capture';await asyncio.sleep(10)
    await a.request('POST','/v1/calls/'+call+'/recordings/'+recording+'/stop',{'principalId':a.PEOPLE[0]})
    path='/v1/enterprise/recordings/'+recording
    for _ in range(90):
        view=await a.request('GET',path+'?principalId='+a.PEOPLE[0])
        if view['state']=='READY':break
        assert view['state'] not in ('FAILED','DELETED'), 'Recording finalization failed'
        await asyncio.sleep(1)
    else:raise AssertionError('Recording did not become READY')
    assert datetime.fromisoformat(view['retentionUntil'].replace('Z','+00:00'))>datetime.fromisoformat(view['readyAt'].replace('Z','+00:00'))
    a.REPORT['checks']['recording_retention_deadline']='PASS'
    access=await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[0]})
    await download_and_decode(a,access,'recording');a.REPORT['checks']['recording_encrypted_real_media_playback']='PASS'
    await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[2]},expected=(404,))
    await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[0]},token=a.CTX['other']['token'],expected=(404,))
    a.REPORT['checks']['recording_cross_user_and_tenant_denied']='PASS'
    audit=await a.request('GET',path+'/access-audit');assert audit['records']
    a.REPORT['checks']['recording_access_audited']='PASS'
    await a.request('DELETE',path+'?expectedVersion='+str(view['version'])+'&principalId='+a.PEOPLE[0])
    await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[0]},expected=(404,409))
    a.REPORT['checks']['recording_delete_revokes_access']='PASS'
    await deleted_object(access);a.REPORT['checks']['recording_physical_object_deleted']='PASS'
    a.REPORT['phase']='recording_retention_worker'
    result=await a.request('POST','/v1/calls/'+call+'/recordings',{'principalId':a.PEOPLE[0]})
    retained=result['recording']['recordingId'];await asyncio.sleep(10)
    await a.request('POST','/v1/calls/'+call+'/recordings/'+retained+'/stop',{'principalId':a.PEOPLE[0]})
    retained_path='/v1/enterprise/recordings/'+retained
    for _ in range(90):
        retained_view=await a.request('GET',retained_path+'?principalId='+a.PEOPLE[0])
        if retained_view['state']=='READY':break
        assert retained_view['state'] not in ('FAILED','DELETED')
        await asyncio.sleep(1)
    else:raise AssertionError('Retention recording not READY')
    await retention_expiry(a,'recording',call,retained_path,retained_view,a.PEOPLE[0])
    await a.finish(call)
    for room in rooms:await room.disconnect()


async def voicemail(a):
    a.REPORT['phase']='voicemail_mailbox'
    mailboxes=await a.request('GET','/v1/enterprise/voicemail/mailboxes?limit=100')
    mailbox=next((m for m in mailboxes['mailboxes'] if m['owner']=={'type':'PRINCIPAL','id':a.PEOPLE[1]} and m['status']=='ACTIVE'),None)
    if mailbox is None:
        mailbox=await a.request('POST','/v1/enterprise/voicemail/mailboxes',{'owner':{'type':'PRINCIPAL','id':a.PEOPLE[1]},
            'displayLabel':'Acceptance voicemail','maxMessageSeconds':30,'retentionDays':1})
    rules=await a.request('GET','/v1/enterprise/forwarding-rules?limit=100')
    rule=next((r for r in rules['forwardingRules'] if r['principalId']==a.PEOPLE[1] and r['condition']=='NO_ANSWER' and r['status']=='ACTIVE'),None)
    if rule is None:
        await a.request('POST','/v1/enterprise/forwarding-rules',{'principalId':a.PEOPLE[1],'condition':'NO_ANSWER',
            'destination':{'type':'VOICEMAIL','id':mailbox['mailboxId']}})
    else:assert rule['destination']=={'type':'VOICEMAIL','id':mailbox['mailboxId']}
    number=str(100000+int(uuid.uuid4().hex[:8],16)%800000)
    await a.request('POST','/v1/enterprise/extensions',{'extensionNumber':number,'displayLabel':'Acceptance voicemail route',
        'destination':{'type':'PRINCIPAL','id':a.PEOPLE[1]}})
    call=await a.dial(extension=number)
    room,_=await a.join(call,0);await a.publish(room)
    a.REPORT['phase']='voicemail_unanswered_to_capture'
    message=None
    for _ in range(100):
        values=await a.request('GET','/v1/enterprise/voicemail/messages?mailboxId='+mailbox['mailboxId']+'&principalId='+a.PEOPLE[1])
        message=next((m for m in values['messages'] if m['callId']==call),None)
        if message:break
        await asyncio.sleep(1)
    assert message, 'Unanswered call never entered voicemail'
    await asyncio.sleep(10)
    view=await a.call_view(call)
    if view['state'] not in a.TERMINAL:await a.finish(call)
    path='/v1/enterprise/voicemail/messages/'+message['messageId']
    for _ in range(90):
        view=await a.request('GET',path+'?principalId='+a.PEOPLE[1])
        if view['state']=='READY':break
        assert view['state'] not in ('FAILED','DELETED'), 'Voicemail finalization failed'
        await asyncio.sleep(1)
    else:raise AssertionError('Voicemail did not become READY')
    assert datetime.fromisoformat(view['retentionUntil'].replace('Z','+00:00'))>datetime.fromisoformat(view['readyAt'].replace('Z','+00:00'))
    a.REPORT['checks']['voicemail_retention_deadline']='PASS'
    access=await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[1]})
    await download_and_decode(a,access,'voicemail');a.REPORT['checks']['voicemail_real_audio_lifecycle']='PASS'
    await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[0]},expected=(404,))
    await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[1]},token=a.CTX['other']['token'],expected=(404,))
    a.REPORT['checks']['voicemail_owner_and_tenant_isolation']='PASS'
    marked=await a.request('PUT',path+'/read-state',{'principalId':a.PEOPLE[1],'expectedVersion':view['version'],'readState':'READ'})
    await a.request('DELETE',path+'?expectedVersion='+str(marked['version'])+'&principalId='+a.PEOPLE[1])
    await a.request('POST',path+'/media-access',{'principalId':a.PEOPLE[1]},expected=(404,409))
    a.REPORT['checks']['voicemail_read_delete_access_revocation']='PASS'
    await deleted_object(access);a.REPORT['checks']['voicemail_physical_object_deleted']='PASS'
    await room.disconnect()
    a.REPORT['phase']='voicemail_retention_worker'
    call=await a.dial(extension=number);room,_=await a.join(call,0);await a.publish(room)
    for _ in range(100):
        values=await a.request('GET','/v1/enterprise/voicemail/messages?mailboxId='+mailbox['mailboxId']+'&principalId='+a.PEOPLE[1])
        retained=next((m for m in values['messages'] if m['callId']==call),None)
        if retained:break
        await asyncio.sleep(1)
    assert retained,'Retention voicemail was not captured'
    await asyncio.sleep(10)
    if (await a.call_view(call))['state'] not in a.TERMINAL:await a.finish(call)
    retained_path='/v1/enterprise/voicemail/messages/'+retained['messageId']
    for _ in range(90):
        retained_view=await a.request('GET',retained_path+'?principalId='+a.PEOPLE[1])
        if retained_view['state']=='READY':break
        assert retained_view['state'] not in ('FAILED','DELETED')
        await asyncio.sleep(1)
    else:raise AssertionError('Retention voicemail not READY')
    await retention_expiry(a,'voicemail',call,retained_path,retained_view,a.PEOPLE[1]);await room.disconnect()
