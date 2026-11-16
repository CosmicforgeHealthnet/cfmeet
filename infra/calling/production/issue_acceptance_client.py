"""Production verification of the CF Meet server-bug fixes (16 November 2026), client side.

Runs inside the existing probe image (python + livekit), started by `issue_acceptance.py`.
Reads `/result/context.private.json`: the controlled acceptance accounts, a short-lived
credential restricted to those accounts' principals, and the public base URLs. It talks only
to the public API, exactly as the CF Meet web adapter does, and writes `/result/report.json`:
one PASS / FAIL / NOT_RUN per check plus non-secret ids. Tokens, passwords and credentials are
never written or printed.

It creates calls, one group and one attachment between the controlled accounts only; it never
reads, rings or joins anyone else. Everything it creates is ended, archived or removed, and a
contact relationship is removed again only if this run created it.

Checks (issue ids from the server-bug document):
  C1  an offer stays PENDING and the Call DIALING until a callee device acknowledges receipt;
      the acknowledgement moves both to RINGING
  C2  cancelling (acknowledged or not) closes the callee's offer: nothing actionable, and
      neither party's history shows RINGING
  C3  a real LiveKit connection is admitted for both parties: CONNECTED, audio both ways
  C4  a forged signaling credential is refused with 401 AUTHENTICATION_FAILED
  C5  the connected voice call is upgraded to VIDEO and a camera track then flows
  C6  one presenter at a time; only the presenter's effective LiveKit permission carries
      the screen sources; stopping clears it
  C7  a third person is invited into the ongoing call, acknowledges, accepts and joins
  M1  attachments enabled for CF Meet (capability and effective policy); a MANAGED image is
      uploaded, scanned CLEAN, sent and downloaded by a member; a non-member gets 404
  M2  groups in common carry the MANAGED group's title for current members only
  M3  shared media carries the sanitized file name, media type and grant operations
"""
import hashlib
import json
import os
import struct
import sys
import time
import urllib.error
import urllib.request
import uuid
import zlib
from pathlib import Path

RESULT = Path('/result')
TERMINAL = ('ENDED', 'CANCELLED', 'FAILED', 'NO_ANSWER', 'DECLINED', 'BUSY')
KINDS = {'IMAGE', 'DOCUMENT', 'FILE', 'VOICE_NOTE', 'VIDEO'}
MAX_BYTES = 26_214_400
SCREEN_SOURCES = {3, 4}  # LiveKit TrackSource SCREEN_SHARE, SCREEN_SHARE_AUDIO
FILE_NAME = 'cfmeet-verification.png'


class Failed(AssertionError):
    pass


def png(width=32, height=32):
    """A small valid PNG (solid colour) so the scanner and thumbnailer see a real image."""
    def chunk(kind, data):
        body = kind + data
        return struct.pack('>I', len(data)) + body + struct.pack('>I', zlib.crc32(body) & 0xffffffff)
    raw = b''.join(b'\x00' + b'\x1f\x6f\xc4' * width for _ in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))


def http(method, url, body=None, headers=None, raw=False, timeout=30):
    data = None
    headers = dict(headers or {})
    if body is not None:
        if isinstance(body, (bytes, bytearray)):
            data = bytes(body)
        else:
            data = json.dumps(body).encode()
            headers.setdefault('Content-Type', 'application/json')
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            status, payload = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, payload = error.code, error.read()
    if raw:
        return status, payload
    try:
        value = json.loads(payload) if payload else {}
    except ValueError:
        value = {}
    return status, value


def error_code(value):
    error = value.get('error') if isinstance(value, dict) else None
    return error.get('code') if isinstance(error, dict) else None


def outcome_of(entry):
    """The viewer's outcome in a Calling history entry (participation) or a Hub entry."""
    participation = entry.get('participation')
    if isinstance(participation, dict):
        return participation.get('outcome')
    return entry.get('outcome')


