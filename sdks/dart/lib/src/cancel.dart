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

/// Waits [delay], returning early (with `false`) when [token] is cancelled.
Future<bool> cancellableDelay(Duration delay, CancelToken? token) async {
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
