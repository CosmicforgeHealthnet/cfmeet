# ADR 0027: Application (RTC) call-audio capture through LiveKit Egress

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §60 (voicemail) and §61 (call recording) for application-only
  (CF Meet RTC / LiveKit) Calls. Implemented in `services/calling/src/media/livekit_egress.rs`
  (adapter), `services/calling/src/calls/app/enterprise/capture.rs` (collector),
  `services/calling/src/calls/app/enterprise/voicemail.rs` (the neutral
  `CallMediaCapture` interface), `packages/object-storage` (bounded server-side `get`),
  `infra/livekit/egress/`. Builds on ADRs 0023 and 0024.

## Context

Voicemail and recording were certified for SIP / Asterisk legs (Telephony captures at the
edge and delivers a WAV). Application-only Calls had no capture adapter, so Calling
refused them voicemail / recording (`CAPTURE_UNAVAILABLE`) rather than faking a result.
Business rules (authorization, consent, notice, retention, access) must stay identical for
both media paths and must not depend on Asterisk or on LiveKit Egress types.

## Decision

* **One provider-neutral boundary.** `CallMediaCapture` (start / stop / poll / release)
  addressed by Calling's own capture id (the voicemail message or recording id — the
  idempotency key). A `CaptureRequest` carries the Call, the purpose (`VOICEMAIL` |
  `RECORDING`, never client-supplied), the maximum duration and the **authorized media
  scope**: the media identities whose audio alone may be captured (a voicemail: the
  caller's live sessions; empty = the Call's mixed audio, only for an authorized
  recording). `CaptureStatus` is `Running | Pending | Finished(media) | Failed(code)`;
  no provider identifier crosses it.
* **Routing by the actual media path, unchanged above it.** A Call with a SIP leg keeps
  the certified Telephony / Asterisk edge capture; an application-only Call uses the
  configured `CallMediaCapture` (`CALLING_CAPTURE_PROVIDER=livekit-egress`, default
  `none`). Policy, consent, notice, artifact, retention and access code is shared.
* **LiveKit Egress adapter** (`media::livekit_egress`), using the Call's existing media resource
  (`MediaResourceName::for_call`; it never creates a room) and Calling's server API key
  (`roomRecord` grant only for Egress requests):
  * voicemail → **track egress of the caller's microphone track only** (no other
    participant, no video, no room mix);
  * recording → **audio-only room-composite egress** (mixed call audio, never video);
  * output: Ogg / Opus to the private staging key `{prefix}/{captureId}.ogg` of the
    protected bucket, `disable_manifest` (no room / participant metadata objects). The
    Egress worker's storage credentials live in ITS injected configuration — Calling never
    sends a credential, and no request carries metadata or contact data;
  * idempotent start (an existing egress for the capture is the capture), stop only of
    running egresses (412 / not-found = already stopped), best-effort stop after a failed
    or timed-out start.
* **Collection through the SAME storage semantics as SIP.** A worker pass
  (`capture_tick`, never kill-switched) polls application captures: a due stop (caller
  gone, maximum message length, recording `STOPPING`) is (re-)issued only while the adapter
  reports the capture `Running` (a lost stop is retried, a successful one never repeated);
  `Finished` media goes through `deliver_capture` — content type verified (`audio/wav`, or
  `audio/ogg` only with an `OggS` container), digest, size bound, private object under the
  canonical key (extension from the verified format), READY, events — then the staging
  object is released. Storage unavailable keeps the media staged and retries; `Failed`
  marks the artifact FAILED (the Call is not affected); a FAILED (abandoned) capture is
  still stopped and released for 30 minutes and never becomes READY.
* **Recorder participants.** A LiveKit participant of kind `EGRESS` (only creatable with
  Calling's server credential) is neither admitted as a session nor quarantined by
  reconciliation, and its webhooks are not session evidence.
* **Recording duration.** Unchanged and documented: the per-policy
  `maxRecordingSeconds` (60–1440) bounded by the platform maximum `MAX_RECORDING_SECONDS`
  (1440 s), which is the single-artifact delivery bound (`MAX_CAPTURE_BYTES`, 24 MiB:
  ≈ 25 min of 8 kHz 16-bit WAV). It is an explicit policy value under a technical
  ceiling, not an arbitrary product limit; raising the ceiling needs chunked / multipart
  artifact delivery and is out of scope.

## Consequences

* Application-only Calls get voicemail and recording with the same authorization,
  consent, notice, retention and audited access as SIP Calls. Certified against a real
  LiveKit 1.13.9, real LiveKit Egress 1.10.0, real WebRTC audio and an S3-compatible
  store (`rtc_certification`), with targeted failure tests against the real adapter
  (`enterprise_capture_egress`).
* Deployments that do not run Egress keep `CALLING_CAPTURE_PROVIDER=none`: RTC voicemail
  / recording stay refused (`CAPTURE_UNAVAILABLE`), never faked. `livekit-egress` without
  the LiveKit provider or the protected artifact store refuses to start.
* The audio-only room composite uses Egress's headless browser, which does not gather ICE
  candidates on loopback: LiveKit must advertise a non-loopback (private) media address.
* SIP desk-phone keypad consent is still not offered: policies that need a SIP
  participant's consent deny recording (fail closed).
