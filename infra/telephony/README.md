# CF Meet SIP / media edge (`infra/telephony`)

The self-operated SIP / media edge of the Telephony service (Part E §41; ADR 0015). It is
**Asterisk 20** (Ubuntu 24.04 package `asterisk`) acting as a B2BUA with mixing bridges
that anchor all public-network media. Telephony (`services/telephony`) drives it **only**
through the Asterisk REST Interface (ARI); business services never see SIP.

```
              carriers (lawful interconnects)                    CF Meet private network
   ┌──────────────────────────────┐   SIP 5060/udp, 5061/tls    ┌──────────────────────────────┐
   │ carrier SBC A / B / …        │ ──────────────────────────▶ │  SIP / media edge (Asterisk) │
   │ (allowlisted CIDRs only)     │ ◀── RTP 10000-20000/udp ──▶ │  PJSIP trunks, mixing bridges│
   └──────────────────────────────┘                             │  ARI 8088 (private only)     │
                                                                └──────────┬───────────────────┘
                                         ARI REST + event WebSocket        │   internal SIP 5070/udp
                                         (private control network)         │   (RTC media gateway)
                                        ┌──────────────────────────┐       │   ┌──────────────────────┐
                                        │ Telephony service (8092) │◀──────┘   │ RTC media gateway     │
                                        │ internal API only        │           │ (application media    │
                                        └──────────┬───────────────┘           │  side, LiveKit SIP)   │
                                                   │ PostgreSQL (own DB)       └──────────────────────┘
```

## Network segmentation (§48, §175–§177)

| Plane | Listener | Exposure |
|---|---|---|
| Public SIP ingress | `<public-sip-ip>:5061/tls`, `:5060/udp` | carriers only: firewall allowlist of each interconnect's signalling CIDRs; PJSIP `identify` by IP; no anonymous endpoint (unidentified INVITEs are refused before any dialplan) |
| Media (RTP) | `10000-20000/udp` (`rtp.conf`) | carriers' media CIDRs and the RTC media gateway; media is anchored at the edge (`direct_media = no`) — never routed to application containers |
| SIP control (ARI) | `<private-control-ip>:8088` (`http.conf`) | **private only**, Telephony instances only; TLS when not single-host. Telephony refuses a public ARI URL in `staging` / `production` |
| Internal media peer | `<private-media-ip>:5070/udp` | the RTC media gateway only (`app-media` endpoint identified by its CIDR) |
| Telephony internal API | `:8092` | service-authenticated (`/internal/v1`), cluster-internal |
| Provider callbacks | `POST /provider-callbacks/{provider}[/{token}]` on Telephony | provider-authenticated (signature / token), 16 KiB body limit |
| Database | Telephony's own PostgreSQL | Telephony only |

## Toll-fraud boundary

* The dialplan **never dials**: every dialog enters Telephony's Stasis application, and
  only Telephony originates — after authorization proof, route / country policy, rate,
  concurrency, spend reservation, provider / route enablement and circuit checks.
* Caller identity is selected server-side (the session's relay number, bound to the
  interconnect); a request body can never set it.
* Unknown peers are refused at the edge; known peers can only reach bound relay numbers
  (others are refused by Telephony and raise `SIP_PEER_REJECTED`); inbound dialogs are
  rate-limited per peer.
* Connected-line / party identity is never propagated across a bridge
  (`send_connected_line = no`).

## Secret injection (§47, §66–§67)

Nothing secret is committed or stored in a database row.

| Secret | Delivered as | Referenced by |
|---|---|---|
| Trunk credentials, carrier peer addresses | `/run/secrets/edge/trunks/<peer-id>.conf` (generated from the secret store; see `asterisk/trunk.example.conf`) | `sip_peer_config_refs.id` (= PJSIP endpoint name) and `.secret_ref` |
| SIP TLS certificate / key | `/run/secrets/edge/sip-tls.{crt,key}` | `asterisk/pjsip.conf` |
| ARI user + password | `/run/secrets/edge/ari-users.conf` (edge) and the file named by `TELEPHONY_ARI_PASSWORD_FILE` (Telephony) | `asterisk/ari.conf` |
| Provider API / callback secrets | files in `TELECOM_PROVIDER_SECRETS_DIR` | `telecom_providers.credential_ref` |

Telephony configuration for the edge: `TELEPHONY_SIP_EDGE=asterisk-ari`,
`TELEPHONY_ARI_URL`, `TELEPHONY_ARI_USERNAME`, `TELEPHONY_ARI_PASSWORD_FILE` (inline
passwords are refused), `TELEPHONY_ARI_APP` (default `cfmeet-telephony`),
`TELEPHONY_ARI_TIMEOUT_MS`.

## Carrier peer configuration

1. Register the interconnect and its route in Telephony:
   `telephony-admin provider add --id <id> --adapter SIP_TRUNK --capabilities OUTBOUND_VOICE,INBOUND_VOICE,SIP_TRUNK,STATUS_QUERY,EVENTS,DTMF,CALLER_ID_SELECTION`,
   `telephony-admin sip-peer add --id <peer-id> --provider <id> --secret-ref <name> --allowed-cidrs <cidrs>`,
   `telephony-admin route add --id <route> --provider <id> --countries NG --capabilities VOICE_OUTBOUND,VOICE_INBOUND --sip-peer <peer-id> ...`,
   `telephony-admin number bind --number <relay E.164> --route <route>`;
   enable them only after certification.
2. Generate `/run/secrets/edge/trunks/<peer-id>.conf` from the secret store and reload PJSIP.
3. Run the real-provider certification (below) before enabling the public flags.

## Local simulation and certification

No Docker is needed: install `asterisk` and `sip-tester` (SIPp) from the Ubuntu 24.04
archive and run

```
cd services/telephony && TEST_DATABASE_URL=postgres://... cargo test --test sip_certification -- --nocapture
```

The test boots Asterisk with `certification/` (loopback only: edge `127.0.0.1:5070`,
carrier A `127.0.0.2`, carrier B `127.0.0.4`, application media `127.0.0.3`, ARI
`127.0.0.1:8088` with a documented certification-only password) and runs the SIPp
scenarios in `certification/sipp/`:

* outbound INVITE / 180 / 200 / ACK, application-media bridge with a single-use media
  join code, **real RTP** (edge RTP statistics on the public leg), DTMF both ways (RFC
  4733), remote hold / unhold (re-INVITE sendonly / sendrecv) and edge hold / resume,
  hangup (BYE), usage;
* busy (486), no answer (ring timeout → CANCEL / 487);
* inbound INVITE to a relay number: peer validation, ringing **before** answer, relay
  mapping, caller protection, offer, answer only when the application media joins, BYE;
* unidentified source refused at the edge; known peer to an unbound number refused;
* phone-to-phone bridge of two public legs on two interconnects (RTP on both);
* transfer primitive (redirect of a ringing dialog → 302);
* edge down → public origination fails safely.

CI runs it in `.github/workflows/telephony-sip-certification.yml` with
`TELEPHONY_SIP_CERT_REQUIRED=1`. These peers are **simulators**: this certifies the edge
and the SIP signalling, not a real carrier. Real-carrier certification needs a lawful
interconnect (see `services/telephony/README.md`).
