# infra/api-gateway — local runtime of the Part VI services

LOCAL ONLY. Staging / production integration requirements are recorded in
[`docs/phases/phase-06/deployment-integration.md`](../../docs/phases/phase-06/deployment-integration.md);
nothing here changes live Nginx routing, the VPS or any deployment workflow.

| Service | Port (127.0.0.1) | Database | Network exposure |
|---|---|---|---|
| API Gateway (`services/api-gateway`) | 8099 | none | the only customer-facing listener |
| Developer Platform (`services/developer-platform`) | 8098 | `cfmeet_developer_platform` (own) | internal only (gateway → Developer Platform) |
| Platform Access (existing) | 8094 | its own | internal only |

## Native run (recommended, no Docker)

```bash
infra/api-gateway/local/bootstrap.sh --admin-url postgres://postgres:postgres@127.0.0.1:5432/postgres
# add .generated/platform-access-issuers.json entries to your local Platform Access bundle, restart it
cp infra/api-gateway/local/.generated/developer-platform.env services/developer-platform/.env   # set the password
cp infra/api-gateway/local/.generated/api-gateway.env services/api-gateway/.env
make developer-platform-migrate && make developer-platform     # and make developer-platform-worker
make api-gateway
curl -s localhost:8099/ready     # publicRouting: disabled — every customer path is 404
```

Enable the foundation route locally by setting `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true`
and `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=true`, then
`curl -H "Authorization: Bearer <credential>" localhost:8099/v1/developer-platform/context`.

## Container topology (optional)

[`local/compose.yml`](local/compose.yml): the gateway on `ingress` (published on
127.0.0.1:8099) and `internal`; the Developer Platform and its worker on `internal`
(`internal: true`, no published port). Platform Access and PostgreSQL stay native and are
reached through `host.docker.internal`. Make the generated key files readable by the
image's non-root user (uid 10001) before `docker compose -f infra/api-gateway/local/compose.yml up --build`.
