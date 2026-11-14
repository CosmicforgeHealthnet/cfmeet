"""Restore drill of an encrypted off-host backup copy (GitHub runner, never the VPS).

    python3 offsite_drill.py <downloaded artifact dir> <age identity file> <evidence out.json>

For every file of `offsite-manifest.json`: the sha256 of the encrypted file as downloaded
from the off-host store must equal the manifest; it is decrypted with the owner's drill
identity, the decrypted dump's sha256 must equal the dump checksum recorded on the VPS, and it
is restored with pg_restore into a fresh, network-less PostgreSQL of the recorded major version.
Every migration history must have exactly the row count recorded at backup time. Plaintext is
deleted as soon as each restore finishes. The evidence holds hashes, counts and timings only.
Exit 0 only when every database passes.
"""
import hashlib
import json
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path

TABLE = re.compile(r'[a-z_][a-z0-9_]{0,62}')


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def run(*args, stdin=None):
    out = subprocess.run(args, stdin=stdin, capture_output=True, text=True)
    if out.returncode:
        raise RuntimeError('%s failed: %s' % (args[0] if args[0] != 'docker' else ' '.join(args[:2]), out.stderr[-500:]))
    return out.stdout


def restore(dump, major, histories):
    name = 'cfmeet-offsite-drill-' + uuid.uuid4().hex[:12]
    run('docker', 'run', '-d', '--name', name, '--network', 'none', '-e', 'POSTGRES_HOST_AUTH_METHOD=trust',
        'postgres:%d-alpine' % major)
    try:
        for _ in range(90):
            if subprocess.run(['docker', 'exec', name, 'pg_isready', '-q', '-U', 'postgres']).returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError('drill PostgreSQL did not start')
        run('docker', 'exec', name, 'createdb', '-U', 'postgres', 'drill')
        started = time.monotonic()
        with open(dump, 'rb') as f:
            run('docker', 'exec', '-i', name, 'pg_restore', '-U', 'postgres', '-d', 'drill', '--no-owner',
                '--no-privileges', '--exit-on-error', stdin=f)
        seconds = round(time.monotonic() - started, 1)
        sql = lambda q: run('docker', 'exec', name, 'psql', '-XAtq', '-U', 'postgres', '-d', 'drill', '-c', q).strip()
        restored = {}
        for table in histories:
            if not TABLE.fullmatch(table):
                raise RuntimeError('unexpected history table name')
            restored[table] = int(sql('SELECT count(*) FROM %s;' % table))
        tables = int(sql("SELECT count(*) FROM information_schema.tables WHERE table_schema='public';"))
        return restored, tables, seconds
    finally:
        subprocess.run(['docker', 'rm', '-f', '-v', name], capture_output=True)


def drill(folder, identity):
    folder = Path(folder)
    manifest = json.loads((folder / 'offsite-manifest.json').read_text())
    evidence = dict(runId=manifest['runId'], sha=manifest['sha'], offHostFiles=[], drills=[])
    for item in manifest['files']:
        encrypted = folder / item['file']
        seen = sha256(encrypted)
        evidence['offHostFiles'].append(dict(file=item['file'], sha256=seen, bytes=encrypted.stat().st_size))
        result = dict(label=item['label'], result='FAIL')
        evidence['drills'].append(result)
        if seen != item['sha256']:
            result['reason'] = 'encrypted file differs from the backup manifest'
            continue
        plain = folder / (item['file'] + '.plain')
        try:
            run('age', '--decrypt', '--identity', str(identity), '--output', str(plain), str(encrypted))
            if sha256(plain) != item['dumpSha256']:
                result['reason'] = 'decrypted dump differs from the dump checksum'
                continue
            restored, tables, seconds = restore(plain, int(item['postgresMajor']), item['histories'])
            result.update(histories=restored, tables=tables, seconds=seconds)
            if restored != item['histories']:
                result['reason'] = 'migration history row counts differ'
                continue
            result['result'] = 'PASS'
        except RuntimeError as error:
            result['reason'] = str(error)[:300]
        finally:
            plain.unlink(missing_ok=True)
    evidence['verdict'] = 'PASS' if all(d['result'] == 'PASS' for d in evidence['drills']) else 'FAIL'
    return evidence


def main(argv):
    if len(argv) != 3:
        print('usage: offsite_drill.py <artifact dir> <identity file> <evidence.json>')
        return 2
    evidence = drill(argv[0], argv[1])
    Path(argv[2]).write_text(json.dumps(evidence, indent=1, sort_keys=True))
    for d in evidence['drills']:
        print('RESTORE_DRILL=%s service=%s tables=%s seconds=%s %s' % (
            d['result'], d['label'], d.get('tables'), d.get('seconds'), d.get('reason', '')))
    return 0 if evidence['verdict'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
