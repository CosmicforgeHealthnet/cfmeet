"""Meetings production acceptance client: Q&A restore and LiveKit participant identity.

Runs inside the probe image (python + livekit 1.1.20) started by `meeting_acceptance.py`.
Reads `/result/context.private.json` (the controlled acceptance account), talks only to the
public API exactly as the web/mobile app does, joins real LiveKit participants with the
credentials the Meeting Service issues, and writes `/result/report.json`: one PASS/FAIL per
check plus non-secret ids. Tokens, passwords and credentials are never written or printed.

It creates its own meeting space and conference, never touches any other meeting, and always
ends the conference and archives the space.
"""
import asyncio
import base64
import json
import time
import urllib.error
import urllib.request
from pathlib import Path

RESULT = Path('/result')
SAME_NAME = 'Acceptance Alex'


class Failed(AssertionError):
    pass


def claims(credential):
    """Payload of a LiveKit access token (signature is LiveKit's business; the connect proves it)."""
    body = credential.split('.')[1]
    return json.loads(base64.urlsafe_b64decode(body + '=' * (-len(body) % 4)))


def token_participant_id(credential):
    """participantId from metadata and attributes; both must exist and agree."""
    c = claims(credential)
    meta = json.loads(c.get('metadata') or '{}')
    attrs = c.get('attributes') or {}
    if set(meta) != {'participantId'} or set(attrs) != {'participantId'}:
        raise Failed('credential metadata/attributes carry more or less than participantId')
    if meta['participantId'] != attrs['participantId']:
        raise Failed('metadata and attribute disagree')
    if (c.get('video') or {}).get('canUpdateOwnMetadata'):
        raise Failed('credential lets the client rewrite its own metadata')
    return meta['participantId']


def remote_participant_id(p):
    """How a client reads the id from a LiveKit participant (attributes, metadata fallback)."""
    pid = (getattr(p, 'attributes', None) or {}).get('participantId')
    if not pid and p.metadata:
        pid = json.loads(p.metadata).get('participantId')
    return pid


