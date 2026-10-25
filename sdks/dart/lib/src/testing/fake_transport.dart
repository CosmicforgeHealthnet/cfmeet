/// A deterministic in-memory [CfMeetTransport] for application tests (Part VI §52.3):
/// scripted responses per catalogue operation or per method + path, recorded requests,
/// deterministic request ids and contract-shaped error envelopes and pages.
library;

import 'dart:async';
import 'dart:convert';

import '../client.dart' show discoveryOperation, findOperation;
import '../generated/operations.dart';
import '../operation.dart';
import '../response.dart';
import '../transport.dart';

/// The request id of the [n]-th request (1-based) a [FakeCfMeetTransport] receives.
typedef FakeRequestIdGenerator = String Function(int n);

/// `req_fake_000001`, `req_fake_000002`, ...
String defaultFakeRequestId(int n) =>
    'req_fake_${n.toString().padLeft(6, '0')}';

/// The public API Gateway's error code for an HTTP [status] (contract
/// `cfmeet.api-gateway.common` `ErrorCode`); `CONFLICT` / `PRECONDITION_FAILED` for
/// 409 / 412 (domain codes), `HTTP_<status>` otherwise.
String fakeErrorCodeFor(int status) => switch (status) {
      400 => 'VALIDATION_ERROR',
      401 => 'AUTHENTICATION_FAILED',
      403 => 'INSUFFICIENT_SCOPE',
      404 => 'NOT_FOUND',
      405 => 'METHOD_NOT_ALLOWED',
      409 => 'CONFLICT',
      412 => 'PRECONDITION_FAILED',
      413 => 'PAYLOAD_TOO_LARGE',
      414 => 'URI_TOO_LONG',
      429 => 'RATE_LIMITED',
      431 => 'HEADERS_TOO_LARGE',
      500 => 'INTERNAL_ERROR',
      502 => 'UPSTREAM_UNAVAILABLE',
      503 => 'SERVICE_UNAVAILABLE',
      504 => 'UPSTREAM_TIMEOUT',
      _ => 'HTTP_$status',
    };

/// The contract error envelope `{"error": {"code", "message", "requestId"}}`.
Map<String, Object?> fakeErrorEnvelope(String code,
        {String? message, String? requestId}) =>
    {
      'error': {
        'code': code,
        'message': message ?? 'fake $code',
        'requestId': requestId,
      },
    };

/// One page of a cursor-paginated operation, shaped by its [PaginationSpec]:
/// `{<itemsField>: items, <nextField>: nextCursor}` plus [extra] fields.
Map<String, Object?> fakePageBody(
    String domain, String name, List<Object?> items,
    {String? nextCursor, Map<String, Object?> extra = const {}}) {
  final p = _paginationOf(domain, name);
  return {...extra, p.itemsField: items, p.nextField: nextCursor};
}

/// The `x-cf-quota-*` response headers carrying [quota].
Map<String, String> fakeQuotaHeaders(QuotaInfo quota) => {
      if (quota.limitMinute != null)
        'x-cf-quota-limit-minute': '${quota.limitMinute}',
      if (quota.remainingMinute != null)
        'x-cf-quota-remaining-minute': '${quota.remainingMinute}',
      if (quota.resetSeconds != null)
        'x-cf-quota-reset': '${quota.resetSeconds}',
      if (quota.limitDay != null) 'x-cf-quota-limit-day': '${quota.limitDay}',
      if (quota.window != null) 'x-cf-quota-window': quota.window!,
      if (quota.level != null) 'x-cf-quota-level': quota.level!,
    };

PaginationSpec _paginationOf(String domain, String name) {
  final op = _restOperation(domain, name);
  final p = op.pagination;
  if (p == null) {
    throw ArgumentError(
        '${op.qualifiedName} is not a cursor-paginated list operation');
  }
  return p;
}

/// The catalogue operation `domain.name` (checked: unknown names throw
/// `CfMeetUsageError` with suggestions; WebSocket channels throw [ArgumentError]).
OperationSpec _restOperation(String domain, String name) {
  if (domain == discoveryOperation.domain && name == discoveryOperation.name) {
    return discoveryOperation;
  }
  final op = findOperation(domain, name);
  if (op.isWebSocket) {
    throw ArgumentError('${op.qualifiedName} is a WebSocket channel: '
        'script it with FakeRealtimeConnector');
  }
  return op;
}

enum _Kind { response, network, timeout }

