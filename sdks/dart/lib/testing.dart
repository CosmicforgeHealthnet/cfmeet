/// Deterministic test fakes for applications built on the CF Meet platform SDK
/// (Part VI §52.3): unit and integration tests of YOUR code run against the real
/// [CfMeetClient] / [RealtimeClient] without a network, live CF Meet services or
/// telecom providers, and without real time passing.
///
/// * [FakeCfMeetTransport] — `CfMeetClient(transport: fake)`: responses scripted per
///   catalogue operation (names validated) or per method + path, sequences (503 then
///   200, network errors, timeouts), recorded requests, deterministic request ids
///   (`req_fake_000001`, ...), a default 404 `NOT_FOUND` envelope;
/// * [FakeResponse] / [fakeErrorEnvelope] / [fakePageBody] — contract-shaped answers;
/// * [FakeRealtimeConnector] — `RealtimeClient(client, connector: fake.connector)`:
///   push frames, close with a code, observe sent frames and ticket placement;
/// * [FakeClock] — `retryOptions: clock.retryOptions()`,
///   `options: clock.reconnectOptions()`: backoff and Retry-After recorded, not slept.
///
/// Not exported by `cfmeet_platform_sdk.dart`; import it from tests only. Uses no
/// `dart:io`, so it also works in browser test runs.
library;

import 'cfmeet_platform_sdk.dart';

export 'src/testing/fake_clock.dart';
export 'src/testing/fake_realtime.dart';
export 'src/testing/fake_transport.dart';
