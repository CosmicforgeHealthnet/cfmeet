"""Production web Messaging journey fixtures (messaging-web-verify).

The browser journey itself runs on the workflow runner against the real website
(apps/web/tests/messaging-production.browser.mjs). This VPS step only prepares what a real
user would already have: three fresh, email-verified accounts (app_acceptance: the
verification code of each newly created controlled account is read from Redis), and two
Hub contact connections (A-B and A-C), made through the public API. Account C never opens
Messages here: it is the contact who has not set up messaging yet.

Secrets stay protected: the accounts file is written 0600 under the VPS secret store and
read once by the runner over SSH; nothing secret is printed.
"""
import json
import re
import urllib.error
import urllib.request
import uuid

import production as p

API = 'https://api.cfmeet.com'


def request(path, token, method='GET', body=None, key=None):
    headers = {'Accept': 'application/json', 'Authorization': 'Bearer ' + token}
    data = None
    if body is not None:
        headers['Content-Type'] = 'application/json'
        data = json.dumps(body).encode()
    if key:
        headers['Idempotency-Key'] = key
    req = urllib.request.Request(API + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as error:
        raise RuntimeError('%s %s answered %d' % (method, re.sub(r'[0-9a-f-]{36}', '{id}', path), error.code))


def hub(token, path, method='GET', body=None, key=None):
    return request('/api/v2/communications' + path, token, method, body, key)


def accounts_file(run_id):
    assert re.fullmatch(r'[0-9]{1,20}', run_id)
    return p.SECRETS / ('cfmeet-web-journey-' + run_id + '.json')


def prepare(run_id):
    import app_acceptance
    people = app_acceptance.accounts('web-journey-' + run_id, count=3)
    for person in people:
        login = app_acceptance.api('login', {'email': person['email'], 'password': person['password']})
        person['token'] = login['access_token']
        person['cfMeetId'] = hub(person['token'], '/me')['cfMeetId']
    a, b, c = people
    for other in (b, c):
        made = hub(a['token'], '/contact-requests', 'POST', {'cfMeetId': other['cfMeetId']}, key=str(uuid.uuid4()))
        hub(other['token'], '/contact-requests/' + made['requestId'] + '/accept', 'POST',
            {'expectedVersion': made['version']}, key=str(uuid.uuid4()))
    # The label each account's new-chat picker shows for its contacts (alias, else the
    # display name, else the CF Meet ID: apps/web/src/lib/communications/comms-core.ts).
    for person in people:
        contacts = hub(person['token'], '/contacts').get('data', [])
        person['labels'] = {x['cfMeetId']: ((x.get('alias') or '').strip() or (x.get('displayName') or '').strip()
                                            or x['cfMeetId'])
                            for x in contacts if x.get('relationship') == 'CONNECTED'}
    assert b['cfMeetId'] in a['labels'] and c['cfMeetId'] in a['labels'], 'contacts are connected'
    assert a['cfMeetId'] in b['labels'] and a['cfMeetId'] in c['labels'], 'connections are mutual'
    # The API sign-ins used for the setup are ended; the browser signs in on its own.
    for person in people:
        try:
            request('/api/v1/auth/logout', person.pop('token'), 'POST', {})
        except RuntimeError:
            pass
    fixtures = [{k: person[k] for k in ('email', 'password', 'cfMeetId', 'labels')} for person in people]
    p.write_private(accounts_file(run_id), json.dumps(fixtures))
    print(json.dumps({'webJourneyFixtures': {'accounts': 3, 'emailVerified': True,
                                             'contactConnections': ['A-B', 'A-C'],
                                             'unenrolledContact': 'C (never opened Messages)',
                                             'file': accounts_file(run_id).name}}), flush=True)
