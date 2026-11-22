#!/usr/bin/env python3
"""Root-only, local Docker log archive. Never sends log content over the network."""
import datetime as dt
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading
import time

NAME = re.compile(r'cfmeet-[A-Za-z0-9_.-]{1,100}')
UTC = dt.timezone.utc


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    with temporary.open('w', encoding='utf-8') as target:
        json.dump(value, target)
        target.flush()
        os.fsync(target.fileno())
    os.replace(temporary, path)


def expired_archives(folder, today):
    cutoff = today - dt.timedelta(days=14)
    for path in folder.glob('*.log'):
        try:
            old = dt.date.fromisoformat(path.stem) < cutoff
        except ValueError:
            continue
        if old and path.is_file() and not path.is_symlink():
            yield path


class Archive:
    def __init__(self, root, container):
        if not NAME.fullmatch(container):
            raise ValueError('Only configured CF Meet containers may be collected')
        self.folder = root / container
        self.folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.folder.is_symlink():
            raise ValueError('Archive directory cannot be a symlink')
        self.cursor_file = self.folder / 'cursor.json'
        if self.cursor_file.is_symlink():
            raise ValueError('Cursor cannot be a symlink')
        self.cursor = (dt.datetime.now(UTC) - dt.timedelta(days=14)).isoformat()
        if self.cursor_file.exists():
            value = json.loads(self.cursor_file.read_text(encoding='utf-8'))['since']
            dt.datetime.fromisoformat(value.replace('Z', '+00:00'))
            self.cursor = value
        self.output = None
        self.day = None
        self.checkpoint_at = time.monotonic()

    def checkpoint(self):
        if self.output:
            self.output.flush()
            os.fsync(self.output.fileno())
        # Persist the cursor only AFTER the corresponding bytes are durable. A crash
        # may replay the timestamp boundary, but cannot skip an unflushed log line.
        atomic_json(self.cursor_file, {'since': self.cursor})
        self.checkpoint_at = time.monotonic()

    def append(self, line):
        timestamp = line.partition(b' ')[0].decode('ascii', errors='ignore')
        try:
            instant = dt.datetime.fromisoformat(timestamp.replace('Z', '+00:00'))
            day = instant.astimezone(UTC).date()
        except ValueError:
            # Docker's own diagnostic lines have no timestamp. Retain their bytes too.
            timestamp = None
            day = dt.datetime.now(UTC).date()
        if self.day != day:
            if self.output:
                self.checkpoint()
                self.output.close()
            target = self.folder / (day.isoformat() + '.log')
            if target.is_symlink():
                raise ValueError('Archive file cannot be a symlink')
            self.output = target.open('ab')
            os.chmod(target, 0o600)
            self.day = day
        self.output.write(line)  # exact Docker bytes, including stdout/stderr timestamps
        if timestamp:
            self.cursor = timestamp
        if time.monotonic() - self.checkpoint_at >= 5:
            self.checkpoint()

    def close(self):
        self.checkpoint()
        if self.output:
            self.output.close()
            self.output = None


def run(config):
    root = Path(config['directory'])
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.is_symlink() or not root.is_absolute():
        raise ValueError('Invalid archive directory')
    names = config['containers']
    if not names or len(names) > 80 or len(set(names)) != len(names) or any(not NAME.fullmatch(n) for n in names):
        raise ValueError('Invalid owned container list')
    stopped = threading.Event()
    processes = {}
    guard = threading.Lock()
    failures = {}

    def stop(*_):
        stopped.set()
        with guard:
            for process in processes.values():
                if process.poll() is None:
                    process.terminate()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def collect(name):
        while not stopped.is_set():
            archive = None
            try:
                archive = Archive(root, name)
                process = subprocess.Popen(['/usr/bin/docker', 'logs', '--timestamps', '--since', archive.cursor,
                                            '--follow', name], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                with guard:
                    processes[name] = process
                for line in iter(process.stdout.readline, b''):
                    archive.append(line)
                    if stopped.is_set():
                        break
                if process.wait(timeout=10):
                    raise RuntimeError('Docker log stream ended')
            except Exception:
                with guard:
                    failures[name] = failures.get(name, 0) + 1
                # No log line, credential or exception text enters the control output.
            finally:
                if archive:
                    archive.close()
            stopped.wait(3)  # resolves the current name again after a release replaces it

    threads = [threading.Thread(target=collect, args=(name,), daemon=True) for name in names]
    for thread in threads:
        thread.start()
    while not stopped.wait(10):
        with guard:
            atomic_json(root / 'health.json', {'at': dt.datetime.now(UTC).isoformat(),
                        'containers': len(names), 'threadsAlive': sum(t.is_alive() for t in threads),
                        'failures': dict(failures)})
        for name in names:
            for path in expired_archives(root / name, dt.datetime.now(UTC).date()):
                path.unlink()
    for thread in threads:
        thread.join(timeout=15)


if __name__ == '__main__':
    os.umask(0o077)
    run(json.loads(Path(sys.argv[1]).read_text(encoding='utf-8')))
