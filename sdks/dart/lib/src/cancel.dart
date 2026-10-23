/// Cooperative cancellation of requests, retries, pagination and realtime connections.
library;

import 'dart:async';

/// Cancels the work it is passed to. One token can cancel several requests.
class CancelToken {
  final Completer<void> _cancelled = Completer<void>();
  String? _reason;

  bool get isCancelled => _cancelled.isCompleted;
  String? get reason => _reason;

  /// Completes when [cancel] is called.
  Future<void> get whenCancelled => _cancelled.future;

  void cancel([String reason = 'cancelled']) {
    if (_cancelled.isCompleted) return;
    _reason = reason;
    _cancelled.complete();
  }
}

/// Waits [delay], returning early (with `false`) when [token] is cancelled. [sleeper]
/// replaces the real-time timer (tests: a fake clock); cancellation still wins.
Future<bool> cancellableDelay(Duration delay, CancelToken? token,
    {Future<void> Function(Duration delay)? sleeper}) async {
  if (sleeper != null) {
    if (token == null) {
      await sleeper(delay);
      return true;
    }
    if (token.isCancelled) return false;
    final slept = await Future.any([
      sleeper(delay).then((_) => true),
      token.whenCancelled.then((_) => false),
    ]);
    return slept && !token.isCancelled;
  }
  if (token == null) {
    await Future<void>.delayed(delay);
    return true;
  }
  if (token.isCancelled) return false;
  final timer = Completer<bool>();
  final t = Timer(delay, () {
    if (!timer.isCompleted) timer.complete(true);
  });
  unawaited(token.whenCancelled.then((_) {
    t.cancel();
    if (!timer.isCompleted) timer.complete(false);
  }));
  return timer.future;
}
