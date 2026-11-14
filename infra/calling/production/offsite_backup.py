"""Encrypted off-host backup copy and its verification record (VPS side).

Dispatched only by .github/workflows/cfmeet-offsite-backup.yml, which holds the production
lock. Runs on the VPS as root:

    python3 offsite_backup.py encrypt <main sha> <run id> <recipients file> <age binary>
    python3 offsite_backup.py record  <backup run id> <evidence file>

encrypt  runs the coordinated `backup` (pg_dump of every CF Meet database plus the Meeting
         database, sha256, pg_restore --list, same-host restore tests), then encrypts every
         dump to the owner's age *public* recipients into <backup>/offsite/. Only `*.age`,
         their sha256 and a manifest without row data leave the host; the workflow streams
         them to the off-host store (GitHub Actions artifact storage) and restores them on a
         separate, network-isolated runner that holds the decryption identity. The plaintext
         dumps stay on the VPS (0600) exactly as before.
record   receives the workflow's evidence after the off-host copy and the restore drill:
         artifact id and digest, the sha256 of every encrypted file as seen off-host, and the
         drill verdict per database. It re-hashes its own encrypted files, and writes
         <backup>/offsite.json only if everything matches. `org-cutover-import` requires it.

The VPS never holds a private key: recipients files containing AGE-SECRET-KEY or a PRIVATE
KEY are refused, as in infra/deployment/backup.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import coordinated_release as cr
import production as p

RECIPIENT_RE = re.compile(r'age1[02-9ac-hj-np-z]{58}')
OFFSITE_STATUS = 'OFF_HOST_COPY_VERIFIED'
DRILL_REQUIRED = ('meeting-service', 'organizations', 'platform-access')
KEEP_RUNS = 7
KEEP_DAYS = 14


def recipients(path):
    text = Path(path).read_text()
    if 'AGE-SECRET-KEY' in text.upper() or 'PRIVATE KEY' in text.upper():
        raise RuntimeError('Recipients file contains private key material; refusing')
    lines = [line.strip() for line in text.splitlines() if line.strip() and not line.startswith('#')]
    if not lines or not all(RECIPIENT_RE.fullmatch(line) for line in lines):
        raise RuntimeError('Recipients file must contain only age1... public recipients')
    return lines


def encrypt(sha, run_id, recipients_file, age_binary):
    keys = recipients(recipients_file)
    version = subprocess.run([age_binary, '--version'], capture_output=True, text=True)
    if version.returncode:
        raise RuntimeError('age binary does not run on this host')
    manifest = cr.backup(sha, run_id)
    folder = cr.BACKUPS / run_id
    out = folder / 'offsite'
    out.mkdir(mode=0o700)
    files = []
    for item in manifest['databases']:
        source = folder / item['file']
        target = out / (item['file'] + '.age')
        args = [age_binary, '--encrypt', '--output', str(target)]
        for key in keys:
            args += ['--recipient', key]
        proc = subprocess.run(args + [str(source)], capture_output=True, text=True)
        if proc.returncode or not target.exists():
            target.unlink(missing_ok=True)
            raise RuntimeError('Encryption failed: ' + item['label'])
        os.chmod(target, 0o600)
        with open(target, 'rb') as f:
            if not f.read(64).startswith(b'age-encryption.org/v1'):
                raise RuntimeError('Encrypted file has no age header: ' + item['label'])
        files.append(dict(label=item['label'], file=target.name, bytes=target.stat().st_size,
                          sha256=cr.sha256(target), dumpSha256=item['sha256'], postgresMajor=item['postgresMajor'],
                          histories=item['histories']))
    public = dict(runId=run_id, sha=sha, createdAt=manifest['createdAt'], recipients=keys, files=files,
                  note='Encrypted to the owner public recipients; no row data, no credentials.')
    p.write_private(out / 'offsite-manifest.json', json.dumps(public, indent=1, sort_keys=True))
    cr.emit('offsiteEncrypted', {'runId': run_id, 'files': [{k: f[k] for k in ('label', 'file', 'bytes', 'sha256')}
                                                            for f in files]})
    cr.emit('offsitePruned', prune())
    return public


def prune(keep=KEEP_RUNS, days=KEEP_DAYS, now=None):
    """Local retention for the daily off-host runs only (folders holding offsite/): keep the
    newest `keep`, delete the rest once older than `days`. Release and activation backups are
    never touched; off-host copies follow the artifact retention."""
    import shutil
    import time
    now = now or time.time()
    runs = sorted((d for d in cr.BACKUPS.iterdir() if (d / 'offsite' / 'offsite-manifest.json').exists()),
                  key=lambda d: d.stat().st_mtime, reverse=True)
    removed = []
    for folder in runs[keep:]:
        if now - folder.stat().st_mtime > days * 86400:
            shutil.rmtree(folder)
            removed.append(folder.name)
    return removed


def record(run_id, evidence_file):
    if not cr.RUN_RE.fullmatch(run_id):
        raise RuntimeError('Invalid backup run id')
    folder = cr.BACKUPS / run_id
    public = json.loads((folder / 'offsite' / 'offsite-manifest.json').read_text())
    evidence = json.loads(Path(evidence_file).read_text())
    if evidence.get('runId') != run_id:
        raise RuntimeError('Evidence is for another backup run')
    copied = {f['file']: f['sha256'] for f in evidence.get('offHostFiles', [])}
    for f in public['files']:
        local = cr.sha256(folder / 'offsite' / f['file'])
        if local != f['sha256'] or copied.get(f['file']) != local:
            raise RuntimeError('Off-host copy does not match the encrypted file: ' + f['label'])
    drills = {d['label']: d for d in evidence.get('drills', [])}
    labels = [f['label'] for f in public['files']]
    failed = [label for label in labels if (drills.get(label) or {}).get('result') != 'PASS']
    if failed:
        raise RuntimeError('Restore drill did not pass for: ' + ', '.join(failed))
    for label in labels:
        expected = next(f['histories'] for f in public['files'] if f['label'] == label)
        if drills[label].get('histories') != expected:
            raise RuntimeError('Restore drill histories differ from the backup manifest: ' + label)
    if not evidence.get('artifactId') or not evidence.get('runUrl'):
        raise RuntimeError('Evidence lacks the off-host artifact reference')
    doc = dict(runId=run_id, status=OFFSITE_STATUS, at=cr.now(), store=evidence.get('store', 'github-actions-artifact'),
               artifactId=evidence['artifactId'], artifactDigest=evidence.get('artifactDigest'),
               retentionDays=evidence.get('retentionDays'), runUrl=evidence['runUrl'],
               files={f['file']: f['sha256'] for f in public['files']},
               drills={label: {k: drills[label].get(k) for k in ('result', 'tables', 'seconds')} for label in labels})
    p.write_private(folder / 'offsite.json', json.dumps(doc, indent=1, sort_keys=True))
    cr.emit('offsiteRecorded', {k: v for k, v in doc.items() if k != 'files'})
    return doc


def verified(run_id, required=DRILL_REQUIRED):
    """The off-host record of one backup run, if it is complete for `required`."""
    path = cr.BACKUPS / run_id / 'offsite.json'
    if not path.exists():
        return None
    doc = json.loads(path.read_text())
    if doc.get('status') != OFFSITE_STATUS:
        return None
    if any((doc.get('drills') or {}).get(label, {}).get('result') != 'PASS' for label in required):
        return None
    return doc


def main(argv):
    try:
        if argv[:1] == ['encrypt'] and len(argv) == 5:
            if not cr.SHA_RE.fullmatch(argv[1]) or not cr.RUN_RE.fullmatch(argv[2]):
                raise RuntimeError('Invalid SHA or run id')
            encrypt(*argv[1:])
        elif argv[:1] == ['record'] and len(argv) == 3:
            record(*argv[1:])
        else:
            raise RuntimeError('usage: offsite_backup.py encrypt <sha> <run> <recipients> <age> | record <run> <evidence>')
    except Exception as exc:
        print('OFFSITE_BACKUP_FAILED:', type(exc).__name__,
              cr.scrub(str(exc)) if isinstance(exc, (AssertionError, RuntimeError)) else '', flush=True)
        return 1
    return 0


if __name__ == '__main__':
    os.umask(0o077)
    sys.exit(main(sys.argv[1:]))
