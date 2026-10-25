/// `dart:io` transport (servers, CLI, Flutter mobile / desktop).
library;

import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'transport.dart';

/// [CfMeetTransport] over a `dart:io` [HttpClient]. Pass an [HttpClient] to control
/// proxies, certificates or connection pooling (and in tests).
class IoTransport implements CfMeetTransport {
  IoTransport(
      {HttpClient? httpClient, this.maxResponseBytes = 32 * 1024 * 1024})
      : _client = httpClient ?? HttpClient(),
        _owned = httpClient == null {
    if (_owned) _client.autoUncompress = true;
  }

  final HttpClient _client;
  final bool _owned;

  /// Responses larger than this fail (a misbehaving server cannot exhaust memory).
  final int maxResponseBytes;

  @override
  Future<TransportResponse> send(TransportRequest r) {
    final result = Completer<TransportResponse>();
    var sent = false;
    HttpClientRequest? request;
    StreamSubscription<List<int>>? bodySub;

    void fail(TransportException e) {
      if (result.isCompleted) return;
      result.completeError(e);
      request?.abort(e);
      unawaited(bodySub?.cancel());
    }

    final timer = Timer(r.timeout, () {
      fail(TransportException(TransportFailure.timeout,
          requestSent: sent, message: 'no response within ${r.timeout}'));
    });
    final cancel = r.cancelToken;
    if (cancel != null) {
      if (cancel.isCancelled) {
        timer.cancel();
        return Future.error(const TransportException(TransportFailure.cancelled,
            requestSent: false));
      }
      unawaited(cancel.whenCancelled.then((_) => fail(TransportException(
          TransportFailure.cancelled,
          requestSent: sent,
          message: cancel.reason ?? ''))));
    }

    Future<void> run() async {
      try {
        final req = await _client.openUrl(r.method, r.url);
        request = req;
        if (result.isCompleted) {
          req.abort();
          return;
        }
        req.followRedirects = false;
        req.persistentConnection = true;
        r.headers.forEach(req.headers.set);
        final body = r.body;
        req.contentLength = body?.length ?? 0;
        // From here on, bytes may reach the server.
        sent = true;
        if (body != null && body.isNotEmpty) req.add(body);
        final resp = await req.close();
        final headers = <String, String>{};
        resp.headers.forEach((name, values) {
          headers[name.toLowerCase()] = values.join(', ');
        });
        final builder = BytesBuilder(copy: false);
        final done = Completer<void>();
        bodySub = resp.listen(
          (chunk) {
            builder.add(chunk);
            if (builder.length > maxResponseBytes) {
              fail(const TransportException(TransportFailure.network,
                  requestSent: true, message: 'response too large'));
            }
          },
          onError: (Object e) {
            if (!done.isCompleted) done.completeError(e);
          },
          onDone: () {
            if (!done.isCompleted) done.complete();
          },
          cancelOnError: true,
        );
        await done.future;
        if (!result.isCompleted) {
          result.complete(TransportResponse(
              status: resp.statusCode,
              headers: headers,
              body: builder.takeBytes()));
        }
      } catch (e) {
        if (e is TransportException) {
          fail(e);
        } else {
          fail(TransportException(TransportFailure.network,
              requestSent: sent, cause: e, message: e.runtimeType.toString()));
        }
      }
    }

    unawaited(run());
    return result.future.whenComplete(timer.cancel);
  }

  @override
  void close({bool force = false}) {
    if (_owned) _client.close(force: force);
  }
}

/// The default transport of this platform.
CfMeetTransport createDefaultTransport() => IoTransport();
