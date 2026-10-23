/// Retry policy by retry class, backoff with jitter and Retry-After.
library;

import 'dart:math';

import 'operation.dart';

/// Statuses retried for [RetryClass.safe] and [RetryClass.idempotent].
const Set<int> retryableStatuses = {408, 429, 502, 503, 504};

/// Statuses after which a [RetryClass.never] operation's outcome is unknown.
const Set<int> unknownOutcomeStatuses = {500, 502, 504};

/// Whether an HTTP [status] answer may be retried for [retry].
bool shouldRetryStatus(RetryClass retry, int status) => switch (retry) {
      RetryClass.safe ||
      RetryClass.idempotent =>
        retryableStatuses.contains(status),
      RetryClass.noRetry => status == 429,
      RetryClass.never => false,
    };

/// Whether a network failure or timeout may be retried for [retry].
bool shouldRetryNetwork(RetryClass retry) =>
    retry == RetryClass.safe || retry == RetryClass.idempotent;

/// Waits [delay] before a retry or a reconnect. The SDK's default waits in real time;
/// tests inject a fake (`FakeClock.sleep` of `package:cfmeet_platform_sdk/testing.dart`)
/// so that backoff and Retry-After are observable without sleeping.
typedef RetrySleeper = Future<void> Function(Duration delay);

/// Backoff configuration.
class RetryOptions {
  const RetryOptions({
    this.maxRetries = 3,
    this.baseDelay = const Duration(milliseconds: 250),
    this.maxDelay = const Duration(seconds: 8),
    this.maxRetryAfter = const Duration(seconds: 60),
    this.sleeper,
  });

  /// Retries after the first attempt (0 disables retries for every class).
  final int maxRetries;

  /// First backoff step; doubled per retry, full jitter.
  final Duration baseDelay;

  /// Ceiling of one computed backoff.
  final Duration maxDelay;

  /// Longest Retry-After the SDK waits for. A server asking for longer is not retried:
  /// the error (with its `retryAfter`) goes to the caller.
  final Duration maxRetryAfter;

  /// Waits between attempts (null: a real-time timer, cancelled by the request's
  /// `CancelToken`). Cancellation still interrupts an injected sleeper's wait.
  final RetrySleeper? sleeper;

  RetryOptions copyWith({int? maxRetries}) => RetryOptions(
        maxRetries: maxRetries ?? this.maxRetries,
        baseDelay: baseDelay,
        maxDelay: maxDelay,
        maxRetryAfter: maxRetryAfter,
        sleeper: sleeper,
      );

  /// Backoff before retry number [retry] (0-based): uniform in
  /// `[0, min(maxDelay, baseDelay * 2^retry)]`.
  Duration backoff(int retry, Random random) {
    final ceiling = min(maxDelay.inMicroseconds,
        baseDelay.inMicroseconds * pow(2, min(retry, 20)).toInt());
    if (ceiling <= 0) return Duration.zero;
    return Duration(microseconds: random.nextInt(ceiling + 1));
  }
}

/// Parses a Retry-After value: delta seconds or an HTTP date (relative to [now]).
/// Null when absent or unparseable; never negative.
Duration? parseRetryAfter(String? value, {DateTime? now}) {
  if (value == null) return null;
  final v = value.trim();
  if (v.isEmpty) return null;
  if (RegExp(r'^\d{1,9}$').hasMatch(v)) return Duration(seconds: int.parse(v));
  final at = parseHttpDate(v);
  if (at == null) return null;
  final d = at.difference(now ?? DateTime.now().toUtc());
  return d.isNegative ? Duration.zero : d;
}

const List<String> _months = [
  'jan',
  'feb',
  'mar',
  'apr',
  'may',
  'jun',
  'jul',
  'aug',
  'sep',
  'oct',
  'nov',
  'dec'
];

/// Parses an HTTP date (RFC 9110 §5.6.7: IMF-fixdate, and the obsolete RFC 850 and
/// asctime forms). Null when malformed. (No dart:io: usable in browser builds.)
DateTime? parseHttpDate(String value) {
  final v = value.trim();
  final imf = RegExp(
          r'^[A-Za-z]{3}, (\d{2}) ([A-Za-z]{3}) (\d{4}) (\d{2}):(\d{2}):(\d{2}) GMT$')
      .firstMatch(v);
  final rfc850 = RegExp(
          r'^[A-Za-z]+, (\d{2})-([A-Za-z]{3})-(\d{2}) (\d{2}):(\d{2}):(\d{2}) GMT$')
      .firstMatch(v);
  final asctime = RegExp(
          r'^[A-Za-z]{3} ([A-Za-z]{3}) +(\d{1,2}) (\d{2}):(\d{2}):(\d{2}) (\d{4})$')
      .firstMatch(v);
  int? day, month, year, h, m, s;
  int? monthOf(String name) {
    final i = _months.indexOf(name.toLowerCase());
    return i < 0 ? null : i + 1;
  }

  if (imf != null) {
    day = int.parse(imf[1]!);
    month = monthOf(imf[2]!);
    year = int.parse(imf[3]!);
    h = int.parse(imf[4]!);
    m = int.parse(imf[5]!);
    s = int.parse(imf[6]!);
  } else if (rfc850 != null) {
    day = int.parse(rfc850[1]!);
    month = monthOf(rfc850[2]!);
    final yy = int.parse(rfc850[3]!);
    year = yy < 70 ? 2000 + yy : 1900 + yy;
    h = int.parse(rfc850[4]!);
    m = int.parse(rfc850[5]!);
    s = int.parse(rfc850[6]!);
  } else if (asctime != null) {
    month = monthOf(asctime[1]!);
    day = int.parse(asctime[2]!);
    h = int.parse(asctime[3]!);
    m = int.parse(asctime[4]!);
    s = int.parse(asctime[5]!);
    year = int.parse(asctime[6]!);
  } else {
    return null;
  }
  if (month == null || day < 1 || day > 31 || h > 23 || m > 59 || s > 60) {
    return null;
  }
  return DateTime.utc(year, month, day, h, m, s);
}