class Journey:
    def __init__(self, context):
        self.c = context
        self.checks = {}
        self.details = {}
        self.people = []
        self.calls = []
        self.endpoints = []
        self.contacts_created = []
        self.group = None

    # -- reporting -----------------------------------------------------------------------
    def record(self, name, state, note=None):
        self.checks[name] = state
        if note:
            self.details.setdefault('notes', {})[name] = note
        print('CHECK', state, name, note or '', flush=True)

    def check(self, name, ok, note=None):
        self.record(name, 'PASS' if ok else 'FAIL', None if ok else note)
        return ok

    def group_of(self, name, fn):
        """One failed group never hides the others; the failure is named, never a secret."""
        try:
            return fn()
        except Failed as error:
            self.record(name, 'FAIL', str(error)[:200])
        except Exception as error:  # noqa: BLE001
            self.record(name, 'FAIL', type(error).__name__)

    # -- transports ----------------------------------------------------------------------
    def api(self, method, path, body=None, token=None, actor=None, base=None, expected=(200,)):
        headers = {'Accept': 'application/json', 'Idempotency-Key': str(uuid.uuid4())}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        if actor:
            headers['CF-Acting-Principal'] = actor
        if body is None and method in ('POST', 'PUT', 'PATCH'):
            body = {}
        status, value = http(method, (base or self.c['base']) + path, body, headers)
        if status not in expected:
            raise Failed('%s %s answered %d %s' % (method, path.split('?')[0], status, error_code(value) or ''))
        return value

    def scoped(self, method, path, body=None, actor=None, expected=(200,)):
        return self.api(method, path, body, self.c['credential'], actor, expected=expected)

    def messaging(self, method, path, body=None, actor=None, expected=(200,)):
        return self.api(method, path, body, self.c['credential'], actor, self.c['messaging'], expected)

    def hub(self, person, path, method='GET', body=None, expected=(200,)):
        return self.api(method, '/api/v2/communications' + path, body, person['session'], expected=expected)

    def signaling(self, person, method, path, body=None, expected=(200,)):
        return self.api(method, path, body, person['signaling'], expected=expected)

    # -- setup -----------------------------------------------------------------------------
    def sign_in(self):
        for account in self.c['accounts']:
            login = self.api('POST', '/api/v1/auth/login', {'email': account['email'], 'password': account['password']})
            person = {'session': login['access_token'], 'refresh': login.get('refresh_token')}
            person['principalId'] = self.api('GET', '/api/v2/messaging/me', token=person['session'])['principalId']
            person['cfMeetId'] = self.hub(person, '/me')['cfMeetId']
            self.people.append(person)
        allowed = set(self.c.get('principals') or [])
        if allowed and {p['principalId'] for p in self.people} != allowed:
            raise Failed('signed-in principals differ from the credential restriction')
        self.details['testPrincipals'] = [p['principalId'] for p in self.people]

    def contact(self, a, b):
        for item in self.hub(a, '/contacts')['data']:
            if item.get('cfMeetId') == b['cfMeetId'] and item.get('relationship', 'CONNECTED') == 'CONNECTED':
                return item['contactId']
        request = self.hub(a, '/contact-requests', 'POST', {'cfMeetId': b['cfMeetId']}, expected=(200, 201))
        self.hub(b, '/contact-requests/%s/accept' % request['requestId'], 'POST',
                 {'expectedVersion': request['version']}, expected=(200, 201))
        for item in self.hub(a, '/contacts')['data']:
            if item.get('cfMeetId') == b['cfMeetId']:
                self.contacts_created.append((a, item['contactId']))
                return item['contactId']
        raise Failed('controlled contact relationship not established')

    def endpoint(self, person):
        body = {'principalId': person['principalId'], 'endpointKey': 'issue-verification-' + uuid.uuid4().hex,
                'endpointKind': 'WEB', 'capabilities': {'audio': True, 'video': True}}
        created = self.scoped('POST', '/v1/calling/endpoints', body, expected=(201,))
        person['endpointId'] = created['endpointId']
        self.endpoints.append(created['endpointId'])
        person['signaling'] = self.scoped('POST', '/v1/calling/signaling-credentials',
                                          {'principalId': person['principalId'], 'endpointId': person['endpointId']},
                                          expected=(200, 201))['token']

    # -- calling helpers -------------------------------------------------------------------
    def dial(self, a, b):
        created = self.scoped('POST', '/v1/calls', {'kind': 'VOICE', 'initiatorPrincipalId': a['principalId'],
                                                   'initiatorEndpointId': a['endpointId'],
                                                   'targetPrincipalIds': [b['principalId']]}, expected=(201,))
        call = created.get('call', created)
        self.calls.append(call['callId'])
        return call

    def view(self, call_id):
        return self.scoped('GET', '/v1/calls/' + call_id)

    def offer(self, person, call_id, rounds=40):
        for _ in range(rounds):
            for item in self.signaling(person, 'GET', '/v1/calling/invitations')['data']:
                if item['callId'] == call_id:
                    return item
            time.sleep(.25)
        return None

    def finish(self, call_id):
        current = self.view(call_id)
        if current['state'] not in TERMINAL + ('ENDING',):
            verb = 'end' if current['state'] in ('ANSWERED', 'CONNECTED', 'HELD', 'TRANSFERRING') else 'cancel'
            self.scoped('POST', '/v1/calls/%s/%s' % (call_id, verb), {'expectedVersion': current['version']})
        for _ in range(60):
            current = self.view(call_id)
            if current['state'] in TERMINAL:
                return current
            time.sleep(.25)
        raise Failed('controlled call did not finish')

    def history_entry(self, person, call_id):
        for item in self.signaling(person, 'GET', '/v1/calling/history?limit=50')['data']:
            if item.get('callId') == call_id:
                return item
        return None

    def hub_history_entry(self, person, call_id):
        for item in self.hub(person, '/calls/history?limit=50')['data']:
            if item.get('callId') == call_id:
                return item
        return None

    # -- C4 --------------------------------------------------------------------------------
    def c4(self):
        status, value = http('GET', self.c['base'] + '/v1/calling/invitations',
                             headers={'Authorization': 'Bearer cfsig_forged_' + uuid.uuid4().hex})
        self.check('C4 forged signaling credential refused with 401 AUTHENTICATION_FAILED',
                   status == 401 and error_code(value) == 'AUTHENTICATION_FAILED', '%d %s' % (status, error_code(value)))

    # -- C1 + C2 ---------------------------------------------------------------------------
    def c1_c2(self, a, b):
        # Offline callee: the offer was delivered to no device yet.
        call = self.dial(a, b)
        self.details['c1CallId'] = call['callId']
        self.check('C1 a new app call starts DIALING, not RINGING', call['state'] == 'DIALING', call['state'])
        offer = self.offer(b, call['callId'])
        if offer is None:
            raise Failed('callee endpoint received no offer')
        self.check('C1 the callee offer stays PENDING before any device acknowledges it',
                   offer['status'] == 'PENDING' and offer['actionable'] is True, offer['status'])
        self.check('C1 the caller sees DIALING while no device has acknowledged',
                   self.view(call['callId'])['state'] == 'DIALING')
        acknowledged = self.signaling(b, 'POST', '/v1/calling/invitations/%s/acknowledge' % offer['invitationId'])
        self.check('C1 the device acknowledgement moves the offer and the Call to RINGING',
                   acknowledged.get('status') == 'RINGING' and acknowledged.get('callState') == 'RINGING',
                   '%s/%s' % (acknowledged.get('status'), acknowledged.get('callState')))
        self.check('C1 the caller sees RINGING only after the acknowledgement',
                   self.view(call['callId'])['state'] == 'RINGING')
        self.cancel_and_check(a, b, call['callId'], 'acknowledged')
        # The reported case: a callee that never came online, then the caller cancels.
        offline = self.dial(a, b)
        self.details['c2OfflineCallId'] = offline['callId']
        pending = self.offer(b, offline['callId'])
        self.check('C1 an unacknowledged offer is still PENDING on a second call',
                   pending is not None and pending['status'] == 'PENDING')
        self.cancel_and_check(a, b, offline['callId'], 'never acknowledged')

    def cancel_and_check(self, a, b, call_id, label):
        finished = self.finish(call_id)
        self.check('C2 (%s) the cancelled call is CANCELLED' % label, finished['state'] == 'CANCELLED', finished['state'])
        offers = [x for x in self.signaling(b, 'GET', '/v1/calling/invitations')['data'] if x['callId'] == call_id]
        self.check('C2 (%s) the callee has no actionable offer left' % label,
                   not any(x['actionable'] for x in offers) and all(x['status'] not in ('PENDING', 'RINGING') for x in offers),
                   ','.join(x['status'] for x in offers))
        for who, person in (('callee', b), ('caller', a)):
            entry = self.history_entry(person, call_id)
            self.check('C2 (%s) the %s history entry is not RINGING' % (label, who),
                       entry is not None and outcome_of(entry) != 'RINGING' and entry.get('state') == 'CANCELLED',
                       'missing' if entry is None else '%s/%s' % (entry.get('state'), outcome_of(entry)))
        hub_entry = self.hub_history_entry(b, call_id)
        self.check('C2 (%s) the callee Hub history shows a finished, not ringing, call' % label,
                   hub_entry is not None and hub_entry.get('terminal', True) and outcome_of(hub_entry) != 'RINGING',
                   'missing' if hub_entry is None else '%s/%s' % (hub_entry.get('state'), outcome_of(hub_entry)))

    # -- C3 --------------------------------------------------------------------------------
    def c3(self, a, b):
        sys.path.insert(0, '/')
        from communications_acceptance import Rtc  # noqa: E402 (mounted read-only by the runner)
        call = self.dial(a, b)
        self.details['c3CallId'] = call['callId']
        offer = self.offer(b, call['callId'])
        if offer is None:
            raise Failed('callee endpoint received no offer')
        self.signaling(b, 'POST', '/v1/calling/invitations/%s/accept' % offer['invitationId'],
                       {'expectedVersion': offer['callVersion']})
        clients = []
        try:
            current = self.view(call['callId'])
            sessions = [next(x for x in current['sessions'] if x['endpointId'] == person['endpointId'] and x['state'] != 'ENDED')
                        for person in (a, b)]
            report = {}
            for person, session in zip((a, b), sessions):
                credential = self.signaling(person, 'POST', '/v1/calling/sessions/%s/media-credentials' % session['sessionId'], {})
                client = Rtc(report)
                clients.append(client)
                client.cmd('connect', url=credential['url'], token=credential['token'])
            self.check('C3 both parties are admitted to media (no "authoritative state unavailable")', True)
            for client in clients:
                client.cmd('wait_permission', publish=True, timeout=15)
                client.cmd('publish', kind='audio')
            for _ in range(120):
                state = self.view(call['callId'])['state']
                if state == 'CONNECTED':
                    break
                time.sleep(.25)
            self.check('C3 the call becomes CONNECTED', state == 'CONNECTED', state)
            for client in clients:
                client.cmd('verify_received', kind='audio', timeout=12)
            self.check('C3 audio is received in both directions', True)
            self.group_of('C5 voice to video upgrade', lambda: self.c5(call['callId'], clients))
            self.group_of('C6 screen sharing', lambda: self.c6(call['callId'], a, b, clients))
            if len(self.people) > 2:
                self.group_of('C7 invite into an ongoing call', lambda: self.c7(call['callId'], a, self.people[2]))
            else:
                self.record('C7 invite into an ongoing call', 'NOT_RUN', 'only two controlled accounts')
        finally:
            for client in clients:
                try:
                    client.close()
                except Exception:  # noqa: BLE001
                    pass
            self.finish(call['callId'])

    # -- C5 / C6 / C7 (on the connected C3 call) ---------------------------------------
    def participant(self, call_id, person):
        for item in self.view(call_id)['participants']:
            if item.get('principalId') == person['principalId'] and not item.get('endedAt'):
                return item['participantId']
        raise Failed('active participant missing')

    @staticmethod
    def screen_granted(client):
        """Whether the connected participant's effective LiveKit permission (pushed by
        Calling after commit) includes both screen sources. The client credential never
        carries sources: the media gateway grants them when it admits the connection."""
        return SCREEN_SOURCES <= set(client.cmd('sources')['sources'])

    def await_screen(self, client, granted, seconds=15):
        deadline = time.monotonic() + seconds
        while True:
            state = self.screen_granted(client)
            if state == granted or time.monotonic() > deadline:
                return state
            time.sleep(.25)

    def c5(self, call_id, clients):
        current = self.view(call_id)
        self.scoped('POST', '/v1/calls/%s/upgrade' % call_id, {'kind': 'VIDEO', 'expectedVersion': current['version']})
        after = self.view(call_id)
        self.check('C5 the connected voice call becomes VIDEO and stays CONNECTED',
                   after['kind'] == 'VIDEO' and after['state'] == 'CONNECTED', '%s/%s' % (after['kind'], after['state']))
        # The new permission reaches LiveKit after commit: publish once it is applied.
        for _ in range(30):
            try:
                clients[0].cmd('publish', kind='video')
                break
            except AssertionError:
                time.sleep(.5)
        else:
            raise Failed('camera publish still refused after the upgrade')
        clients[1].cmd('verify_received', kind='video', timeout=12)
        self.check('C5 after the upgrade a camera track flows to the other party', True)

    def c6(self, call_id, a, b, clients):
        pa, pb = self.participant(call_id, a), self.participant(call_id, b)
        started = self.scoped('POST', '/v1/calls/%s/screen-share' % call_id, {'participantId': pa})
        self.check('C6 a participant starts presenting',
                   (started.get('screenShare') or {}).get('participantId') == pa, json.dumps(started.get('screenShare')))
        self.check('C6 the call view names the presenter',
                   (self.view(call_id).get('screenShare') or {}).get('participantId') == pa)
        presenter = self.await_screen(clients[0], True)
        other = self.screen_granted(clients[1])
        self.check('C6 only the presenter\'s LiveKit permission carries SCREEN_SHARE and SCREEN_SHARE_AUDIO',
                   presenter and not other, 'presenter %s / other %s' % (presenter, other))
        status, value = http('POST', self.c['base'] + '/v1/calls/%s/screen-share' % call_id, {'participantId': pb},
                             {'Authorization': 'Bearer ' + self.c['credential'], 'Idempotency-Key': str(uuid.uuid4())})
        self.check('C6 a second presenter is refused with 409', status == 409, '%d %s' % (status, error_code(value)))
        stopped = self.scoped('POST', '/v1/calls/%s/screen-share/stop' % call_id, {'participantId': pa})
        self.check('C6 stopping clears the presenter and the screen grant',
                   stopped.get('screenShare') is None and self.view(call_id).get('screenShare') is None
                   and self.await_screen(clients[0], False) is False)

    def c7(self, call_id, a, c):
        current = self.view(call_id)
        added = self.scoped('POST', '/v1/calls/%s/participants' % call_id,
                            {'invitingParticipantId': self.participant(call_id, a),
                             'targetPrincipalIds': [c['principalId']], 'expectedVersion': current['version']})
        self.check('C7 the invited person is added as a participant with a live offer',
                   [x.get('principalId') for x in added.get('participants', [])] == [c['principalId']])
        offer = self.offer(c, call_id)
        self.check('C7 the added offer is delivered PENDING', offer is not None and offer['status'] == 'PENDING',
                   None if offer is None else offer['status'])
        if offer is None:
            return
        acknowledged = self.signaling(c, 'POST', '/v1/calling/invitations/%s/acknowledge' % offer['invitationId'])
        self.signaling(c, 'POST', '/v1/calling/invitations/%s/accept' % offer['invitationId'],
                       {'expectedVersion': acknowledged.get('callVersion', offer['callVersion'])})
        after = self.view(call_id)
        joined = [x for x in after['participants'] if x.get('principalId') == c['principalId'] and not x.get('endedAt')]
        self.check('C7 the invited person accepts and joins while the call continues',
                   bool(joined) and after['state'] in ('CONNECTED', 'ANSWERED'), after['state'])

    def lacking(self, *needed):
        """Scopes the verification credential does not hold (the application is not allowed them)."""
        held = set(self.c.get('scopes') or [])
        return [s for s in needed if s not in held] if 'scopes' in self.c else []

    # -- M1 / M2 / M3 ----------------------------------------------------------------------
    def m1_capability(self, a):
        capability = self.hub(a, '/me/capabilities')['messaging']
        attachments = capability.get('attachments') or {}
        self.details['messagingCapability'] = {k: capability.get(k) for k in ('available', 'reason')}
        self.details['attachmentsCapability'] = attachments
        self.check('M1 clients are told attachments are enabled (25 MiB, five kinds)',
                   attachments.get('enabled') is True and attachments.get('maxBytes') == MAX_BYTES
                   and KINDS <= set(attachments.get('allowedKinds') or []), json.dumps(attachments)[:200])
        missing = self.lacking('conversations.manage')
        if missing:
            self.record('M1 the CF Meet environment runs the CFMEET_FIRST_PARTY attachment baseline', 'NOT_RUN',
                        'policy read needs ' + ', '.join(missing))
            return capability
        policy = self.messaging('GET', '/v1/messaging/policy', actor=a['principalId'])
        effective = (policy.get('effective') or {}).get('attachments') or {}
        self.details['policyBaseline'] = policy.get('applicationBaseline')
        self.check('M1 the CF Meet environment runs the CFMEET_FIRST_PARTY attachment baseline',
                   policy.get('applicationBaseline') == 'CFMEET_FIRST_PARTY' and effective.get('enabled') is True
                   and effective.get('maxBytes') == MAX_BYTES, '%s/%s' % (policy.get('applicationBaseline'), effective.get('enabled')))
        return capability

    def m_group(self, a, b, outsider, contact_id):
        title = 'CF Meet verification ' + self.c['runId']
        missing = self.lacking('conversations.create', 'conversations.manage', 'messages.send', 'attachments.write')
        if missing:
            for name in ('M1 upload, scan, send and member download', 'M2 groups in common carry the group title',
                         'M3 shared media carries file name and grant operations'):
                self.record(name, 'NOT_RUN', 'the CF Meet application is not allowed ' + ', '.join(missing))
            return
        status, created = http('POST', self.c['messaging'] + '/v1/conversations',
                               {'contextClass': 'GROUP', 'securityProfile': 'MANAGED', 'title': title},
                               {'Authorization': 'Bearer ' + self.c['credential'], 'CF-Acting-Principal': a['principalId'],
                                'Idempotency-Key': str(uuid.uuid4())})
        if status != 201:
            note = 'MANAGED group creation answered %d %s' % (status, error_code(created) or '')
            for name in ('M1 upload, scan, send and member download', 'M2 groups in common carry the group title',
                         'M3 shared media carries file name and grant operations'):
                self.record(name, 'NOT_RUN', note)
            return
        cid = created.get('conversationId') or created.get('id')
        self.group = (cid, a['principalId'])
        self.details['groupConversationId'] = cid
        self.messaging('POST', '/v1/conversations/%s/members' % cid, {'principalId': b['principalId']},
                       actor=a['principalId'], expected=(200, 201))
        self.group_of('M1 upload, scan, send and member download', lambda: self.m1_upload(a, b, outsider, cid))
        self.group_of('M2 groups in common carry the group title', lambda: self.m2(a, b, cid, title, contact_id))

    def m1_upload(self, a, b, outsider, cid):
        data = png()
        digest = hashlib.sha256(data).hexdigest()
        intent = self.messaging('POST', '/v1/conversations/%s/attachments' % cid,
                                {'kind': 'IMAGE', 'size': len(data), 'sha256': digest, 'mediaType': 'image/png',
                                 'fileName': FILE_NAME}, actor=a['principalId'], expected=(201,))
        aid = intent['attachmentId']
        self.details['attachmentId'] = aid
        upload = intent['upload']
        headers = {h['name']: h['value'] for h in upload.get('headers') or []}
        status, _ = http(upload.get('method', 'PUT'), upload['url'], data, headers, raw=True, timeout=60)
        if status not in (200, 201, 204):
            raise Failed('signed upload answered %d' % status)
        state = self.messaging('POST', '/v1/conversations/%s/attachments/%s/finalize' % (cid, aid), {},
                               actor=a['principalId'])
        for _ in range(40):
            if state.get('state') not in ('VERIFYING', 'INTENT_CREATED', 'SCAN_PENDING'):
                break
            time.sleep(1.5)
            state = self.messaging('GET', '/v1/conversations/%s/attachments/%s' % (cid, aid), actor=a['principalId'])
        self.check('M1 a MANAGED image is scanned CLEAN and becomes READY',
                   state.get('state') == 'READY', '%s %s' % (state.get('state'), state.get('stateReason') or ''))
        if state.get('state') != 'READY':
            return
        self.messaging('POST', '/v1/conversations/%s/messages' % cid,
                       {'clientMessageId': str(uuid.uuid4()), 'text': FILE_NAME, 'attachmentIds': [aid]},
                       actor=a['principalId'], expected=(201,))
        grant = self.messaging('POST', '/v1/conversations/%s/attachments/%s/download-grants' % (cid, aid), {},
                               actor=b['principalId'])
        status, body = http('GET', grant['url'], raw=True, timeout=60)
        self.check('M1 a member downloads exactly the uploaded bytes', status == 200 and body == data, str(status))
        if outsider is not None:
            status, value = http('POST', self.c['messaging'] + '/v1/conversations/%s/attachments/%s/download-grants' % (cid, aid),
                                 {}, {'Authorization': 'Bearer ' + self.c['credential'],
                                      'CF-Acting-Principal': outsider['principalId'], 'Idempotency-Key': str(uuid.uuid4())})
            self.check('M1 a non-member cannot obtain a download grant (404)', status == 404, str(status))
        else:
            self.record('M1 a non-member cannot obtain a download grant (404)', 'NOT_RUN', 'only two controlled accounts')
        self.group_of('M3 shared media carries file name and grant operations', lambda: self.m3(b, cid, aid))

    def m3(self, viewer, cid, aid):
        page = self.hub(viewer, '/conversations/%s/media' % cid)
        items = page.get('data') or page.get('items') or []
        item = next((x for x in items if x.get('attachmentId') == aid), None)
        if item is None:
            raise Failed('uploaded attachment missing from shared media')
        self.details['sharedMediaItem'] = {k: item.get(k) for k in ('fileName', 'mediaType', 'kind', 'metadataLocation', 'state')}
        self.details['sharedMediaThumbnail'] = 'PRESENT' if item.get('thumbnail') else 'NOT_READY_OR_ABSENT'
        download = item.get('download') or {}
        self.check('M3 shared media carries the sanitized file name and media type',
                   item.get('fileName') == FILE_NAME and item.get('mediaType') == 'image/png'
                   and item.get('metadataLocation') == 'SERVER', json.dumps(self.details['sharedMediaItem']))
        self.check('M3 shared media names the access-checked grant operation, never a URL',
                   download.get('path', '').endswith('/attachments/%s/download-grants' % aid)
                   and not any('url' in (part or {}) for part in (item, download, item.get('thumbnail'))),
                   json.dumps(download))

    def m2(self, a, b, cid, title, contact_id):
        groups = self.hub(a, '/contacts/%s/common-groups' % contact_id)['groups']
        mine = next((g for g in groups if g['conversationId'] == cid), None)
        self.check('M2 groups in common list the shared MANAGED group with its title',
                   mine is not None and mine.get('title') == title and mine.get('securityProfile') == 'MANAGED'
                   and mine.get('memberCount') == 2, json.dumps(mine)[:200])
        self.messaging('DELETE', '/v1/conversations/%s/members/%s' % (cid, b['principalId']), actor=a['principalId'],
                       expected=(200, 204))
        groups = self.hub(a, '/contacts/%s/common-groups' % contact_id)['groups']
        self.check('M2 a group the contact no longer belongs to is not listed',
                   all(g['conversationId'] != cid for g in groups))

    # -- cleanup ---------------------------------------------------------------------------
    def cleanup(self):
        for call_id in self.calls:
            try:
                self.finish(call_id)
            except Exception:  # noqa: BLE001
                self.details.setdefault('cleanupFailures', []).append('call')
        for endpoint_id in self.endpoints:
            try:
                self.scoped('DELETE', '/v1/calling/endpoints/' + endpoint_id, expected=(200, 204, 404))
            except Exception:  # noqa: BLE001
                self.details.setdefault('cleanupFailures', []).append('endpoint')
        if self.group:
            cid, owner = self.group
            try:
                current = self.messaging('GET', '/v1/conversations/' + cid, actor=owner)
                self.messaging('PATCH', '/v1/conversations/' + cid,
                               {'expectedVersion': current['version'], 'lifecycleState': 'ARCHIVED'}, actor=owner)
            except Exception:  # noqa: BLE001
                self.details.setdefault('cleanupFailures', []).append('group')
        for owner, contact_id in self.contacts_created:
            try:
                self.hub(owner, '/contacts/' + contact_id, 'DELETE', expected=(200, 204))
            except Exception:  # noqa: BLE001
                self.details.setdefault('cleanupFailures', []).append('contact')
        for person in self.people:
            if person.get('refresh'):
                try:
                    self.api('POST', '/api/v1/auth/logout', {'refresh_token': person['refresh']}, person['session'])
                except Exception:  # noqa: BLE001
                    pass

    def run(self):
        try:
            self.sign_in()
            a, b = self.people[0], self.people[1]
            outsider = self.people[2] if len(self.people) > 2 else None
            self.c4()
            contact_id = self.contact(a, b)
            self.endpoint(a)
            self.endpoint(b)
            if outsider is not None:
                # C7 invites the third account into an ongoing call: like any personal call,
                # that needs a contact relationship (removed again if this run created it).
                self.contact(a, outsider)
                self.endpoint(outsider)
            self.group_of('C1/C2 offer delivery and cancellation', lambda: self.c1_c2(a, b))
            self.group_of('C3 media admission', lambda: self.c3(a, b))
            self.group_of('M1 capability and policy', lambda: self.m1_capability(a))
            self.group_of('M1-M3 managed group journey', lambda: self.m_group(a, b, outsider, contact_id))
        finally:
            self.cleanup()

    def report(self):
        return {'runId': self.c['runId'], 'checks': self.checks, 'details': self.details}


def main():
    os.umask(0o077)
    journey = Journey(json.loads((RESULT / 'context.private.json').read_text()))
    failed = False
    try:
        journey.run()
    except Failed as error:
        failed = True
        journey.record('journey setup', 'FAIL', str(error)[:200])
    except Exception as error:  # noqa: BLE001
        failed = True
        journey.record('journey setup', 'FAIL', type(error).__name__)
    (RESULT / 'report.json').write_text(json.dumps(journey.report(), indent=2))
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    main()
