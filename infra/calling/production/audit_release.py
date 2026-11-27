"""Owner-opted audit activation inside a verified release, without freezing main.

The caller keeps its existing main, exact-SHA, green-CI and production-lock gates.
Every activation refers to that just-verified deployment, even if main advances while
the rollout is running. A failed/uncertain stage is never retried automatically by a
later unrelated merge: a reviewed fix must increase that stage's version.
"""
import datetime as dt
import json
import os

import production as p
import audit_operations as audit
import kt_witness_operations as witness
import meeting_recording_operations as recording
import send_policy_operations as policy

VERSIONS = {'witness': 1, 'logs': 1, 'recording': 2, 'send-policy': 1}


def release(cr, sha, run_id, after_release=None):
    names = cr.container_names()
    resume = cr.SERVICES['messaging']['container'] in names and policy.enabled(cr)
    paused = False
    def before_sources():
        nonlocal paused
        if resume:
            policy.pause_for_release(cr, sha, run_id + '-audit-send-pause')
            paused = True
    cr.release_all(sha, run_id, before_source_swap=before_sources)
    if paused:
        # Only a successful, verified image rollout may re-bootstrap the fast path.
        # Fresh preparation still requires all three protocol/generation ACKs.
        policy.activate(cr, sha, run_id + '-audit-send-prepare', 'prepare')
        policy.activate(cr, sha, run_id + '-audit-send-enable', 'enable')
    if after_release:
        after_release()
    if os.environ.get('CFMEET_AUDIT_ACTIVATIONS') == 'enabled':
        run(cr, sha, run_id)
        cr.verify(sha, run_id)
    elif paused:
        cr.verify(sha, run_id)


def logs_ready(cr):
    root = p.ROOT.resolve()
    files = [audit.LOG_UNIT, root/'log-collector/config.json', root/'logs/services/health.json']
    if any(path.is_symlink() or not path.is_file() for path in files):
        return False
    try:
        unit, config, health = files[0].read_text(), json.loads(files[1].read_text()), json.loads(files[2].read_text())
        running = set(p.run('docker', 'ps', '--format', '{{.Names}}').splitlines())
        expected = audit.log_containers(cr.SERVICES, running)
        age = dt.datetime.now(dt.timezone.utc) - dt.datetime.fromisoformat(health['at'])
        return ('# CF Meet owned service log collector' in unit and 'TasksMax=2048' in unit
                and config['directory'] == str(root/'logs/services')
                and set(config['containers']) == set(expected) and bool(expected)
                and health['threadsAlive'] == len(expected) and health['containers'] == len(expected)
                and not health.get('failures') and 0 <= age.total_seconds() < 30
                and p.run('systemctl', 'show', audit.LOG_UNIT.name, '--property=ActiveState', '--value') == 'active')
    except (OSError, ValueError, KeyError, TypeError):
        return False


def ensure_witness(cr, sha, run_id):
    env = cr.env_of('messaging')
    if (env.get('MESSAGING_KT_WITNESSES_FILE') == witness.TARGET
            and env.get('MESSAGING_KT_WITNESS_QUORUM') == '1'
            and witness.NAME in cr.container_names()
            and p.inspect(witness.NAME)['State']['Running']):
        witness.prove_current_head(cr)
    else:
        witness.activate(cr, sha, run_id + '-audit-witness', 'operator-managed')


def ensure_logs(cr, sha, run_id):
    if not logs_ready(cr):
        audit.activate_service_logs(cr, sha, run_id + '-audit-logs', 'retain-14-days')
    if not logs_ready(cr):
        raise RuntimeError('Owned log streams must be healthy and fully covered')


def ensure_recording(cr, sha, run_id):
    status = recording.facts(cr)
    if status.get('recordingEnabled') is True:
        if status.get('recordingStorageConfigured') is not True:
            raise RuntimeError('Active recording has no configured storage; preserve it for diagnosis')
        if not audit.recording_acceptance_proven():
            raise RuntimeError('Active recording has no complete controlled acceptance receipt; preserve it for review')
        if not recording.media_path.prepared():
            raise RuntimeError('Active recording media ingress has drifted; preserve it for review')
        recording.ensure_callback(cr, run_id + '-audit-recording-check')
        return
    recording.activate(cr, sha, run_id + '-audit-recording-prepare', 'prepare')
    recording.activate(cr, sha, run_id + '-audit-recording-enable', 'enable-30-days')
    if not audit.recording_acceptance_proven():
        raise RuntimeError('Recording acceptance must leave its complete protected receipt')


def ensure_send_policy(cr, sha, run_id):
    if policy.enabled(cr):
        envs = {name: cr.env_of(name) for name in policy.OWNERS}
        states = {name: policy.facts(cr, name) for name in policy.OWNERS}
        try:
            policy.validate_generation(envs, states)
            if states['messaging'].get('sendPolicyLocalEnabled') is not True:
                raise RuntimeError('Fast policy configuration and readiness disagree')
        except RuntimeError:
            policy.pause_for_release(cr, sha, run_id + '-audit-send-invalid')
            raise
        return
    policy.activate(cr, sha, run_id + '-audit-send-prepare', 'prepare')
    policy.activate(cr, sha, run_id + '-audit-send-enable', 'enable')


def save(path, state):
    temporary = path.with_suffix('.tmp')
    if path.is_symlink() or temporary.is_symlink():
        raise RuntimeError('Unknown audit campaign state path; preserved')
    p.write_private(temporary, json.dumps(state, sort_keys=True, indent=1))
    os.replace(temporary, path)


def run(cr, sha, run_id):
    cr.require_release(sha)
    path = p.ROOT/'audit-activation-campaign.json'
    if path.is_symlink() or (path.exists() and path.stat().st_size > 65536):
        raise RuntimeError('Unknown audit campaign state; preserved')
    state = json.loads(path.read_text()) if path.exists() else {'completed': {}}
    if not isinstance(state, dict) or not isinstance(state.get('completed'), dict):
        raise RuntimeError('Invalid audit campaign state; preserved')
    failed = state.get('failure') or state.get('running')
    if failed and VERSIONS.get(failed.get('stage'), 0) <= failed.get('version', 0):
        cr.emit('auditActivationCampaign', dict(result='BLOCKED', stage=failed.get('stage'),
                reason='reviewed stage fix and version increment required; no automatic retry'))
        return
    state.pop('failure', None)
    state.pop('running', None)
    for stage, action in (('witness', ensure_witness), ('logs', ensure_logs),
                          ('recording', ensure_recording), ('send-policy', ensure_send_policy)):
        progress = dict(stage=stage, version=VERSIONS[stage], sha=sha, runId=run_id)
        state['running'] = progress
        save(path, state)
        try:
            action(cr, sha, run_id)
        except Exception as error:
            state['failure'] = dict(progress, errorClass=type(error).__name__)
            state.pop('running', None)
            save(path, state)
            cr.emit('auditActivationCampaign', dict(result='FAIL', stage=stage, errorClass=type(error).__name__))
            raise RuntimeError('Audit activation failed at ' + stage + '; protected state retained') from None
        state['completed'][stage] = progress
        state.pop('running', None)
        save(path, state)
        cr.emit('auditActivationStage', dict(stage=stage, result='PASS', parentRunId=run_id))
    cr.emit('auditActivationCampaign', dict(result='PASS', sha=sha, stages=list(VERSIONS),
            independentOperator=False, recordingRetentionDays=30, logRetentionDays=14))
