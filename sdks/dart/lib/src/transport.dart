/// The HTTP transport the client sends requests through. The default is
/// [IoTransport] (`dart:io`) on native platforms; browser builds pass their own.
library;

import 'cancel.dart';

/// One HTTP request as the client built it.
class TransportRequest {
  const TransportRequest({
    required this.method,
    required this.url,
    required this.headers,
    required this.body,
    required this.timeout,
    this.cancelToken,
  });

  final String method;
  final Uri url;

  /// Header names lower-case.
  final Map<String, String> headers;
  final List<int>? body;

  /// Deadline of the whole exchange (connect, send, response headers and body).
  final Duration timeout;
  final CancelToken? cancelToken;
}

/// The raw answer.
class TransportResponse {
  const TransportResponse({
    required this.status,
    required this.headers,
    required this.body,
  });

  final int status;

  /// Header names lower-case; repeated headers joined with `, `.
  final Map<String, String> headers;
  final List<int> body;
}

enum TransportFailure { network, timeout, cancelled }

/// A request that produced no HTTP response.
class TransportException implements Exception {
  const TransportException(this.kind,
      {required this.requestSent, this.cause, this.message = ''});

  final TransportFailure kind;

  /// False only when the failure happened before any request byte could reach the
  /// server (DNS, connection refused, TLS handshake, a deadline hit while connecting).
  final bool requestSent;
  final Object? cause;
  final String message;

  @override
  String toString() =>
      'TransportException(${kind.name}, requestSent: $requestSent${message.isEmpty ? '' : ', $message'})';
}

/// Sends HTTP requests. Implementations must not follow redirects (a redirect would carry
/// the credential to another origin) and must throw [TransportException] for every
/// failure without a response.
abstract interface class CfMeetTransport {
  Future<TransportResponse> send(TransportRequest request);

  /// Releases connections. [force] aborts requests in flight.
  void close({bool force = false});
}