/// What the fake does with one request: an HTTP answer, a network failure or a timeout.
class FakeResponse {
  /// An HTTP answer whose [body] is JSON-encoded (null: empty body).
  const FakeResponse({
    this.status = 200,
    this.body,
    this.headers = const {},
    this.requestId,
    this.correlationId,
    this.idempotentReplayed = false,
    this.quota,
    this.retryAfter,
  })  : _kind = _Kind.response,
        _raw = null,
        _contentType = null,
        _envelope = false,
        _errorCode = null,
        _errorMessage = null,
        requestSent = true;

  /// A 2xx (or any [status]) JSON answer.
  const FakeResponse.json(
    Object? body, {
    int status = 200,
    Map<String, String> headers = const {},
    String? requestId,
    String? correlationId,
    bool idempotentReplayed = false,
    QuotaInfo? quota,
  }) : this(
          status: status,
          body: body,
          headers: headers,
          requestId: requestId,
          correlationId: correlationId,
          idempotentReplayed: idempotentReplayed,
          quota: quota,
        );

  /// Raw bytes (e.g. a media download), with [contentType].
  const FakeResponse.bytes(
    List<int> bytes, {
    this.status = 200,
    String contentType = 'application/octet-stream',
    this.headers = const {},
    this.requestId,
    this.correlationId,
    this.quota,
  })  : _kind = _Kind.response,
        _raw = bytes,
        _contentType = contentType,
        body = null,
        idempotentReplayed = false,
        retryAfter = null,
        _envelope = false,
        _errorCode = null,
        _errorMessage = null,
        requestSent = true;

  /// A contract error envelope `{"error": {"code", "message", "requestId"}}`; [code]
  /// defaults to [fakeErrorCodeFor] the status, `requestId` to the request's id.
  const FakeResponse.error(
    this.status, {
    String? code,
    String? message,
    this.retryAfter,
    this.quota,
    this.headers = const {},
    this.requestId,
    this.correlationId,
  })  : _kind = _Kind.response,
        _raw = null,
        _contentType = null,
        body = null,
        idempotentReplayed = false,
        _envelope = true,
        _errorCode = code,
        _errorMessage = message,
        requestSent = true;

  /// One page of the cursor-paginated operation `domain.name`
  /// (`{<itemsField>: items, <nextField>: nextCursor}`); throws for an operation without
  /// a pagination spec.
  factory FakeResponse.page(
    String domain,
    String name,
    List<Object?> items, {
    String? nextCursor,
    Map<String, Object?> extra = const {},
    Map<String, String> headers = const {},
    QuotaInfo? quota,
  }) =>
      FakeResponse.json(
          fakePageBody(domain, name, items,
              nextCursor: nextCursor, extra: extra),
          headers: headers,
          quota: quota);

  /// The connection failed. [requestSent] false: before any byte reached the server
  /// (DNS, refused); true: after (reset) — a NEVER operation's outcome is then unknown.
  const FakeResponse.networkError({this.requestSent = true})
      : _kind = _Kind.network,
        status = 0,
        body = null,
        headers = const {},
        requestId = null,
        correlationId = null,
        idempotentReplayed = false,
        quota = null,
        retryAfter = null,
        _raw = null,
        _contentType = null,
        _envelope = false,
        _errorCode = null,
        _errorMessage = null;

  /// No answer within the attempt's deadline (reported at once: no time passes).
  const FakeResponse.timeout({this.requestSent = true})
      : _kind = _Kind.timeout,
        status = 0,
        body = null,
        headers = const {},
        requestId = null,
        correlationId = null,
        idempotentReplayed = false,
        quota = null,
        retryAfter = null,
        _raw = null,
        _contentType = null,
        _envelope = false,
        _errorCode = null,
        _errorMessage = null;

  final _Kind _kind;
  final int status;

  /// JSON-encoded body (ignored for errors, bytes, network failures and timeouts).
  final Object? body;

  /// Extra / overriding response headers (any case).
  final Map<String, String> headers;

  /// `x-request-id` (default: the fake's deterministic id of this request).
  final String? requestId;
  final String? correlationId;

  /// Sends `idempotent-replayed: true`.
  final bool idempotentReplayed;

  /// Sent as the `x-cf-quota-*` headers.
  final QuotaInfo? quota;

  /// Sent as `Retry-After` (whole seconds, rounded up).
  final Duration? retryAfter;

  /// For [FakeResponse.networkError] / [FakeResponse.timeout].
  final bool requestSent;

