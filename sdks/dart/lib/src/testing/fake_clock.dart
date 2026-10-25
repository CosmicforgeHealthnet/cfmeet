/// A fake clock for the SDK's waits (retry backoff, Retry-After, reconnect backoff):
/// every wait is recorded and returns at once.
library;

import '../realtime.dart';
import '../retry.dart';

/// Records the delays the SDK asks for and advances virtual time instead of sleeping.
///
/// ```dart
/// final clock = FakeClock();
/// final client = CfMeetClient(..., retryOptions: clock.retryOptions());
/// // ... a 503 with Retry-After: 20 is retried at once; clock.sleeps == [20 s]
/// ```
class FakeClock {
  FakeClock({DateTime? start}) : start = start ?? DateTime.utc(2026);

  /// Virtual time origin.
  final DateTime start;
  Duration _elapsed = Duration.zero;
  final List<Duration> _sleeps = [];

  /// Total virtual time slept.
  Duration get elapsed => _elapsed;

  /// [start] plus [elapsed].
  DateTime get now => start.add(_elapsed);

  /// Every delay requested, in order.
  List<Duration> get sleeps => List.unmodifiable(_sleeps);

  /// A [RetrySleeper]: records [delay], advances [elapsed] and yields to the event loop
  /// once (a zero-length timer, so no real time passes and loops cannot starve it).
  Future<void> sleep(Duration delay) {
    _sleeps.add(delay);
    _elapsed += delay;
    return Future<void>.delayed(Duration.zero);
  }

  /// [RetryOptions] (SDK defaults unless given) whose waits use this clock.
  RetryOptions retryOptions({
    int maxRetries = 3,
    Duration baseDelay = const Duration(milliseconds: 250),
    Duration maxDelay = const Duration(seconds: 8),
    Duration maxRetryAfter = const Duration(seconds: 60),
  }) =>
      RetryOptions(
        maxRetries: maxRetries,
        baseDelay: baseDelay,
        maxDelay: maxDelay,
        maxRetryAfter: maxRetryAfter,
        sleeper: sleep,
      );

  /// [ReconnectOptions] (SDK defaults unless given) whose waits use this clock.
  ReconnectOptions reconnectOptions({
    Duration baseDelay = const Duration(milliseconds: 500),
    Duration maxDelay = const Duration(seconds: 30),
    int? maxConsecutiveFailures,
    Set<int> authFailureCloseCodes = defaultAuthFailureCloseCodes,
  }) =>
      ReconnectOptions(
        baseDelay: baseDelay,
        maxDelay: maxDelay,
        maxConsecutiveFailures: maxConsecutiveFailures,
        authFailureCloseCodes: authFailureCloseCodes,
        sleeper: sleep,
      );
}
