"""Dedicated native two-account Messaging acceptance, with truthful activation gates.

The current production guard blocks enrollment/delivery. Verify the real denial paths
and publish BLOCKED, never manufacture a successful encrypted-send receipt. Real MLS
delivery is independently exercised by certification/tests/personal_communications.rs
in an isolated TEST runtime, plus the existing Messaging real-service suites.
"""
import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.error
import urllib.request
import uuid
from communications_acceptance import Journey


class MessagingJourney(Journey):
    def request(self, owner, path, method='GET', body=None, token=None, actor=None, expected=(200,), key=None):
        headers = {'Content-Type': 'application/json', 'Idempotency-Key': key or str(uuid.uuid4())}
        if token: headers['Authorization'] = 'Bearer ' + token
        if actor: headers['CF-Acting-Principal'] = actor
        req = urllib.request.Request(self.c['urls'][owner] + path, method=method, headers=headers,
            data=json.dumps(body).encode() if body is not None else None)
        try:
            with urllib.request.urlopen(req, timeout=25) as response:
                status, raw = response.status, response.read()
        except urllib.error.HTTPError as error: status, raw = error.code, error.read()
        try: value = json.loads(raw)
        except (ValueError, UnicodeDecodeError): value = {}
        error = value.get('error', {})
        self.report.setdefault('responses', []).append({'method': method, 'route': path.split('?')[0],
            'owner': owner, 'status': status, 'code': error.get('code') if isinstance(error, dict) else None,
            'at': datetime.datetime.now(datetime.timezone.utc).isoformat()})
        assert status in expected, 'Unexpected Messaging acceptance response'
        return value

    def native(self, person, path, method='GET', body=None, expected=(200,)):
        return self.request('meeting-service', '/api/v2/messaging/' + path, method, body,
                            person['session'], expected=expected)

    def remove_fixture_contact(self, a, b):
        self.hub(a, '/profiles/' + b['cfMeetId'] + '/block', 'DELETE')
        for contact in self.hub(a, '/contacts')['data']:
            if contact['cfMeetId'] == b['cfMeetId']:
                self.hub(a, '/contacts/' + contact['contactId'], 'DELETE')

    def run(self):
        swagger = self.request('meeting-service', '/api-docs/messaging.json')
        operations = [operation for item in swagger['paths'].values() for method, operation in item.items()
                      if method in ('get', 'post', 'put', 'patch', 'delete', 'options', 'head')]
        assert len(operations) == 80
        assert sum('x-cfmeet-native-path' in operation for operation in operations) == 71
        assert swagger['info']['version'] == '1.14.0'
        assert not any(path.startswith('/internal/') for path in swagger['paths'])
        self.report['servedSwagger'] = {'version': swagger['info']['version'], 'operations': len(operations), 'nativeBindings': 71}
        self.done('served_consumer_swagger_matches_native_backend_contract')
        for path in ('/metrics', '/internal/v1/communications/policy/check', '/v1/messaging/policy'):
            self.request('meeting-service', path, expected=(404,))
        self.done('private_admin_and_metrics_not_publicly_routed')
        self.request('meeting-service', '/api/v2/messaging/me', expected=(401,))
        self.done('anonymous_native_messaging_denied')
        a, b = self.accounts()
        self.report['testPrincipals'] = [a['principalId'], b['principalId']]
        self.report['scope'] = {k: v for k, v in self.c['scope'].items() if k != 'token'}
        for person in (a, b):
            cap = self.hub(person, '/me/capabilities')
            assert cap['messaging']['available'] is False and cap['messaging']['securityReady'] is False
        self.done('capabilities_do_not_claim_unavailable_private_messaging')
        self.remove_fixture_contact(a, b); self.remove_fixture_contact(b, a)
        self.direct(a, b, expected=(403,), profile='PRIVATE_E2EE')
        self.done('unconnected_direct_api_cannot_bypass_communications_policy')
        self.connect(a, b)
        self.done('native_contact_connection_and_idempotent_acceptance')
        try:
            for person in (a, b):
                denied = self.native(person, 'devices/registration-challenges', 'POST', {}, expected=(503,))
                assert denied.get('error'), 'Enrollment must remain blocked, not return a challenge'
            self.done('production_e2ee_device_guard_remains_active')
            for profile in ('PRIVATE_E2EE', None):
                body = {'contextClass': 'DIRECT', 'counterpartCfMeetId': b['cfMeetId']}
                if profile: body['securityProfile'] = profile
                denied = self.native(a, 'relay/v1/conversations', 'POST', body, expected=(409,))
                assert denied['error']['code'] == 'E2EE_NOT_AVAILABLE'
            self.done('private_and_default_conversation_creation_have_no_plaintext_fallback')
            self.direct(a, b, expected=(422,), profile='MANAGED')
            self.done('managed_not_activated_as_personal_e2ee_substitute')
            self.native(a, 'relay/v1/messaging/policy', expected=(404,))
            self.native(a, 'relay/v1/messaging/audit', expected=(404,))
            self.done('native_consumer_relay_excludes_policy_admin_and_audit')
            for person in (a, b):
                listing = self.native(person, 'relay/v1/conversations')
                assert not listing['conversations'], 'Isolated fixture should have no conversation created by a failed send'
            self.done('failed_activation_does_not_create_managed_or_partial_conversations')
            self.hub(b, '/profiles/' + a['cfMeetId'] + '/block', 'PUT', {})
            self.direct(a, b, expected=(403,), profile='PRIVATE_E2EE')
            self.done('blocked_direct_api_fails_at_messaging_owner')
        finally:
            self.remove_fixture_contact(a, b); self.remove_fixture_contact(b, a)
            for person in (a, b):
                self.request('meeting-service', '/api/v1/auth/logout', 'POST',
                    {'refresh_token': person['refresh']}, person['session'])
                self.native(person, 'me', expected=(401,))
            self.done('controlled_relationship_and_session_cleanup')
        self.report['activationVerdict'] = 'BLOCKED_EXTERNAL_DEPENDENCY'
        self.report['requiredEvidence'] = ['independent cryptographic assessment',
            'owner signoff on the exact reviewed production E2EE configuration', 'reviewed production key custody and transparency anti-equivocation evidence']
        self.report['encryptedProductionDelivery'] = 'NOT_RUN_ENROLLMENT_BLOCKED'
        self.report['productionMediaAttachmentsOfflineMultiDevice'] = 'NOT_RUN_REQUIRES_APPROVED_E2EE'
        self.report['physicalDevicePush'] = 'NOT_RUN_NO_PROVIDER_CREDENTIALS_OR_DEVICE_EVIDENCE'
        self.report['deviceIds'] = []
        self.report['messageIds'] = []
        self.report['conversationIds'] = []
        return self.report