class Api:
    def __init__(self, base, bearer=None, participant=None):
        self.base, self.bearer, self.participant = base, bearer, participant

    def call(self, method, path, body=None, expected=(200,)):
        headers = {'Accept': 'application/json'}
        if self.bearer:
            headers['Authorization'] = 'Bearer ' + self.bearer
        if self.participant:
            headers['X-Participant-Token'] = self.participant
        data = None
        if body is not None or method in ('POST', 'PUT', 'PATCH'):
            headers['Content-Type'] = 'application/json'
            data = json.dumps(body if body is not None else {}).encode()
        request = urllib.request.Request(self.base + path, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, raw = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, raw = error.code, error.read()
        try:
            value = json.loads(raw) if raw else {}
        except ValueError:
            value = {}
        if status not in expected:
            code = (value.get('error') or {}).get('code') if isinstance(value, dict) else None
            raise Failed('%s %s answered %d %s' % (method, path.split('?')[0], status, code or ''))
        return status, value


class Journey:
    def __init__(self, context):
        self.base = context['base']
        self.account = context['account']
        self.run_id = context['runId']
        self.checks = {}
        self.details = {}
        self.rooms = []

    def check(self, name, ok, fatal=False):
        self.checks[name] = 'PASS' if ok else 'FAIL'
        print('CHECK', self.checks[name], name, flush=True)
        if not ok and fatal:
            raise Failed(name)

    def step(self, name, fn):
        try:
            return fn()
        except Failed as error:
            self.check(name + ': ' + str(error), False)
        except Exception as error:  # noqa: BLE001 - one failed group must not hide the rest
            self.check(name + ': ' + type(error).__name__, False)

    # -- setup -------------------------------------------------------------------------
    def setup(self):
        _, login = Api(self.base).call('POST', '/api/v1/auth/login',
                                       {'email': self.account['email'], 'password': self.account['password']})
        self.host = Api(self.base, bearer=login['access_token'])
        _, space = self.host.call('POST', '/api/v2/meeting-spaces', {
            'title': 'CF Meet acceptance ' + self.run_id, 'accessPolicy': 'PUBLIC'}, expected=(201,))
        self.space = space['id']
        _, conf = self.host.call('POST', '/api/v2/meeting-spaces/%s/conferences' % self.space, {}, expected=(201,))
        self.cid = conf['id']
        self.details.update(meetingSpaceId=self.space, conferenceId=self.cid)
        self.host.call('POST', '/api/v2/conferences/%s/start' % self.cid, {})
        deadline = time.monotonic() + 60
        status = None
        while time.monotonic() < deadline:
            status = self.host.call('GET', '/api/v2/conferences/%s' % self.cid)[1].get('status')
            if status == 'LIVE':
                break
            time.sleep(1)
        self.check('setup: own conference is LIVE', status == 'LIVE', fatal=True)

    def join(self, name=None, api=None, previous=None):
        body = {}
        if name:
            body['displayName'] = name
        if previous:
            body['previousSessionId'] = previous
        _, v = (api or Api(self.base)).call('POST', '/api/v2/conferences/%s/join' % self.cid, body)
        if v.get('rtcStatus') != 'ISSUED':
            raise Failed('media credential not issued (%s)' % v.get('rtcStatus'))
        return dict(pid=v['participant']['id'], session=v['participantSession']['id'],
                    credential=v['rtc']['credential'], url=v['rtc']['url'],
                    token=v.get('participantToken'), name=v['participant'].get('displayName'))

    def base_path(self):
        return '/api/v2/conferences/%s' % self.cid

    # -- LiveKit identity --------------------------------------------------------------
    def tokens(self):
        self.me = self.join(api=self.host)
        self.a = self.join(SAME_NAME)
        self.b = self.join(SAME_NAME)
        self.guest_a = Api(self.base, participant=self.a['token'])
        self.guest_b = Api(self.base, participant=self.b['token'])
        ids = [token_participant_id(j['credential']) for j in (self.me, self.a, self.b)]
        self.check('identity: every credential carries the canonical participantId of its join',
                   ids == [self.me['pid'], self.a['pid'], self.b['pid']])
        self.check('identity: two participants named %r get distinct participant ids' % SAME_NAME,
                   len(set(ids)) == 3 and self.a['name'] == self.b['name'] == SAME_NAME)
        ca, cb = claims(self.a['credential']), claims(self.b['credential'])
        self.check('identity: media identity and display name unchanged (ps_<session>, name)',
                   ca['sub'] == 'ps_' + self.a['session'].replace('-', '') and ca['name'] == SAME_NAME
                   and cb['sub'] != ca['sub'])
        host_meta = claims(self.me['credential']).get('metadata', '')
        self.check('identity: no account data in RTC metadata', '@' not in host_meta and len(host_meta) < 80)
        self.details.update(participantIds={'host': self.me['pid'], 'guestA': self.a['pid'], 'guestB': self.b['pid']})

    async def connect(self, joined):
        from livekit import rtc
        room = rtc.Room()
        await asyncio.wait_for(room.connect(joined['url'], joined['credential'], rtc.RoomOptions(auto_subscribe=True)), 25)
        self.rooms.append(room)
        return room

    async def wait_remote(self, room, count, timeout=20):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if len(room.remote_participants) >= count and all(
                    remote_participant_id(p) for p in room.remote_participants.values()):
                return
            await asyncio.sleep(.25)

    def tiles(self, room):
        """identity → (participantId, name) as a client's tile map would hold it."""
        return {p.identity: (remote_participant_id(p), p.name) for p in room.remote_participants.values()}

    async def media(self):
        self.host_room = await self.connect(self.me)
        self.room_a = await self.connect(self.a)
        self.room_b = await self.connect(self.b)
        await self.wait_remote(self.host_room, 2)
        tiles = self.tiles(self.host_room)
        ia, ib = claims(self.a['credential'])['sub'], claims(self.b['credential'])['sub']
        self.check('livekit: host sees both same-name guests with their own participantId',
                   tiles.get(ia) == (self.a['pid'], SAME_NAME) and tiles.get(ib) == (self.b['pid'], SAME_NAME))
        await self.wait_remote(self.room_a, 2)
        self.check('livekit: a guest sees the other guest and the host with the right ids',
                   self.tiles(self.room_a).get(ib, (None,))[0] == self.b['pid']
                   and self.me['pid'] in {v[0] for v in self.tiles(self.room_a).values()})
        self.tile_b = tiles.get(ib, (None,))[0]
        await asyncio.sleep(0)

    def targeting(self):
        bp = self.base_path()
        self.guest_b.call('POST', bp + '/hand-raise', {})
        hands = self.host.call('GET', bp + '/hand-raise')[1].get('hands', [])
        self.check('hands: the raised hand belongs to the tile the token names',
                   [h.get('participantId') for h in hands] == [self.tile_b] and self.tile_b == self.b['pid'])
        self.host.call('POST', bp + '/spotlights', {'participantId': self.tile_b})
        spot = self.guest_a.call('GET', bp + '/collaboration')[1].get('spotlights', [])
        self.check('spotlight: spotlighting the tile targets that participant only',
                   [s.get('participantId') for s in spot] == [self.b['pid']])
        self.host.call('DELETE', bp + '/spotlights/' + self.b['pid'])
        self.host.call('DELETE', bp + '/participants/%s/hand-raise' % self.b['pid'])

    async def reconnect(self):
        await self.room_b.disconnect()
        again = self.join(api=self.guest_b, previous=self.b['session'])
        self.check('reconnect: new session, same participantId in the new credential',
                   again['pid'] == self.b['pid'] and again['session'] != self.b['session']
                   and token_participant_id(again['credential']) == self.b['pid'])
        await self.connect(again)
        identity = claims(again['credential'])['sub']
        end = time.monotonic() + 20
        while time.monotonic() < end and identity not in self.tiles(self.host_room):
            await asyncio.sleep(.25)
        self.check('reconnect: host maps the new media identity to the same participant',
                   self.tiles(self.host_room).get(identity, (None,))[0] == self.b['pid'])
        self.b.update(again)

    # -- Q&A restore -------------------------------------------------------------------
    def qna(self):
        bp = self.base_path()
        _, q = self.guest_a.call('POST', bp + '/questions', {'body': 'Acceptance question ' + self.run_id},
                                 expected=(201,))
        qid = q['id']
        self.guest_b.call('POST', bp + '/questions/%s/vote' % qid, {})
        _, d = self.host.call('POST', bp + '/questions/%s/dismiss' % qid, {})
        hidden = all(x['id'] != qid for x in self.guest_b.call('GET', bp + '/questions')[1]['questions'])
        status, _ = self.guest_b.call('POST', bp + '/questions/%s/restore' % qid, {}, expected=(403,))
        self.check('qna: an attendee cannot restore (403 PERMISSION_DENIED)', status == 403)
        _, r = self.host.call('POST', bp + '/questions/%s/restore' % qid, {})
        rq = r['question']
        self.check('qna: dismissed unanswered question restores to OPEN, votes and body kept',
                   d['question']['status'] == 'DISMISSED' and hidden and r['changed'] is True
                   and rq['status'] == 'OPEN' and rq['upvoteCount'] == 1
                   and rq['body'] == 'Acceptance question ' + self.run_id
                   and rq['version'] == d['question']['version'] + 1)
        _, again = self.host.call('POST', bp + '/questions/%s/restore' % qid, {})
        self.check('qna: repeated restore is a no-op (changed: false, same version)',
                   again['changed'] is False and again['question']['version'] == rq['version'])
        back = [x for x in self.guest_b.call('GET', bp + '/questions')[1]['questions'] if x['id'] == qid]
        self.check('qna: restored question is visible to attendees again with their vote',
                   len(back) == 1 and back[0]['votedByMe'] is True)

        _, q2 = self.guest_b.call('POST', bp + '/questions', {'body': 'Answered then dismissed'}, expected=(201,))
        self.host.call('POST', bp + '/questions/%s/answer' % q2['id'], {'answer': 'Yes'})
        self.host.call('POST', bp + '/questions/%s/dismiss' % q2['id'], {})
        _, r2 = self.host.call('POST', bp + '/questions/%s/restore' % q2['id'], {})
        self.check('qna: answered question restores to ANSWERED with its answer',
                   r2['question']['status'] == 'ANSWERED' and r2['question']['answer'] == 'Yes')

        _, q3 = self.guest_a.call('POST', bp + '/questions', {'body': 'Anonymous acceptance', 'anonymous': True},
                                  expected=(201,))
        self.host.call('POST', bp + '/questions/%s/dismiss' % q3['id'], {})
        _, r3 = self.host.call('POST', bp + '/questions/%s/restore' % q3['id'], {})
        listed = self.host.call('GET', bp + '/questions')[1]
        self.check('qna: anonymous question stays anonymous after restore',
                   r3['question']['anonymous'] is True and self.a['pid'] not in json.dumps(r3)
                   and self.a['pid'] not in json.dumps([x for x in listed['questions'] if x['id'] == q3['id']]))

        _, q4 = self.guest_a.call('POST', bp + '/questions', {'body': 'Archived acceptance'}, expected=(201,))
        self.host.call('POST', bp + '/questions/%s/archive' % q4['id'], {})
        status, _ = self.host.call('POST', bp + '/questions/%s/restore' % q4['id'], {}, expected=(409,))
        self.check('qna: archived question cannot be restored (409)', status == 409)

        events = self.host.call('GET', bp + '/events?after=0')[1].get('events', [])
        restored = [e for e in events if e.get('eventType') == 'qna.question_restored']
        by_q = {e['payload']['questionId']: e['payload'] for e in restored}
        self.check('realtime: exactly one qna.question_restored per real restore, correct status',
                   len(restored) == 3 and by_q.get(qid, {}).get('status') == 'OPEN'
                   and by_q.get(q2['id'], {}).get('status') == 'ANSWERED'
                   and by_q.get(qid, {}).get('version') == rq['version'])
        anon = by_q.get(q3['id'], {})
        self.check('realtime: anonymous restored event carries no author',
                   anon.get('anonymous') is True and anon.get('participantId') is None
                   and anon.get('displayName') is None)

    # -- cleanup -----------------------------------------------------------------------
    async def close_rooms(self):
        for room in self.rooms:
            try:
                await asyncio.wait_for(room.disconnect(), 10)
            except Exception:  # noqa: BLE001 - cleanup must continue
                pass

    def cleanup(self):
        if getattr(self, 'cid', None):
            try:
                self.host.call('POST', self.base_path() + '/end', {}, expected=(200, 409))
                self.check('cleanup: conference ended', True)
            except Exception as error:  # noqa: BLE001
                self.check('cleanup: conference ended: ' + type(error).__name__, False)
        if getattr(self, 'space', None):
            try:
                version = self.host.call('GET', '/api/v2/meeting-spaces/' + self.space)[1]['version']
                self.host.call('POST', '/api/v2/meeting-spaces/%s/archive' % self.space, {'version': version})
                self.check('cleanup: meeting space archived', True)
            except Exception as error:  # noqa: BLE001
                self.check('cleanup: meeting space archived: ' + type(error).__name__, False)


async def main():
    context = json.loads((RESULT / 'context.private.json').read_text())
    journey = Journey(context)
    try:
        journey.step('setup', journey.setup)
        if journey.checks and all(v == 'PASS' for v in journey.checks.values()):
            journey.step('identity tokens', journey.tokens)
            try:
                await journey.media()
                journey.step('targeting', journey.targeting)
                await journey.reconnect()
            except Exception as error:  # noqa: BLE001
                journey.check('livekit media journey: ' + (str(error) if isinstance(error, Failed) else type(error).__name__), False)
            journey.step('qna', journey.qna)
    finally:
        await journey.close_rooms()
        journey.cleanup()
        report = dict(runId=journey.run_id, checks=journey.checks, details=journey.details,
                      verdict='PASS' if journey.checks and all(v == 'PASS' for v in journey.checks.values()) else 'FAIL')
        (RESULT / 'report.json').write_text(json.dumps(report, indent=2))
        print('MEETING_ACCEPTANCE_VERDICT', report['verdict'], flush=True)


if __name__ == '__main__':
    asyncio.run(main())