  final List<int>? _raw;
  final String? _contentType;
  final bool _envelope;
  final String? _errorCode;
  final String? _errorMessage;

  bool get isNetworkError => _kind == _Kind.network;
  bool get isTimeout => _kind == _Kind.timeout;

  TransportResponse _toTransport(String generatedId) {
    final id = requestId ?? generatedId;
    final out = <String, String>{'x-request-id': id};
    List<int> bytes;
    if (_raw != null) {
      bytes = _raw;
      out['content-type'] = _contentType!;
    } else if (_envelope) {
      bytes = utf8.encode(jsonEncode(fakeErrorEnvelope(
          _errorCode ?? fakeErrorCodeFor(status),
          message: _errorMessage,
          requestId: id)));
      out['content-type'] = 'application/json';
      out['cache-control'] = 'no-store';
    } else if (body != null) {
      bytes = utf8.encode(jsonEncode(body));
      out['content-type'] = 'application/json';
    } else {
      bytes = const [];
    }
    if (correlationId != null) out['x-correlation-id'] = correlationId!;
    if (idempotentReplayed) out['idempotent-replayed'] = 'true';
    if (quota != null) out.addAll(fakeQuotaHeaders(quota!));
    final ra = retryAfter;
    if (ra != null) {
      final seconds = (ra.inMilliseconds + 999) ~/ 1000;
      out['retry-after'] = '${seconds < 0 ? 0 : seconds}';
    }
    headers.forEach((k, v) => out[k.toLowerCase()] = v);
    return TransportResponse(status: status, headers: out, body: bytes);
  }

  @override
  String toString() => switch (_kind) {
        _Kind.network => 'FakeResponse.networkError(requestSent: $requestSent)',
        _Kind.timeout => 'FakeResponse.timeout(requestSent: $requestSent)',
        _Kind.response => 'FakeResponse($status)',
      };
}

/// One request the fake received.
class RecordedRequest {
  RecordedRequest._({
    required this.index,
    required this.requestId,
    required this.method,
    required this.url,
    required this.path,
    required this.headers,
    required this.bodyBytes,
    required this.timeout,
    required this.operation,
    required this.pathParams,
  });

  /// 1-based position among every request the fake received.
  final int index;

  /// The id the fake answered with (`x-request-id`, envelope `requestId`).
  final String requestId;
  final String method;
  final Uri url;

  /// The URL path without the fake's `pathPrefix`.
  final String path;

  /// Request headers, names lower-case (including `authorization`, `idempotency-key`).
  final Map<String, String> headers;
  final List<int>? bodyBytes;

  /// The attempt deadline the client set.
  final Duration timeout;

  /// The catalogue operation the method and path belong to, if any.
  final OperationSpec? operation;

  /// Decoded path parameters of [operation] (`{'messageId': 'm1'}`).
  final Map<String, String> pathParams;

  /// Query parameters (first value of each).
  Map<String, String> get query => url.queryParameters;
  Map<String, List<String>> get queryAll => url.queryParametersAll;
  String? get idempotencyKey => headers['idempotency-key'];

  /// `domain.name` of [operation], or null.
  String? get operationName => operation?.qualifiedName;

  /// The body decoded: JSON when the content type says so, otherwise UTF-8 text (null
  /// when there is none).
  Object? get body {
    final b = bodyBytes;
    if (b == null || b.isEmpty) return null;
    final text = utf8.decode(b, allowMalformed: true);
    if ((headers['content-type'] ?? '').contains('json')) {
      try {
        return jsonDecode(text);
      } on FormatException {
        return text;
      }
    }
    return text;
  }

  /// [body] as a JSON object (throws [StateError] otherwise).
  Map<String, dynamic> get json {
    final b = body;
    if (b is Map<String, dynamic>) return b;
    throw StateError('request body is not a JSON object');
  }

  @override
  String toString() => // never header values (credentials)
      'RecordedRequest(#$index $method $path${operation == null ? '' : ' ${operation!.qualifiedName}'})';
}

/// Produces the answer to one request (may be asynchronous, e.g. gated by a test).
typedef FakeResponder = FutureOr<FakeResponse> Function(
    RecordedRequest request);

/// The script of one registered route. Replies are used in order; the last one repeats
/// once the others are used up.
class FakeRoute {
  FakeRoute._(this.description);

  /// `sms.getMessage` or `GET /v1/...`.
  final String description;
  final List<FakeResponder> _script = [];
  int _used = 0;

  /// The requests this route answered.
  final List<RecordedRequest> requests = [];

