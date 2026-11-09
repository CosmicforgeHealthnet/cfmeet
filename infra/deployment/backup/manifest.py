#!/usr/bin/env python3
"""Backup manifest helper for the CF Meet database backup tooling (SEC-010 / SEC-011).

A manifest describes ONE database backup and travels next to it (it is never encrypted:
it holds only names, sizes, checksums, versions and migration-history row counts, no row
data and no credentials). The restore drill verifies a restored database against it.

    manifest.py write  --out M --service S --database D ... (see `write --help`)
    manifest.py get    M <dotted.key>          print one scalar value
    manifest.py history M                       name|required|present|rows|max_version lines
    manifest.py set-offsite M --status S [--target-kind K] [--detail TEXT]
    manifest.py summary --out F --stamp T --results R --offsite-status S [...]
"""
import argparse
import datetime
import json
import os
import socket
import sys

FORMAT = "cfmeet-db-backup/1"
LOCAL_ONLY = "LOCAL_ONLY — not disaster recovery"
OFFSITE_STATUSES = ("NOT_CONFIGURED", "PENDING", "COPIED", "FAILED")


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_json(path, value):
    tmp = path + ".partial"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, ensure_ascii=False)
        handle.write("\n")
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)


def offsite_block(status, kind=None, detail=None):
    block = {"status": status}
    if kind:
        block["target_kind"] = kind
    if detail:
        block["detail"] = detail
    if status == "COPIED":
        block["classification"] = "OFF_HOST_COPY_VERIFIED"
        block["copied_at"] = now()
    else:
        block["classification"] = LOCAL_ONLY
    return block


def read_history(path):
    entries = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            name, required, present, rows, max_version = line.split("|", 4)
            entries.append({
                "name": name,
                "required": required == "true",
                "present": present == "true",
                "rows": int(rows),
                "max_version": max_version or None,
            })
    return entries


def cmd_write(a):
    manifest = {
        "format": FORMAT,
        "service": a.service,
        "database": a.database,
        "label": a.label,
        "created_at": now(),
        "source_host": a.source_host or socket.gethostname(),
        "pg_dump_version": a.pg_dump_version,
        "server_version": a.server_version,
        "database_size_bytes": int(a.db_size),
        "public_table_count": int(a.table_count),
        "history_tables": read_history(a.history_file),
        "dump": {
            "file": os.path.basename(a.dump_file),
            "format": "custom",
            "options": ["--format=custom", "--no-owner", "--no-privileges"],
            "size_bytes": int(a.dump_size),
            "sha256": a.dump_sha256,
        },
        "plaintext_retained": a.plaintext_retained == "yes",
    }
    if a.encrypted_file:
        manifest["encryption"] = {
            "tool": "age",
            "recipients": [r for r in a.recipient if r],
            "file": os.path.basename(a.encrypted_file),
            "size_bytes": int(a.encrypted_size),
            "sha256": a.encrypted_sha256,
        }
    else:
        manifest["encryption"] = {"tool": "none"}
    manifest["offsite"] = offsite_block(a.offsite_status)
    write_json(a.out, manifest)


def load(path):
    with open(path, encoding="utf-8") as handle:
        value = json.load(handle)
    if value.get("format") != FORMAT:
        sys.exit("manifest %s: unsupported format" % path)
    return value


def cmd_get(a):
    value = load(a.manifest)
    for part in a.key.split("."):
        if not isinstance(value, dict) or part not in value:
            sys.exit("manifest %s: no key %s" % (a.manifest, a.key))
        value = value[part]
    if isinstance(value, (dict, list)):
        sys.exit("manifest %s: %s is not a scalar" % (a.manifest, a.key))
    if isinstance(value, bool):
        value = "true" if value else "false"
    print("" if value is None else value)


def cmd_history(a):
    for entry in load(a.manifest).get("history_tables", []):
        print("%s|%s|%s|%d|%s" % (entry["name"], "true" if entry["required"] else "false",
                                  "true" if entry["present"] else "false", entry["rows"],
                                  entry.get("max_version") or ""))


def cmd_set_offsite(a):
    value = load(a.manifest)
    value["offsite"] = offsite_block(a.status, a.target_kind, a.detail)
    write_json(a.manifest, value)


def cmd_summary(a):
    databases = []
    with open(a.results, encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line:
                continue
            service, status, manifest, detail = line.split("|", 3)
            databases.append({"service": service, "status": status,
                              "manifest": manifest or None, "detail": detail or None})
    ok = all(d["status"] == "OK" for d in databases) and bool(databases)
    summary = {
        "format": "cfmeet-db-backup-run/1",
        "stamp": a.stamp,
        "finished_at": now(),
        "source_host": a.source_host or socket.gethostname(),
        "result": "OK" if ok else "PARTIAL_FAILURE",
        "databases": databases,
        "offsite": offsite_block(a.offsite_status, a.target_kind, a.detail),
    }
    write_json(a.out, summary)


def main():
    # Shell callers compare this output byte for byte: UTF-8, LF, whatever the platform.
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    w = sub.add_parser("write")
    for name in ("out", "service", "database", "label", "pg-dump-version", "server-version",
                 "db-size", "table-count", "history-file", "dump-file", "dump-sha256", "dump-size"):
        w.add_argument("--" + name, required=True)
    w.add_argument("--encrypted-file")
    w.add_argument("--encrypted-sha256")
    w.add_argument("--encrypted-size", default="0")
    w.add_argument("--recipient", action="append", default=[])
    w.add_argument("--plaintext-retained", choices=("yes", "no"), required=True)
    w.add_argument("--offsite-status", choices=OFFSITE_STATUSES, default="NOT_CONFIGURED")
    w.add_argument("--source-host")
    w.set_defaults(func=cmd_write)

    g = sub.add_parser("get")
    g.add_argument("manifest")
    g.add_argument("key")
    g.set_defaults(func=cmd_get)

    h = sub.add_parser("history")
    h.add_argument("manifest")
    h.set_defaults(func=cmd_history)

    o = sub.add_parser("set-offsite")
    o.add_argument("manifest")
    o.add_argument("--status", choices=OFFSITE_STATUSES, required=True)
    o.add_argument("--target-kind")
    o.add_argument("--detail")
    o.set_defaults(func=cmd_set_offsite)

    s = sub.add_parser("summary")
    s.add_argument("--out", required=True)
    s.add_argument("--stamp", required=True)
    s.add_argument("--results", required=True)
    s.add_argument("--offsite-status", choices=OFFSITE_STATUSES, required=True)
    s.add_argument("--target-kind")
    s.add_argument("--detail")
    s.add_argument("--source-host")
    s.set_defaults(func=cmd_summary)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
