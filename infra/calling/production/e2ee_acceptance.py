"""Production multi-device personal E2EE acceptance (messaging-e2ee-verify).

Runs the real-OpenMLS acceptance client (certification/src/bin/cfmeet-e2ee-acceptance.rs,
built static by the workflow into ./bin/) against https://api.cfmeet.com with two fresh,
email-verified accounts, then proves from Messaging's own database (read-only) that none of
the exchanged phrases was ever stored and that every message of the conversation is opaque
PRIVATE_E2EE ciphertext. Never emits tokens, passwords, keys or account addresses.

Only runs when production E2EE is on under the owner's release record
(coordinated activate-personal-e2ee); otherwise it reports BLOCKED and fails.
"""
import json
import os
import subprocess
from pathlib import Path

import production as p

BINARY = Path(__file__).resolve().parent / 'bin' / 'cfmeet-e2ee-acceptance'
RELEASE_TARGET = '/run/messaging/e2ee-production-release.json'
RELEASE_SOURCE = p.ROOT / 'messaging' / 'e2ee-production-release.json'
REPORT_PREFIX = 'E2EE_ACCEPTANCE_REPORT '


def release_status(env):
    """The actual release decision as recorded (never a certification claim)."""
    if env.get('MESSAGING_E2EE_DEVICES_ENABLED') != 'true' or env.get('MESSAGING_E2EE_PRODUCTION_RELEASE_FILE') != RELEASE_TARGET:
        return None
    record = json.loads(RELEASE_SOURCE.read_text())
    return {k: record.get(k) for k in ('decisionId', 'decidedAt', 'independentAssessment', 'acceptedResidualRisks')}


def parse_report(output):
    for line in output.splitlines():
        if line.startswith(REPORT_PREFIX):
            return json.loads(line[len(REPORT_PREFIX):])
    return {'verdict': 'FAIL', 'failure': 'client produced no report'}


def plaintext_sql(conversation, phrases):
    """Counts only: messages of the conversation that are not opaque PRIVATE_E2EE ciphertext,
    and messages whose stored bytes contain any exchanged phrase (both must be 0)."""
    assert len(conversation) == 36 and all(c in '0123456789abcdef-' for c in conversation)
    checks = ' OR '.join("position(convert_to('%s', 'UTF8') in coalesce(mls_ciphertext, ''::bytea)) > 0"
                         % ph.replace("'", "''") for ph in phrases) or 'false'
    return ("SELECT (SELECT count(*) FROM messaging_messages WHERE conversation_id = '%s' AND message_kind = 'APPLICATION' "
            "AND (security_profile <> 'PRIVATE_E2EE' OR mls_ciphertext IS NULL OR managed_sealed IS NOT NULL)) || '|' || "
            "(SELECT count(*) FROM messaging_messages WHERE conversation_id = '%s' AND (%s)) || '|' || "
            "(SELECT count(*) FROM messaging_messages WHERE conversation_id = '%s' AND message_kind = 'APPLICATION')"
            % (conversation, conversation, checks, conversation))


def execute(sha, run_id):
    import app_acceptance
    import communications_release as release
    import coordinated_release as cr
    env = p.env_of(p.inspect('cfmeet-messaging'))
    assert env.get('CF_RUNTIME_ENVIRONMENT') == 'production'
    status = release_status(env)
    if status is None:
        print(json.dumps({'verdict': 'BLOCKED', 'reason': 'production E2EE devices are not enabled under an owner release record'}))
        raise RuntimeError('Personal E2EE is not activated in production (activate-personal-e2ee)')
    assert BINARY.is_file(), 'The workflow did not provide the acceptance client'
    folder = p.ROOT / 'messaging' / ('e2ee-acceptance-' + run_id)
    folder.mkdir(mode=0o700, parents=True, exist_ok=False)
    accounts = [{'email': a['email'], 'password': a['password']}
                for a in app_acceptance.accounts('e2ee-' + run_id, count=2)]
    p.write_private(folder / 'accounts.private.json', json.dumps(accounts))
    os.chown(folder, 10001, 10001)
    os.chown(folder / 'accounts.private.json', 10001, 10001)
    BINARY.chmod(0o555)
    # A static binary; the image only provides an unprivileged, read-only sandbox.
    result = subprocess.run(['docker', 'run', '--rm', '--name', 'cfmeet-e2ee-acceptance-' + run_id,
        '--network', 'host', '--user', '10001:10001', '--read-only', '--memory', '256m', '--cpus', '1',
        '--mount', f'type=bind,src={BINARY},dst=/acceptance,readonly',
        '--mount', f'type=bind,src={folder}/accounts.private.json,dst=/accounts.json,readonly',
        '-e', 'CFMEET_BASE_URL=https://api.cfmeet.com', '-e', 'CFMEET_E2EE_ACCOUNTS=/accounts.json',
        '--entrypoint', '/acceptance', 'python:3.12-slim'], capture_output=True, text=True, timeout=600)
    report = parse_report(result.stdout)
    report.update(requestedImageSha=sha, runId=run_id, environment='production', e2eeRelease=status,
                  independentCryptographicCertification='NOT_CLAIMED')
    if report.get('verdict') == 'PASS' and report.get('conversationId'):
        row = release.db_sql(cr.SERVICES['messaging']['db'], 'SET default_transaction_read_only = on;\n'
                             + plaintext_sql(report['conversationId'], report.get('phrases', [])))
        not_opaque, containing, total = (int(x) for x in row.strip().splitlines()[-1].split('|'))
        report['serverStorage'] = {'applicationMessages': total, 'notOpaqueCiphertext': not_opaque,
                                   'containingAnExchangedPhrase': containing}
        if not_opaque or containing or total < len(report.get('phrases', [])):
            report['verdict'] = 'FAIL'
            report['failure'] = 'server storage is not opaque ciphertext only'
        else:
            report['checks']['server_stores_only_opaque_ciphertext_no_plaintext'] = 'PASS'
    # Phrases are random test strings; keep them only in the protected receipt.
    receipt = dict(report)
    p.write_private(folder / 'receipt.json', json.dumps(receipt, sort_keys=True))
    report['phrases'] = len(report.get('phrases', []))
    print(json.dumps(report, sort_keys=True))
    assert report.get('verdict') == 'PASS', 'Multi-device E2EE production acceptance failed; protected evidence retained'
    return report
