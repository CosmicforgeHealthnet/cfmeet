"""Authorized backend journey. Secret context stays on the VPS and private network."""
import json
import os
from pathlib import Path
import secrets
import subprocess
import select
import sys
import time
import urllib.error
import urllib.request
import uuid


def execute(sha, run_id, scope, stage):
    import production as p
    import communications_release as release
    folder=release.WORK/('staging-test-'+run_id if stage else 'production-test-'+run_id)
    folder.mkdir(mode=0o700,exist_ok=True)
    if stage:
        accounts=[]
    else:
        import app_acceptance
        accounts=app_acceptance.accounts()[:2]
    ports=release.STAGE_TLS if stage else release.PROD_TLS
    context=dict(sha=sha,stage=stage,scope=scope,accounts=accounts,
        urls={k:'https://api.cfmeet.com:'+str(v) for k,v in ports.items()})
    if not stage:context['urls']['meeting-service']='https://api.cfmeet.com'
    p.write_private(folder/'context.private.json',json.dumps(context))
    os.chown(folder,10001,10001);os.chown(folder/'context.private.json',10001,10001)
    source=Path(__file__).resolve().parent
    base=json.loads(p.run('docker','image','inspect','python:3.12-slim'))[0]['RepoDigests'][0]
    tag='cfmeet-calling-api-probe:'+__import__('hashlib').sha256((source/'Probe.Dockerfile').read_bytes()+base.encode()).hexdigest()[:20]
    p.run('docker','build','--network','default','--build-arg','PYTHON_IMAGE='+base,'-f',str(source/'Probe.Dockerfile'),'-t',tag,str(source))
    # The test client receives only fixture account credentials and a bounded
    # customer credential. No owner DB password or internal service key.
    command=['docker','run','--rm','--name','cfmeet-communications-client-'+run_id,'--network',p.NET,'--memory','768m','--cpus','1',
        '--mount',f'type=bind,src={folder},dst=/result',
        '--mount',f'type=bind,src={Path(__file__).resolve()},dst=/acceptance.py,readonly',
        '--mount',f'type=bind,src={source}/rtc_client.py,dst=/rtc_client.py,readonly',
        tag,'python','-u','/acceptance.py']
    with (folder/'execution.private.log').open('w') as log:
        result=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT)
        deadline=time.monotonic()+420;progress=time.monotonic()+30
        try:
            while result.poll() is None:
                assert time.monotonic()<deadline,'Communications media acceptance timed out'
                if time.monotonic()>=progress:
                    print('COMMUNICATIONS_ACCEPTANCE_RUNNING: native identity, policy and real media assertions',flush=True);progress=time.monotonic()+30
                time.sleep(.25)
        finally:
            if result.poll() is None:p.run('docker','stop','--time','10','cfmeet-communications-client-'+run_id);result.wait(timeout=20)
    report=json.loads((folder/'report.json').read_text()) if (folder/'report.json').exists() else {'checks':{},'failure':'client did not produce evidence'}
    report.update(sha=sha,workflowRunId=run_id,environment='isolated-vps-staging' if stage else 'production')
    report['images']={name:p.inspect(('cfmeet-comm-stage-'+name) if stage else release.OWNERS.get(name,'cfmeet-communications'))['Image'] for name in release.PORTS}
    report['registryDigests']={name:json.loads(p.run('docker','image','inspect',ref))[0].get('RepoDigests',[]) for name,ref in report['images'].items()}
    manifest=release.WORK/('registry-'+sha+'.json')
    if manifest.exists():
        provenance=json.loads(manifest.read_text())
        assert all(provenance[name]['imageId']==ref for name,ref in report['images'].items()),'Transferred image identity differs from registry evidence'
        report['registryDigests']={name:provenance[name]['registryDigests'] for name in report['images']}
    if result.returncode:
        print(json.dumps(report),flush=True)
        raise RuntimeError('Communications backend acceptance failed; protected evidence retained on VPS')
    assert report['checks'] and all(x=='PASS' for x in report['checks'].values())
    return report


