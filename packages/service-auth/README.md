# packages/service-auth (`cf-service-auth`)

Internal service authentication for CF Meet backend services (Part B §11,
[ADR 0004](../../docs/decisions/0004-internal-service-authentication.md)). A shared
library, not a deployable service; it never depends on a service.

* Ed25519 (EdDSA) compact JWS service tokens: pinned algorithm, `typ` `cf-service+jwt`,
  required `kid`, exactly one audience, runtime environment binding, 120 s default /
  300 s maximum lifetime, 30 s skew, signed request / correlation ids and optional
  signed `DelegatedPlatformContext`.
* Transport in `X-CF-Service-Authorization: Bearer …` plus `X-CF-Request-Id` /
  `X-CF-Correlation-Id`, which must equal the signed claims.
* Receiver-authoritative permissions from a trust bundle; tokens carry none. The
  permission registry fixes each permission's receiver and the issuers it may ever be
  granted to.
* Separately typed delegated credentials with their own keys and trust bundles:
  Contact Vault access tokens (`contact_delegation`, `cf-contact-access+jwt`) and number
  routing grants (`number_grant`, `cf-number-grant+jwt`, Part IV Workstream K: signed by
  Numbering's dedicated grant key, revision-bound, at most 300 s, no telephone number;
  receivers verify locally with a `NumberGrantTrust` bundle and Numbering's verification
  is authoritative for revocation and single use).
* Contracts: `packages/contracts/platform/` (`cfmeet.platform.*`) and
  `numbering/routing-grant` (`cfmeet.numbering.routing-grant`); conformance is tested in
  both directions.

## Use

```rust
use cf_service_auth::{InternalPermission, OutboundServiceRequest, ServiceId};

// outbound (new operation, or continuing an inbound one with next_hop_of)
let hop = OutboundServiceRequest::new_operation(ServiceId::privacy_relay());
signer.sign(&hop)?.apply_to(&mut headers);

// inbound
let caller = verifier.verify_headers(&headers)?;          // InternalServiceContext
caller.require_permission(InternalPermission::CommunicationAuthorize)?;
```

The optional `client` feature adds `InternalServiceClient` (reqwest + rustls): targets by
audience from configuration, `/internal/v1/` paths only, HTTPS outside `local` / `test`,
no redirects, no proxies, bounded timeouts.

## Keys

```bash
cargo run --bin cf-service-keygen -- generate --kid calling-staging-2026-10 --out /secure/calling-staging-2026-10.pem
cargo run --bin cf-service-keygen -- public-jwk --kid calling-staging-2026-10 --key /secure/calling-staging-2026-10.pem
```

`generate` writes a PKCS#8 PEM with mode 0600, refuses to overwrite, and prints only the
public JWK for the receivers' trust bundles. One key per service and per runtime
environment; never commit a private key (`*.pem` is git-ignored and CI scans for key
material). Rotation: add the new public key to every receiver's bundle, switch the
signer, remove the old key after the maximum token lifetime plus skew.

## Tests

```bash
make service-auth-test     # core and `client` feature sets
make service-auth-lint
```

Suites: `authentication` (signature, algorithm, header, audience, environment, lifetime,
id binding), `policy` (trust bundle validation, least privilege, rotation), `context`
(context separation, delegated context, propagation), `contract_conformance` (against
`packages/contracts`) and `keygen`.
