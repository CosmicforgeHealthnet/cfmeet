# Native private history readiness, 25 November 2026

Android/iOS private Messaging and encrypted account history are **not certified or activated** by the browser or service integration results. This audit inspected the actual Expo application and the available SDK/crypto bindings.

## Implementation evidence

- `apps/mobile/features/messaging/api.ts` and `hooks.ts` implement MANAGED relay messaging. `lib/messaging/relay-paths.ts` intentionally marks PRIVATE_E2EE unreadable. The existing screens preserve that boundary and never convert private conversations to managed.
- `apps/mobile/package.json` uses Expo 57 and has no native Messaging crypto module dependency. `sdks/react-native` explicitly ships no native code; its native bridge interface covers incoming calls and push, not MLS.
- `packages/messaging-crypto/ffi/include/cf_messaging_crypto.h` exposes the Rust C ABI, including legacy transfer functions. Its implementation uses `history_transfer::seal_history/open_history`; the paged v2 object implementation in `src/history_transfer/paged.rs` is not exported through this ABI.
- No Android/iOS module currently binds the Rust device handle, staged commit lifecycle, attachment crypto, or paged history objects into the Expo app. No native encrypted history database or account-scoped Messaging identity vault/unlock lifecycle is integrated.
- The existing SecureStore adapter stores account access/refresh tokens. It is not an encrypted Messaging history vault and does not implement protected private-history unlocking.

## Safe reliability changes verified here

MANAGED conversation pagination now follows all opaque cursor pages instead of silently ending after five pages. Repeated cursors fail visibly. Message cursors and materialized history now share the same query-cache entry; clearing or evicting that entry replays server history from zero, rather than retaining a module-global cursor that hides earlier messages. Interrupted batches publish neither partial history nor an advanced cursor. Bounded polling batches resume after 1,000 events, including non-display events. List/history requests honor cancellation.

Validation on Windows/Node 25.2.1: `npm run typecheck` passed; `npm test` passed all 55 tests, including six new history regressions. Scoped ESLint passed all four changed mobile files. Full Expo 57 default lint was attempted and failed on 15 pre-existing errors and one warning in unrelated meeting/settings/toast screens. Its automatic ESLint bootstrap dependency/config changes were removed; no dependency or lockfile change is required by this patch.

## Required before native private activation

1. Ship actual Android/iOS Rust bindings and v2 history exports, preserving staged commits and existing reviewed cryptography, then verify their byte-level interoperability with the web client.
2. Implement account/device-bound encrypted local persistence and OS-protected unlocking with explicit cancellation, biometric-key invalidation, logout, session-expiry and terminal-revocation behavior. Expo 57 SecureStore documentation warns against storing large values directly and documents biometric invalidation; token storage alone is insufficient.
3. Integrate device enrollment, witnessed transparency, canonical message/event replay, resumable approved history transfer and private attachment/event materialization into the actual native application.
4. Complete the required native security review and build/sign through the existing approved release infrastructure; execute acceptance on real Android and iOS installations, including simultaneous authorized devices, offline recovery, native OS unlock and revocation. Browser emulation does not establish these results.

These are missing implementation and security/acceptance prerequisites, not a feature flag that can safely be enabled. Encrypted backup remains inactive pending its separate verified design/review.

Expo version-matched references: [SDK 57](https://docs.expo.dev/versions/v57.0.0/) and [SecureStore 57](https://docs.expo.dev/versions/v57.0.0/sdk/securestore.md).