class Rtc:
    def __init__(self,report):
        self.report=report
        self.log=open('/result/rtc-'+uuid.uuid4().hex+'.private.log','w')
        self.process=subprocess.Popen([sys.executable,'/rtc_client.py'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.log,text=True,bufsize=1)
    def cmd(self,cmd,**body):
        self.report['rtcStep']=cmd
        self.process.stdin.write(json.dumps({'cmd':cmd,**body})+'\n');self.process.stdin.flush()
        assert select.select([self.process.stdout],[],[],40)[0],'RTC response timed out'
        line=self.process.stdout.readline();assert line,'RTC worker stopped'
        response=json.loads(line)
        if not response.get('ok'):
            category=str(response.get('error','RTC_ERROR')).split(':',1)[0]
            self.report['rtcFailureClass']=category if category.isidentifier() and len(category)<64 else 'RTC_ERROR'
        assert response.get('ok'),'RTC operation failed: '+cmd
        return response
    def close(self):
        try:self.cmd('quit')
        finally:
            try:self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:self.process.kill();self.process.wait()
            self.log.close()


class Journey:
    def __init__(self,context):
        self.c=context
        self.report={'checks':{},'phase':'authentication'}
        self.calls=[]

    def request(self,owner,path,method='GET',body=None,token=None,actor=None,expected=(200,),key=None):
        headers={'Content-Type':'application/json','Idempotency-Key':key or str(uuid.uuid4())}
        if token:headers['Authorization']='Bearer '+token
        if actor:headers['CF-Acting-Principal']=actor
        req=urllib.request.Request(self.c['urls'][owner]+path,method=method,headers=headers,
            data=json.dumps(body).encode() if body is not None else None)
        try:
            with urllib.request.urlopen(req,timeout=25) as response:
                status=response.status;raw=response.read()
        except urllib.error.HTTPError as e:status=e.code;raw=e.read()
        try:value=json.loads(raw)
        except (ValueError,UnicodeDecodeError):value={}
        if status not in expected:
            error=value.get('error',{})
            self.report['failedRequest']={'owner':owner,'method':method,'route':path.split('?')[0],
                'status':status,'code':error.get('code') if isinstance(error,dict) else None}
            raise AssertionError('Unexpected backend response')
        return value

    def hub(self,person,path,method='GET',body=None,expected=(200,),key=None):
        return self.request('meeting-service','/api/v2/communications'+path,method,body,person['session'],expected=expected,key=key)

    def done(self,name):
        self.report['checks'][name]='PASS'
        self.report['phase']=name

    def accounts(self):
        values=self.c['accounts']
        if not values:
            for _ in range(2):
                username='comm_'+uuid.uuid4().hex[:16]
                a=dict(username=username,email=username+'@acceptance.invalid',password='Aa9!'+secrets.token_urlsafe(28))
                self.request('meeting-service','/api/v1/auth/register','POST',dict(**a,confirm_password=a['password']),expected=(200,201))
                values.append(a)
        for a in values:
            login=self.request('meeting-service','/api/v1/auth/login','POST',{'email':a['email'],'password':a['password']})
            a['session']=login['access_token'];a['refresh']=login['refresh_token']
            profile=self.hub(a,'/me')
            assert profile['activationState']=='ACTIVE' and profile['phoneVerification']['required'] is False
            assert 'principalId' not in profile and 'email' not in profile
            a['cfMeetId']=profile['cfMeetId']
            # Existing authenticated identity API owns canonical client identities.
            identity=self.request('meeting-service','/api/v2/messaging/me',token=a['session'])
            a['principalId']=identity['principalId']
            again=self.hub(a,'/me')
            assert again['cfMeetId']==a['cfMeetId']
        assert values[0]['cfMeetId']!=values[1]['cfMeetId']
        assert values[0]['principalId']!=values[1]['principalId']
        self.done('native_accounts_stable_identity_without_phone')
        return values

    def direct(self,a,b,expected=(201,),profile='MANAGED'):
        return self.request('messaging','/v1/conversations','POST',
            {'contextClass':'DIRECT','securityProfile':profile,'counterpartPrincipalId':b['principalId']},
            self.c['scope']['token'],a['principalId'],expected)

    def dial(self,a,b,kind='VOICE',expected=(201,)):
        return self.request('calling','/v1/calls','POST',{'kind':kind,
            'initiatorPrincipalId':a['principalId'],'initiatorEndpointId':a['endpointId'],
            'targetPrincipalIds':[b['principalId']]},self.c['scope']['token'],expected=expected)

    def connect(self,a,b):
        key=str(uuid.uuid4())
        request=self.hub(a,'/contact-requests','POST',{'cfMeetId':b['cfMeetId']},key=key)
        assert request==self.hub(a,'/contact-requests','POST',{'cfMeetId':b['cfMeetId']},key=key)
        body={'expectedVersion':request['version']};key=str(uuid.uuid4())
        path='/contact-requests/'+request['requestId']+'/accept'
        accepted=self.hub(b,path,'POST',body,key=key)
        assert accepted==self.hub(b,path,'POST',body,key=key)

    def media_call(self,a,b,kind):
        call=self.dial(a,b,kind)['call']['callId']
        token=self.c['scope']['token'];path='/v1/calls/'+call
        for _ in range(40):
            offers=self.request('calling','/v1/calling/invitations',token=b['signaling'])['data']
            invitation=next((x for x in offers if x['callId']==call and x['actionable']),None)
            if invitation:break
            time.sleep(.25)
        else:raise AssertionError('Incoming ringing invitation missing')
        self.request('calling','/v1/calling/invitations/'+invitation['invitationId']+'/accept','POST',{'expectedVersion':invitation['callVersion']},b['signaling'])
        clients=[]
        try:
            current=self.request('calling',path,token=token)
            sessions=[next(x for x in current['sessions'] if x['endpointId']==person['endpointId'] and x['state']!='ENDED') for person in (a,b)]
            self.request('calling','/v1/calling/sessions/'+sessions[1]['sessionId']+'/media-credentials','POST',{},a['signaling'],expected=(404,))
            for person,session in zip((a,b),sessions):
                credentials=self.request('calling','/v1/calling/sessions/'+session['sessionId']+'/media-credentials','POST',{},person['signaling'])
                client=Rtc(self.report);clients.append(client)
                client.cmd('connect',url=credentials['url'],token=credentials['token'])
            for client in clients:
                client.cmd('wait_permission',publish=True,timeout=15)
                client.cmd('publish',kind='audio')
                if kind=='VIDEO':client.cmd('publish',kind='video')
            for _ in range(40):
                if self.request('calling',path,token=token)['state']=='CONNECTED':break
                time.sleep(.25)
            else:raise AssertionError('Native media call did not become CONNECTED')
            for client in clients:
                client.cmd('verify_received',kind='audio',timeout=12)
                if kind=='VIDEO':client.cmd('verify_received',kind='video',timeout=12)
        finally:
            for client in clients:
                try:client.close()
                except Exception:pass
            self.clean_call(call)

    def clean_call(self,call):
        token=self.c['scope']['token'];path='/v1/calls/'+call
        current=self.request('calling',path,token=token)
        verb='end' if current['state'] in ('ANSWERED','CONNECTED','HELD','TRANSFERRING') else 'cancel'
        if current['state'] not in ('ENDED','CANCELLED','FAILED','NO_ANSWER','DECLINED','BUSY','ENDING'):
            self.request('calling',path+'/'+verb,'POST',{'expectedVersion':current['version']},token)
        for _ in range(40):
            state=self.request('calling',path,token=token)['state']
            if state in ('CANCELLED','ENDED','FAILED','NO_ANSWER','DECLINED','BUSY'):return
            time.sleep(.25)
        raise AssertionError('Controlled call did not finish')

    def run(self):
        self.request('meeting-service','/api/v2/communications/me',expected=(401,))
        if not self.c['stage']:
            for path in ('/internal/v1/accounts/communications/status','/internal/v1/accounts/communications/backfill'):
                for method in ('GET','POST'):
                    self.request('meeting-service',path,method,{} if method=='POST' else None,expected=(404,))
            self.done('public_ingress_denies_private_account_endpoints')
        self.request('communications','/internal/v1/communications/policy/check','POST',{},expected=(401,))
        swagger=self.request('meeting-service','/api-docs/communications.json')
        operations=[v for path,item in swagger['paths'].items() for verb,v in item.items() if verb in ('get','put','post','patch','delete')]
        assert len(operations)==29 and not any('/internal/' in x for x in swagger['paths'])
        self.done('authenticated_ingress_private_policy_and_public_swagger')
        a,b=self.accounts()
        capabilities=self.hub(a,'/me/capabilities')
        assert capabilities['phoneVerification']['status']=='DEFERRED'
        if not capabilities['messaging']['securityReady']:assert capabilities['messaging']['available'] is False
        self.done('capabilities_report_actual_messaging_security_readiness')
        if self.c['stage']:
            policy=self.request('messaging','/v1/messaging/policy',token=self.c['scope']['token'])
            self.request('messaging','/v1/messaging/policy','PUT',{'expectedVersion':policy['version'],
                'policy':{'contexts':{'DIRECT':{'allowedProfiles':['MANAGED','PRIVATE_E2EE'],'defaultProfile':'PRIVATE_E2EE','requestsRequireAcceptance':False}}}},self.c['scope']['token'])
        for a0 in (a,b):
            endpoint=self.request('calling','/v1/calling/endpoints','POST',{'principalId':a0['principalId'],
                'endpointKey':'comm-'+uuid.uuid4().hex,'endpointKind':'WEB','capabilities':{'audio':True,'video':True}},self.c['scope']['token'],expected=(201,))
            a0['endpointId']=endpoint['endpointId']
            a0['signaling']=self.request('calling','/v1/calling/signaling-credentials','POST',{'principalId':a0['principalId'],'endpointId':a0['endpointId']},self.c['scope']['token'],expected=(200,201))['token']
        # Fresh dedicated accounts must have no implicit personal permission.
        # Previously accepted production fixtures are removed through their API.
        contacts=self.hub(a,'/contacts')['data']
        for contact in contacts:
            if contact['cfMeetId']==b['cfMeetId']:self.hub(a,'/contacts/'+contact['contactId'],'DELETE')
        denied=self.dial(a,b,expected=(403,404,409))
        assert denied.get('error',{}).get('code')=='TARGET_UNAVAILABLE'
        if self.c['stage']:
            denied=self.direct(a,b,expected=(403,404,409))
            assert denied.get('error',{}).get('code')=='DELIVERY_REFUSED'
        self.done('unconnected_direct_apis_cannot_bypass_relationships')
        invite=self.hub(a,'/invites','POST',{'purpose':'CONNECT','expiresInSeconds':600,'maxUses':1})
        assert invite['payload'].startswith('https://cfmeet.com/connect/')
        token=invite['payload'].rsplit('/',1)[1]
        resolved=self.hub(b,'/discovery/invites/resolve','POST',{'purpose':'CONNECT','token':token})
        assert resolved['cfMeetId']==a['cfMeetId'] and resolved['relationship'] in ('NONE','REVOKED')
        assert not any(k in resolved for k in ('principalId','email','phone','tenantId'))
        listing=self.hub(a,'/invites')
        assert token not in json.dumps(listing)
        self.hub(b,'/invites/'+invite['inviteId'],'DELETE',expected=(404,))
        self.done('opaque_qr_invitation_owner_isolation_and_private_discovery')
        self.connect(a,b)
        self.done('idempotent_request_and_acceptance')
        conversation=None
        if self.c['stage']:
            conversation=self.direct(a,b)['conversationId']
            assert self.direct(a,b,expected=(200,))['conversationId']==conversation
            self.request('messaging','/v1/conversations/'+conversation+'/messages','POST',
                {'clientMessageId':str(uuid.uuid4()),'text':'Isolated Sections 4-7 backend acceptance'},self.c['scope']['token'],a['principalId'],(201,))
            self.done('canonical_direct_conversation_reuse_and_managed_test_message')
        for kind in ('VOICE','VIDEO'):
            self.media_call(a,b,kind)
        self.done('native_voice_video_ringing_accept_media_credentials_and_bidirectional_frames')
        contact=self.hub(a,'/contacts')['data'][0]
        self.hub(a,'/contacts/'+contact['contactId'],'PATCH',{'expectedVersion':contact['version'],'alias':'My saved contact','favorite':True})
        assert self.hub(b,'/contacts')['data'][0]['alias'] is None
        self.hub(a,'/contacts/'+contact['contactId']+'/common-groups')
        self.done('separate_aliases_and_messaging_owned_common_groups')
        self.hub(b,'/profiles/'+a['cfMeetId']+'/block','PUT')
        assert self.dial(a,b,expected=(403,404,409)).get('error',{}).get('code')=='TARGET_UNAVAILABLE'
        if self.c['stage']:
            assert self.direct(a,b,expected=(403,404,409)).get('error',{}).get('code')=='DELIVERY_REFUSED'
            denied=self.request('messaging','/v1/conversations/'+conversation+'/messages','POST',
                {'clientMessageId':str(uuid.uuid4()),'text':'must be refused'},self.c['scope']['token'],a['principalId'],(403,404,409))
            assert denied.get('error',{}).get('code')=='DELIVERY_REFUSED'
        self.hub(b,'/profiles/'+a['cfMeetId']+'/block','DELETE')
        assert self.dial(a,b,expected=(403,404,409)).get('error',{}).get('code')=='TARGET_UNAVAILABLE'
        self.done('immediate_block_and_unblock_without_implicit_reconnection')
        self.connect(a,b)
        profile=self.hub(a,'/me')
        rotated=self.hub(a,'/me/public-id/rotate','POST',{'expectedVersion':profile['version']})
        assert rotated['cfMeetId']!=a['cfMeetId']
        self.hub(b,'/discovery/public-id/resolve','POST',{'cfMeetId':a['cfMeetId']},expected=(404,))
        self.hub(b,'/discovery/invites/resolve','POST',{'purpose':'CONNECT','token':token},expected=(404,))
        assert self.request('meeting-service','/api/v2/messaging/me',token=a['session'])['principalId']==a['principalId']
        self.done('rotation_invalidates_old_id_invite_and_preserves_principal')
        self.request('meeting-service','/api/v1/meetings',token=a['session'])
        self.done('meetings_remain_independently_available')
        # Close only fixture relationships; retained account/principal/history IDs
        # allow audit and subsequent regression runs without touching customers.
        for contact in self.hub(a,'/contacts')['data']:
            if contact['cfMeetId']==b['cfMeetId']:self.hub(a,'/contacts/'+contact['contactId'],'DELETE')
        self.done('fixture_relationship_cleanup')
        self.report['limitations']=['Real media uses controlled synthetic audio tones and video frames, not browser microphone or camera permissions.',
            'MANAGED messages are exercised only in the isolated TEST application; production MLS activation remains the next milestone.']


if __name__=='__main__':
    os.umask(0o077)
    journey=Journey(json.loads(Path('/result/context.private.json').read_text()))
    failed=False
    try:journey.run()
    except Exception as error:
        failed=True;journey.report['failureType']=type(error).__name__
        if isinstance(error,AssertionError) and str(error).startswith(('RTC operation failed:','RTC response timed out','RTC worker stopped','Incoming ringing invitation missing','Native media call did not become CONNECTED','Controlled call did not finish')):
            journey.report['failure']=str(error)
    Path('/result/report.json').write_text(json.dumps(journey.report,indent=2))
    sys.exit(1 if failed else 0)