def execute(sha, run_id):
    import production as p
    import communications_release as release
    import app_acceptance
    env = p.env_of(p.inspect('cfmeet-messaging'))
    assert env.get('CF_RUNTIME_ENVIRONMENT') == 'production'
    assert env.get('MESSAGING_E2EE_DEVICES_ENABLED', 'false') == 'false'
    scope = release.prod_scope()
    scope['token'] = release.credential(scope)
    folder = p.ROOT / 'messaging' / ('acceptance-' + run_id)
    folder.mkdir(mode=0o700, parents=True, exist_ok=False)
    context = {'accounts': app_acceptance.accounts()[:2], 'scope': scope,
        'urls': {'meeting-service': 'https://api.cfmeet.com', 'messaging': 'https://api.cfmeet.com:8444'}}
    p.write_private(folder / 'context.private.json', json.dumps(context))
    os.chown(folder, 10001, 10001); os.chown(folder / 'context.private.json', 10001, 10001)
    source = Path(__file__).resolve().parent
    result = subprocess.run(['docker', 'run', '--rm', '--network', p.NET, '--memory', '128m', '--cpus', '1',
        '--user', '10001:10001', '--mount', f'type=bind,src={folder},dst=/result',
        '--mount', f'type=bind,src={source},dst=/scripts,readonly', 'python:3.12-slim',
        'python', '/scripts/messaging_acceptance.py'], capture_output=True, timeout=240)
    report = json.loads((folder / 'report.json').read_text()) if (folder / 'report.json').is_file() else {'failure': 'client produced no evidence'}
    report.update(workflowSha=source.parent.name, requestedImageSha=sha, runId=run_id, environment='production')
    report['images'] = {name: p.inspect(container)['Image'] for name, container in dict(release.OWNERS, communications='cfmeet-communications').items()}
    p.write_private(folder / 'receipt.json', json.dumps(report, sort_keys=True))
    print(json.dumps(report, sort_keys=True))
    assert result.returncode == 0, 'Messaging production acceptance failed; protected evidence retained'
    return report


if __name__ == '__main__':
    journey = MessagingJourney(json.loads(Path('/result/context.private.json').read_text()))
    try:
        journey.run()
    except Exception as error:
        journey.report['failureClass'] = type(error).__name__
        Path('/result/report.json').write_text(json.dumps(journey.report, sort_keys=True))
        sys.exit(1)
    Path('/result/report.json').write_text(json.dumps(journey.report, sort_keys=True))
