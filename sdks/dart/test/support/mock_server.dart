import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// One request the mock server received.
class Recorded {
  Recorded(this.method, this.uri, this.headers, this.body);

  final String method;

  /// The raw request target (path + query, still percent-encoded).
  final String uri;
  final Map<String, String> headers;
  final String body;

  String get path => uri.split('?').first;
  Map<String, List<String>> get query => Uri.parse(uri).queryParametersAll;
}

typedef Handler = FutureOr<void> Function(
    HttpRequest request, Recorded recorded, int n);

/// A loopback HTTP server answering with a programmable handler and recording every
/// request.
class MockServer {
  MockServer._(this._server);

  static Future<MockServer> start([Handler? handler]) async {
    final s =
        MockServer._(await HttpServer.bind(InternetAddress.loopbackIPv4, 0));
    s.handler = handler ?? (req, rec, n) => s.json(req, 200, {'ok': true});
    s._server.listen(s._handle);
    return s;
  }

  final HttpServer _server;
  late Handler handler;
  final List<Recorded> requests = [];

  String get url => 'http://127.0.0.1:${_server.port}';

  Future<void> _handle(HttpRequest req) async {
    final body = await utf8.decodeStream(req);
    final headers = <String, String>{};
    req.headers.forEach((k, v) => headers[k.toLowerCase()] = v.join(', '));
    final rec = Recorded(req.method, req.uri.toString(), headers, body);
    requests.add(rec);
    try {
      await handler(req, rec, requests.length);
    } catch (_) {
      // The client may have gone away (timeouts, cancellation).
    }
  }

  /// Answers with a JSON body.
  Future<void> json(HttpRequest req, int status, Object? body,
      {Map<String, String> headers = const {}}) async {
    req.response.statusCode = status;
    req.response.headers.contentType = ContentType.json;
    headers.forEach(req.response.headers.set);
    req.response.write(jsonEncode(body));
    await req.response.close();
  }

  /// Answers with the gateway error envelope.
  Future<void> error(HttpRequest req, int status, String code,
          {Map<String, String> headers = const {}}) =>
      json(req, status, {
        'error': {
          'code': code,
          'message': 'msg $code',
          'requestId': 'req-$status'
        }
      }, headers: {
        'x-request-id': 'req-$status',
        ...headers
      });

  /// Drops the connection after the request was received (no response).
  Future<void> drop(HttpRequest req) async {
    final socket = await req.response.detachSocket(writeHeaders: false);
    socket.destroy();
  }

  Future<void> close() => _server.close(force: true);
}
