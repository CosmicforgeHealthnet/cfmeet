# Communications runtime and recovery

Sections 4–7 use the existing private VPS network, PostgreSQL and TLS proxy. Consumer requests enter through Account at `/api/v2/communications`; the Hub has no published host port. Vercel and frontend deployment are outside this release.

## Persistence and keys

The Hub owns its database, runtime role and migration role. Account, Calling and Messaging retain their own databases. Their additive integration migrations have separate history tables, so older service binaries do not mistake feature migrations for unknown base migrations.

Private runtime configuration and acceptance evidence stay under `/opt/cfmeet-core/communications/`, mode 0700. Secret environment and rollback inspection files are mode 0600. The Hub signing key and invitation digest key are mounted read only, mode 0400, owned by its runtime UID 10001. Other services receive only the Hub public key in their trust bundles. The Hub never receives their private keys or account credentials.

A root-only operator key can invoke the Hub's bounded backfill, replay and metrics endpoints. Its grant is only `communications-backfill.run`; it has no Platform Access administration grant. Internal endpoints are absent from public Swagger.

Protected same-host key recovery copies live under `/opt/cfmeet-core/key-recovery/communications/`. To recover a missing Hub signing key or invitation digest key, restore its existing copy to the original feature directory, retain UID 10001 and mode 0400, and recreate the affected container through the existing workflow. Preparation automatically restores a missing file from that copy and refuses silent regeneration once the production database has been provisioned. Never print keys, paste them into a workflow, or commit them.

Losing the invitation digest key without its recovery copy makes outstanding invitation digests unrecoverable. Public CF Meet IDs, accounts, principal mappings and conversations do not depend on that key. A replacement key must be treated as an explicit invitation invalidation, not transparent recovery.

These copies share the VPS failure domain. **Off-server backups and disaster-recovery certification remain DEFERRED by the owner.** No external storage provider is provisioned.

## Release and rollback

Use `.github/workflows/cfmeet-calling-production.yml`, the existing GitHub-controlled VPS workflow. `communications-inspect` is read only. `communications-stage` migrates isolated owner databases and starts private candidates; it does not replace production services or alter customer records. `communications-release` requires successful acceptance of the same immutable image IDs before applying the three additive owner integrations and creating the production Hub database.

An optional `image_sha` selects an already built source revision. The workflow refuses it if service source, shared authentication/policy code or compiled public contracts differ from the workflow revision. Docker image revision labels must match. This permits operations-only fixes without silently deploying different application code.

Production cutover retains the original containers, configurations, mounts, networks, startup commands and loopback bindings. Calling is not stopped while active calls remain. A failed cutover restores prior containers; no feature database tables, account mappings or histories are dropped. `communications-rollback` invokes that retained runtime rollback. Newly introduced database enum states require a compatible Account binary; additive schema rollback is deliberately unsupported.

The existing Account deployment workflow also preserves its running configuration and verifies a private candidate before switching. It retains its own stopped rollback container. It shares the production deployment concurrency group with Communications cutover.

## Acceptance evidence

Private candidates use FIRST_PARTY TEST identities, dedicated databases and authorized test accounts. MANAGED Messaging is allowed only by the isolated TEST policy to exercise canonical direct-conversation creation and send enforcement. Production personal encryption policy is not weakened. Capabilities continue to report Messaging unavailable until its actual device and key-transparency prerequisites are satisfied.

The HTTP journey checks account activation without a phone, stable principal mappings, unique IDs, invitation ownership and discovery privacy, requests and acceptance, direct-call permissions, direct Messaging enforcement, aliases, common groups, blocking, revocation, rotation and independent Meetings. It does not certify new audio/video capture merely from successful call creation. Existing Calling RTC and enterprise regressions remain separate evidence.

The dedicated Communications Hub CI job explicitly enables `personal-communications-e2e`, starts all five real services with least-privilege owner databases, Redis and LiveKit, and runs the resource-requiring journey. Generic certification unit jobs do not attempt to start this additional stack. Google records created before migration are included in backfill preservation tests; those fixtures do not claim an external Google OAuth login was performed.

Phone OTP, phone-number discovery/matching, external telecom, Messaging activation and the unified call/chat timeline are deferred to their respective milestones.
