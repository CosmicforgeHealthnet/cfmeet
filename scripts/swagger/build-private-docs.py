#!/usr/bin/env python3
"""Build the complete CF Meet OpenAPI catalog for an access-controlled engineering host.

This tool generates STATIC files only. It does not deploy or expose them.
The destination must be outside the repository and must be empty.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPECS = (
    ("Meetings", "meetings", "openapi/meeting-service.json"),
    ("Calling", "calling", "bundled/calling/http/v1.openapi.json"),
    ("Messaging", "messaging", "bundled/messaging/http/v1.openapi.json"),
    ("Messaging (internal)", "messaging-internal", "bundled/messaging/internal/v1.openapi.json"),
    ("Numbering / DCNs", "numbering", "bundled/numbering/internal/v1.openapi.json"),
    ("Platform Access (internal)", "platform-access", "bundled/platform-access/internal/v1.openapi.json"),
    ("Platform internal HTTP", "platform-internal-http", "bundled/platform/internal-http/v1.openapi.json"),
    ("Privacy Relay", "privacy-relay", "bundled/privacy-relay/http/v1.openapi.json"),
    ("Privacy Relay (internal)", "privacy-relay-internal", "bundled/privacy-relay/internal/v1.openapi.json"),
    ("SMS Gateway", "sms-gateway", "bundled/sms-gateway/http/v1.openapi.json"),
    ("SMS Gateway (internal)", "sms-gateway-internal", "bundled/sms-gateway/internal/v1.openapi.json"),
    ("Telephony (internal)", "telephony", "bundled/telephony/internal/v1.openapi.json"),
)

CSS_SRI = "sha384-wxLW6kwyHktdDGr6Pv1zgm/VGJh99lfUbzSn6HNHBENZlCN7W602k9VkGdxuFvPn"
JS_SRI = "sha384-wmyclcVGX/WhUkdkATwhaK1X1JtiNrr2EoYJ+diV3vj4v6OC5yCeSu+yW13SYJep"
VERSION = "5.17.14"

HTML = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta name="robots" content="noindex, nofollow, noarchive" />
<title>CF Meet — Private Engineering API Catalog</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@__VERSION__/swagger-ui.css" integrity="__CSS_SRI__" crossorigin="anonymous" referrerpolicy="no-referrer" />
</head>
<body>
<div style="padding:12px 20px;font:14px sans-serif">
<strong>CF Meet — Private Engineering API Catalog</strong>
<label for="service">API service:</label>
<select id="service" aria-label="Select engineering API service"></select>
<span id="error" role="alert"></span>
</div>
<div id="swagger-ui"></div>
<script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@__VERSION__/swagger-ui-bundle.js" integrity="__JS_SRI__" crossorigin="anonymous" referrerpolicy="no-referrer"></script>
<script src="./app.js"></script>
</body>
</html>
""".replace("__VERSION__", VERSION).replace("__CSS_SRI__", CSS_SRI).replace("__JS_SRI__", JS_SRI)

APP_JS = """window.addEventListener('load', function () {
  var select = document.getElementById('service');
  var error = document.getElementById('error');
  function render(url) {
    window.ui = SwaggerUIBundle({
      url: url,
      dom_id: '#swagger-ui',
      supportedSubmitMethods: [],
      deepLinking: true,
      displayRequestDuration: true,
      validatorUrl: null,
      filter: true
    });
  }
  fetch('./swagger-config.json', {cache:'no-store'})
    .then(function(r) {
      if (!r.ok) throw new Error('catalog config unavailable');
      return r.json();
    })
    .then(function(c) {
      if (!Array.isArray(c.urls) || !c.urls.length) throw new Error('catalog empty');
      c.urls.forEach(function(item) {
        var option = document.createElement('option');
        option.value = item.url;
        option.textContent = item.name;
        select.appendChild(option);
      });
      select.addEventListener('change', function() { render(select.value); });
      select.value = c.urls[0].url;
      render(select.value);
    })
    .catch(function(e) { error.textContent = 'Unable to load private API catalog.'; });
});
"""


def build(destination: Path) -> None:
    destination = destination.expanduser().resolve()
    if destination == ROOT or ROOT in destination.parents:
        raise SystemExit("Refusing to generate private API docs inside the repository")
    if destination.exists() and any(destination.iterdir()):
        raise SystemExit("Destination must be empty: refusing stale or partial private catalog")
    entries = []
    records = []
    # Validate every input before writing anything.
    for name, slug, path in SPECS:
        origin = ROOT / "packages" / "contracts" / path
        obj = json.loads(origin.read_text(encoding="utf-8"))
        if not isinstance(obj.get("paths"), dict) or not obj.get("openapi", "").startswith("3."):
            raise SystemExit(f"Invalid bundled OpenAPI: {origin}")
        # The platform/internal-http contract currently has a zero-operation
        # schema by design; preserve it in the engineering catalog anyway.
        if not obj["paths"] and slug != "platform-internal-http":
            raise SystemExit(f"No paths in: {origin}")
        records.append((slug, obj))
        entries.append({"name": name, "url": f"./docs/{slug}.json"})
    (destination / "docs").mkdir(parents=True, exist_ok=True)
    for slug, doc in records:
        (destination / "docs" / f"{slug}.json").write_text(
            json.dumps(doc, separators=(",", ":"), ensure_ascii=False) + "\n", encoding="utf-8"
        )
    (destination / "swagger-config.json").write_text(
        json.dumps({"urls": entries, "primaryName": "Meetings"}, indent=2) + "\n",
        encoding="utf-8",
    )
    (destination / "index.html").write_text(HTML, encoding="utf-8")
    (destination / "app.js").write_text(APP_JS, encoding="utf-8")
    (destination / "robots.txt").write_text("User-agent: *\nDisallow: /\n", encoding="utf-8")
    print(f"Built {len(records)} complete contracts in {destination}.")
    print("PRIVATE ARTIFACT: serve only behind authentication and network access control.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="New empty destination outside repository")
    args = parser.parse_args()
    build(args.out)