  /// Appends [response] to the script.
  FakeRoute thenReply(FakeResponse response) => thenReplyWith((_) => response);

  /// Appends a computed reply to the script.
  FakeRoute thenReplyWith(FakeResponder responder) {
    _script.add(responder);
    return this;
  }

  /// Number of requests answered.
  int get callCount => requests.length;

  /// Whether every scripted reply has been used at least once.
  bool get isExhausted => _used >= _script.length;

  FakeResponder? _nextResponder() {
    if (_script.isEmpty) return null;
    final r = _script[_used < _script.length ? _used : _script.length - 1];
    _used++;
    return r;
  }

  @override
  String toString() => 'FakeRoute($description)';
}

class _Template {
  _Template(String template)
      : names = [
          for (final m in RegExp(r'\{([^}/]+)\}').allMatches(template)) m[1]!
        ],
        literals = template
            .split('/')
            .where((s) => s.isNotEmpty && !s.contains('{'))
            .length,
        regex = RegExp(
            '^${template.split(RegExp(r'\{[^}/]+\}')).map(RegExp.escape).join('([^/]+)')}\$');

  final List<String> names;
  final int literals;
  final RegExp regex;

  Map<String, String>? match(String path) {
    final m = regex.firstMatch(path);
    if (m == null) return null;
    return {
      for (var i = 0; i < names.length; i++)
        names[i]: Uri.decodeComponent(m[i + 1]!),
    };
  }
}

class _RequestRoute {
  _RequestRoute(this.method, this.template, this.route);
  final String method;
  final _Template template;
  final FakeRoute route;
}

List<(OperationSpec, _Template)>? _catalogue;

List<(OperationSpec, _Template)> get _restCatalogue => _catalogue ??= [
      for (final op in [...operations, discoveryOperation])
        if (!op.isWebSocket) (op, _Template(op.path)),
    ];

/// A deterministic in-memory transport: pass it as `CfMeetClient(transport: ...)`.
///
/// ```dart
/// final fake = FakeCfMeetTransport();
/// fake.onOperation('sms', 'getMessage', FakeResponse.json({'id': 'm1'}));
/// final client = CfMeetClient(baseUrl: 'https://api.cfmeet.test', auth: ..., transport: fake);
/// ```
///
/// Requests without a registered route answer the gateway's default-deny 404
/// `NOT_FOUND` envelope (and are listed in [unmatched]). No network, no timers.
class FakeCfMeetTransport implements CfMeetTransport {
  FakeCfMeetTransport({
    this.pathPrefix = '',
    FakeRequestIdGenerator requestIds = defaultFakeRequestId,
  }) : _requestIds = requestIds;

  /// A base-URL path prefix stripped before matching (`/gateway` for
  /// `https://host/gateway`).
  final String pathPrefix;
  final FakeRequestIdGenerator _requestIds;

  final Map<String, FakeRoute> _operationRoutes = {};
  final List<_RequestRoute> _requestRoutes = [];
  final List<RecordedRequest> _requests = [];
  final List<RecordedRequest> _unmatched = [];
  bool _closed = false;

  /// Every request received, in order.
  List<RecordedRequest> get requests => List.unmodifiable(_requests);

  /// Requests no route answered (they got the default 404).
  List<RecordedRequest> get unmatched => List.unmodifiable(_unmatched);

  /// Whether the client closed the transport.
  bool get isClosed => _closed;

  /// The requests of the catalogue operation `domain.name` (checked).
  List<RecordedRequest> requestsFor(String domain, String name) {
    final op = _restOperation(domain, name);
    return _requests.where((r) => r.operation == op).toList();
  }

  /// Scripts the catalogue operation `domain.name` (validated against `operations`:
  /// an unknown name throws `CfMeetUsageError`). Registering it again replaces the
  /// script. Add replies with [FakeRoute.thenReply] / [FakeRoute.thenReplyWith].
  FakeRoute onOperation(String domain, String name, [FakeResponse? reply]) {
    final op = _restOperation(domain, name);
    final route = FakeRoute._(op.qualifiedName);
    if (reply != null) route.thenReply(reply);
    _operationRoutes[op.qualifiedName] = route;
    return route;
  }

