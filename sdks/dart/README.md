# cfmeet_platform_sdk (beta)

Dart / Flutter client for the CF Meet platform API (Part VI Section 52): a typed client
over every public operation the API Gateway routes (generated from the published
contracts), retries by retry class, idempotency keys, cursor pagination, realtime
WebSockets, webhook signature verification and the Messaging E2EE core over `dart:ffi`
(keys never leave the client).

Status: beta, not published (`publish_to: none`). Usage, generation and testing are
documented in [`../README.md`](../README.md); the release pipeline is
`.github/workflows/sdk-release.yml` (manual, `SDK_PUBLICATION_ENABLED`).
