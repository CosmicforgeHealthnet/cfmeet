# sdks/ — CF Meet platform SDKs (Part VI scaffolding)

| SDK | Path | Package | Status |
|---|---|---|---|
| TypeScript | [`typescript/`](typescript) | `@cfmeet/platform-sdk` (private) | scaffold |
| Dart | [`dart/`](dart) | `cfmeet_platform_sdk` (`publish_to: none`) | scaffold |

Both SDKs are generated from the **published** contracts listed in
[`generation.json`](generation.json) (registered in `packages/contracts/manifest.json`)
by [`generate.py`](generate.py) — one deterministic generator, standard library only:

```bash
python3 sdks/generate.py          # regenerate src/generated / lib/src/generated
python3 sdks/generate.py --check  # CI: generated files current, public contracts only, no product names
make sdks-check
```

First-party CosmicForge products and external customers use the same SDKs and the same
published API contracts; they differ only through the scopes Platform Access grants and
the tenant's trust classification. There is no product-specific SDK or endpoint.

**Not implemented (Section 52):** the typed HTTP client, credential handling, retries /
idempotency helpers, pagination, WebSocket streams, publishing. Nothing here should be
consumed by applications yet.