  /// Scripts a method and path (a literal path or a template with `{param}`
  /// placeholders). Takes precedence over [onOperation]; the latest registration wins.
  FakeRoute onRequest(String method, String path, [FakeResponse? reply]) {
    final m = method.toUpperCase();
    if (!const {'GET', 'POST', 'PUT', 'PATCH', 'DELETE'}.contains(m)) {
      throw ArgumentError.value(method, 'method', 'not an HTTP method');
    }
    if (!path.startsWith('/') || path.contains('?')) {
      throw ArgumentError.value(
          path, 'path', 'must start with / and carry no query');
    }
    final route = FakeRoute._('$m $path');
    if (reply != null) route.thenReply(reply);
    _requestRoutes.add(_RequestRoute(m, _Template(path), route));
    return route;
  }

  /// Scripts the cursor-paginated operation `domain.name` to serve [pages] in order
  /// according to its pagination spec: the first request (no cursor) gets page 1 with
  /// `nextCursor: 'fake_cursor_1'`, the request carrying that cursor gets page 2, ...;
  /// the last page has no next cursor. An unknown cursor answers 400 VALIDATION_ERROR.
  FakeRoute onPages(String domain, String name, List<List<Object?>> pages) {
    final p = _paginationOf(domain, name);
    String cursorOf(int i) => 'fake_cursor_$i';
    return onOperation(domain, name).thenReplyWith((req) {
      final cursor = req.query[p.cursorParam];
      var i = 0;
      if (cursor != null) {
        i = int.tryParse(cursor.replaceFirst('fake_cursor_', '')) ?? -1;
        if (cursor != cursorOf(i) || i < 1 || i >= pages.length) {
          return const FakeResponse.error(400,
              code: 'VALIDATION_ERROR', message: 'unknown cursor');
        }
      }
      return FakeResponse.page(domain, name, pages.isEmpty ? [] : pages[i],
          nextCursor: i + 1 < pages.length ? cursorOf(i + 1) : null);
    });
  }

  /// Forgets routes and recorded requests and restarts request ids at 1.
  void reset() {
    _operationRoutes.clear();
    _requestRoutes.clear();
    _requests.clear();
    _unmatched.clear();
    _closed = false;
  }

  @override
  Future<TransportResponse> send(TransportRequest request) async {
    if (_closed) throw StateError('FakeCfMeetTransport is closed');
    final cancel = request.cancelToken;
    if (cancel != null && cancel.isCancelled) {
      throw const TransportException(TransportFailure.cancelled,
          requestSent: false);
    }
    var path = request.url.path;
    if (pathPrefix.isNotEmpty && path.startsWith(pathPrefix)) {
      path = path.substring(pathPrefix.length);
      if (path.isEmpty) path = '/';
    }

    OperationSpec? op;
    var params = const <String, String>{};
    var best = -1;
    for (final (spec, t) in _restCatalogue) {
      if (spec.method != request.method || t.literals <= best) continue;
      final m = t.match(path);
      if (m != null) {
        op = spec;
        params = m;
        best = t.literals;
      }
    }

    FakeRoute? route;
    for (final r in _requestRoutes.reversed) {
      if (r.method == request.method && r.template.match(path) != null) {
        route = r.route;
        break;
      }
    }
    if (route == null && op != null) route = _operationRoutes[op.qualifiedName];

    final rec = RecordedRequest._(
      index: _requests.length + 1,
      requestId: _requestIds(_requests.length + 1),
      method: request.method,
      url: request.url,
      path: path,
      headers: Map.unmodifiable(request.headers),
      bodyBytes: request.body == null ? null : List.unmodifiable(request.body!),
      timeout: request.timeout,
      operation: op,
      pathParams: Map.unmodifiable(params),
    );
    _requests.add(rec);
    route?.requests.add(rec);

    final responder = route?._nextResponder();
    final FakeResponse reply;
    if (responder == null) {
      _unmatched.add(rec);
      reply = FakeResponse.error(404,
          code: 'NOT_FOUND',
          message: route == null
              ? 'no fake response registered for ${request.method} $path'
              : 'no reply scripted for ${route.description}');
    } else {
      reply = await responder(rec);
    }
    if (cancel != null && cancel.isCancelled) {
      throw TransportException(TransportFailure.cancelled,
          requestSent: true, message: cancel.reason ?? '');
    }
    if (reply.isNetworkError) {
      throw TransportException(TransportFailure.network,
          requestSent: reply.requestSent, message: 'fake network error');
    }
    if (reply.isTimeout) {
      throw TransportException(TransportFailure.timeout,
          requestSent: reply.requestSent, message: 'fake timeout');
    }
    return reply._toTransport(rec.requestId);
  }

  @override
  void close({bool force = false}) => _closed = true;
}
