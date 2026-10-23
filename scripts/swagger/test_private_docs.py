#!/usr/bin/env python3
"""Regression tests for offline, private-only Swagger catalog generation."""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main() -> None:
    with TemporaryDirectory(prefix="cfmeet-private-docs-") as tmp:
        out = Path(tmp) / "catalog"
        proc = subprocess.run(
            [sys.executable, str(HERE / "build-private-docs.py"), "--out", str(out)],
            cwd=ROOT, capture_output=True, text=True, check=True
        )
        config = json.loads((out / "swagger-config.json").read_text())
        assert len(config["urls"]) == 12, config["urls"]
        assert len({doc["name"] for doc in config["urls"]}) == 12
        for doc in config["urls"]:
            file = out / doc["url"].removeprefix("./")
            spec = json.loads(file.read_text())
            assert isinstance(spec.get("paths"), dict), doc["name"]
            if doc["name"] != "Platform internal HTTP":
                assert spec["paths"], doc["name"]
        assert (out / "index.html").is_file()
        assert (out / "app.js").is_file()
        assert (out / "robots.txt").is_file()
        assert "supportedSubmitMethods: []" in (out / "app.js").read_text()
        assert "Messaging (internal)" in {doc["name"] for doc in config["urls"]}
        assert "Platform Access (internal)" in {doc["name"] for doc in config["urls"]}
        assert "Telephony (internal)" in {doc["name"] for doc in config["urls"]}
        # The builder must reject accidental publication within the checkout.
        forbidden = ROOT / "apps" / "web" / "public" / "private-api-docs"
        result = subprocess.run(
            [sys.executable, str(HERE / "build-private-docs.py"), "--out", str(forbidden)],
            cwd=ROOT, capture_output=True, text=True,
        )
        assert result.returncode != 0
        assert not forbidden.exists()
        # Existing non-empty destinations must never be silently overwritten.
        result = subprocess.run(
            [sys.executable, str(HERE / "build-private-docs.py"), "--out", str(out)],
            cwd=ROOT, capture_output=True, text=True,
        )
        assert result.returncode != 0
        print(proc.stdout.strip())
    print("private engineering Swagger catalog checks passed")


if __name__ == "__main__":
    main()
