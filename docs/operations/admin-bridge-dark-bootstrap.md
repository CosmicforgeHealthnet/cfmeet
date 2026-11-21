# CF Meet Admin Bridge — initial dark VPS bootstrap

This phase is intentionally not HQ activation. It registers the A1/A2
Admin Bridge image with the coordinated production release, including
selective-image planning and missing-service first-time deployment.

The first coordinated release after merge creates the cfmeet-admin-bridge
container on the existing private Docker network, with a 128 MiB limit,
no published port, non-root uid 10001, read-only filesystem and dropped
capabilities. Its only listener is 127.0.0.1:8190, loopback inside its
own network namespace. Readiness and ops API isolation are checked from
a short-lived probe joining that namespace.

Every ADMIN_BRIDGE_ENABLED, ADMIN_BRIDGE_WRITES_ENABLED, and
ADMIN_BRIDGE_MODULE_* switch must remain false. Release verification
refuses a manual switch-on. The HQ mTLS listener never starts;
Admin Bridge is not exposed through public or private routes and cannot
mutate user data or receive HQ credentials.

This dark-only bootstrap intentionally defers the Admin Bridge database,
migrator role, least-privilege SQL, policy signing, HQ mTLS enrollment, and
owner grants. Those require full separately certified activation after
stacked feature work is merged (design section 14 and PR #133).
Attempting to enable the bridge before this is prohibited.
No telecom, PSTN, SMS, numbering or Privacy Relay feature is activated.

The image is rebuilt and the container replaced only when its actual
Dockerfile COPY inputs changed or the service is missing. Existing
unchanged containers are preserved.
