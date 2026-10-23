/// The CF Meet platform API client: every public operation of the generated catalogue,
/// with authentication, safe request building, typed errors, retries by retry class,
/// idempotency, timeouts, cancellation and cursor pagination.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'auth.dart';
import 'cancel.dart';
import 'errors.dart';
import 'generated/operations.dart';
import 'idempotency.dart';
import 'operation.dart';
import 'response.dart';
import 'retry.dart';
import 'transport.dart';
import 'transport_io.dart' if (dart.library.js_interop) 'transport_stub.dart'
    as platform_transport;

/// A request body that is not JSON (e.g. sealed bytes uploaded with PUT).
class RawBody {
  const RawBody(this.bytes, {this.contentType = 'application/octet-stream'});

  final List<int> bytes;
  final String contentType;
}

/// One retry the client is about to make (for logging / metrics).
class RetryEvent {
  const RetryEvent({
    required this.operation,
    required this.attempt,
    required this.delay,
    required this.reason,
    this.idempotencyKey,
  });

  /// `domain.name`.
  final String operation;

  /// The attempt that failed (1-based).
  final int attempt;
  final Duration delay;

  /// `HTTP <status>`, `network` or `timeout`.
  final String reason;
  final String? idempotencyKey;
}

/// Headers a caller may add to any operation (forwarded by the gateway), besides the
/// operation's documented headers.
const Set<String> commonRequestHeaders = {
  'if-match',
  'if-none-match',
  'accept-language',
  'range',
};

/// The gateway's public discovery document (`GET /discovery`, anonymous).
const OperationSpec discoveryOperation = OperationSpec(
  domain: 'gateway',
  name: 'discovery',
  operationId: 'discovery',
  method: 'GET',
  path: '/discovery',
  protocol: 'REST',
  auth: <String>['ANONYMOUS'],
  scope: null,
  pathParams: <String>[],
  query: <ParamSpec>[],
  headers: <ParamSpec>[],
  body: 'NONE',
  idempotency: 'NONE',
  retry: RetryClass.safe,
  paidTelecom: false,
  pagination: null,
  success: <int>[200],
);

Map<String, Map<String, OperationSpec>>? _index;

/// The catalogue by domain and name.
Map<String, Map<String, OperationSpec>> get operationIndex {
  return _index ??= () {
    final m = <String, Map<String, OperationSpec>>{};
    for (final op in operations) {
      (m[op.domain] ??= <String, OperationSpec>{})[op.name] = op;
    }
    return m;
  }();
}

/// Looks up `domain.name` in the catalogue; throws [CfMeetUsageError] (with suggestions)
/// when it does not exist.
OperationSpec findOperation(String domain, String name) {
  final ops = operationIndex[domain];
  if (ops == null) {
    throw CfMeetUsageError('unknown API domain "$domain" '
        '(known: ${(operationIndex.keys.toList()..sort()).join(', ')})');
  }
  final op = ops[name];
  if (op != null) return op;
  final close = ops.keys
      .where((k) => _distance(k.toLowerCase(), name.toLowerCase()) <= 3)
      .toList()
    ..sort();
  throw CfMeetUsageError('unknown operation "$domain.$name"'
      '${close.isEmpty ? '' : ' (did you mean ${close.take(3).join(', ')}?)'}');
}

int _distance(String a, String b) {
  var prev = List<int>.generate(b.length + 1, (i) => i);
  for (var i = 1; i <= a.length; i++) {
    final cur = List<int>.filled(b.length + 1, 0)..[0] = i;
    for (var j = 1; j <= b.length; j++) {
      cur[j] = [
        prev[j] + 1,
        cur[j - 1] + 1,
        prev[j - 1] + (a[i - 1] == b[j - 1] ? 0 : 1),
      ].reduce(min);
    }
    prev = cur;
  }
  return prev[b.length];
}

/// Fills [op]'s path template with [pathParams], percent-encoding each value. Refuses a
/// missing, extra or empty parameter and any value that could change the path's shape
/// (`/`, `\`, `..`, control characters).
String buildPath(OperationSpec op, Map<String, Object?> pathParams) {
  for (final k in pathParams.keys) {
    if (!op.pathParams.contains(k)) {
      throw CfMeetUsageError('${op.qualifiedName} has no path parameter "$k"');
    }
  }
  return op.path.replaceAllMapped(RegExp(r'\{([^}/]+)\}'), (m) {
    final name = m[1]!;
    final raw = pathParams[name];
    if (raw == null) {
      throw CfMeetUsageError(
          '${op.qualifiedName}: missing path parameter "$name"');
    }
    final value = raw.toString();
    if (value.isEmpty ||
        value.contains('/') ||
        value.contains(r'\') ||
        value.contains('..') ||
        value == '.' ||
        value.codeUnits.any((c) => c < 0x20 || c == 0x7f)) {
      throw CfMeetUsageError(
          '${op.qualifiedName}: unsafe value for path parameter "$name"');
    }
    return Uri.encodeComponent(value);
  });
}

String _queryString(OperationSpec op, Map<String, Object?> query) {
  final allowed = {for (final q in op.query) q.name: q};
  for (final k in query.keys) {
    if (!allowed.containsKey(k)) {
      throw CfMeetUsageError('${op.qualifiedName} has no query parameter "$k" '
          '(allowed: ${allowed.keys.join(', ')})');
    }
  }
  for (final q in op.query) {
    if (q.required && query[q.name] == null) {
      throw CfMeetUsageError(
          '${op.qualifiedName}: missing query parameter "${q.name}"');
    }
  }
  final parts = <String>[];
  for (final e in query.entries) {
    final v = e.value;
    if (v == null) continue;
    final values = v is Iterable ? v : [v];
    for (final item in values) {
      if (item == null) continue;
      parts.add(
          '${Uri.encodeQueryComponent(e.key)}=${Uri.encodeQueryComponent(item.toString())}');
    }
  }
  return parts.join('&');
}

bool _headerValueSafe(String v) =>
    v.length <= 4096 && v.codeUnits.every((c) => c >= 0x20 && c <= 0x7e);

/// The CF Meet platform API client.
///
/// ```dart
/// final client = CfMeetClient(
///   baseUrl: 'https://api.cfmeet.com',
///   auth: CfMeetAuth.platformCredential(Platform.environment['CFMEET_CREDENTIAL']!),
/// );
/// final sent = await client.sms.call('sendMessage', body: {...});
/// ```
class CfMeetClient {
  CfMeetClient({
    required String baseUrl,
    this.auth,
    this.timeout = const Duration(seconds: 30),
    int? maxRetries,
    RetryOptions retryOptions = const RetryOptions(),
    CfMeetTransport? transport,
    this.allowPlatformCredentialInBrowser = false,
    this.allowInsecureHttp = false,
    this.userAgent = 'cfmeet-platform-sdk-dart/0.1.0',
    this.onRetry,
    Random? random,
  })  : baseUri = _parseBase(baseUrl, allowInsecureHttp),
        retryOptions = maxRetries == null
            ? retryOptions
            : retryOptions.copyWith(maxRetries: maxRetries),
        // The browser credential policy is enforced before anything else (including
        // creating a transport).
        _transport = _policyChecked(auth, allowPlatformCredentialInBrowser) ??
            transport ??
            platform_transport.createDefaultTransport(),
        _random = random ?? Random.secure() {
    enforceCredentialPolicy(auth,
        allowInBrowser: allowPlatformCredentialInBrowser);
    if (this.retryOptions.maxRetries < 0) {
      throw const CfMeetUsageError('maxRetries must be >= 0');
    }
  }

  static CfMeetTransport? _policyChecked(
      CfMeetAuth? auth, bool allowInBrowser) {
    enforceCredentialPolicy(auth, allowInBrowser: allowInBrowser);
    return null;
  }

  static Uri _parseBase(String baseUrl, bool allowInsecure) {
    final u = Uri.tryParse(baseUrl);
    if (u == null ||
        !u.hasScheme ||
        u.host.isEmpty ||
        u.hasQuery ||
        u.hasFragment) {
      throw const CfMeetUsageError(
          'baseUrl must be an absolute http(s) URL without query');
    }
    if (u.userInfo.isNotEmpty) {
      throw const CfMeetUsageError('baseUrl must not carry user info');
    }
    final local =
        u.host == 'localhost' || u.host == '127.0.0.1' || u.host == '::1';
    if (u.scheme == 'http' && !local && !allowInsecure) {
      throw const CfMeetUsageError(
          'baseUrl must use https (http is accepted for localhost, or with allowInsecureHttp)');
    }
    if (u.scheme != 'https' && u.scheme != 'http') {
      throw const CfMeetUsageError('baseUrl must use https');
    }
    return u;
  }

  /// The API origin (and optional path prefix).
  final Uri baseUri;

  /// Default authentication (per-request `auth:` overrides it).
  final CfMeetAuth? auth;

  /// Deadline of one HTTP attempt.
  final Duration timeout;
  final RetryOptions retryOptions;
  final bool allowPlatformCredentialInBrowser;
  final bool allowInsecureHttp;
  final String userAgent;
  final void Function(RetryEvent event)? onRetry;
  final CfMeetTransport _transport;
  final Random _random;

  int get maxRetries => retryOptions.maxRetries;

  /// An operation of the catalogue by domain and name (checked).
  BoundOperation operation(String domain, String name) =>
      BoundOperation._(this, findOperation(domain, name));

  /// The operations of one API domain (checked).
  DomainClient domain(String domain) {
    if (!operationIndex.containsKey(domain)) findOperation(domain, '');
    return DomainClient._(this, domain);
  }

  DomainClient get calling => domain('calling');
  DomainClient get developerPlatform => domain('developer-platform');
  DomainClient get gateway => domain('gateway');
  DomainClient get meetings => domain('meetings');
  DomainClient get messaging => domain('messaging');
  DomainClient get numbering => domain('numbering');
  DomainClient get privacyRelay => domain('privacy-relay');
  DomainClient get sms => domain('sms');

  /// `GET /discovery`: the gateway's public discovery document (no credential sent).
  Future<ApiResponse> discovery({CancelToken? cancelToken}) =>
      request(discoveryOperation, cancelToken: cancelToken);

  /// Sends [op].
  ///
  /// * [pathParams] fill the path template (encoded; `/`, `\`, `..` refused);
  /// * [query] only the operation's documented query parameters (Iterable values repeat);
  /// * [headers] only the operation's documented headers and [commonRequestHeaders];
  /// * [body] JSON-encodable value or [RawBody];
  /// * [idempotencyKey] sent as Idempotency-Key when the operation takes one; generated
  ///   (random UUID) when omitted, and reused unchanged by every retry.
  ///
  /// Throws a [CfMeetApiError] subclass for an error answer, [UnknownOutcomeError] when a
  /// NEVER-retried operation may have happened, [CfMeetTimeoutError],
  /// [CfMeetNetworkError], [CfMeetCancelledError] or [CfMeetUsageError].
  Future<ApiResponse> request(
    OperationSpec op, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    Object? body,
    String? idempotencyKey,
    CancelToken? cancelToken,
    Duration? timeout,
    CfMeetAuth? auth,
  }) async {
    if (op.isWebSocket) {
      throw CfMeetUsageError(
          '${op.qualifiedName} is a WebSocket channel: use RealtimeClient');
    }
    final path = buildPath(op, pathParams);
    final qs = _queryString(op, query);
    final allowedHeaders = {
      for (final h in op.headers) h.name,
      ...commonRequestHeaders,
    };
    final sendHeaders = <String, String>{};
    headers.forEach((k, v) {
      final name = k.toLowerCase();
      if (!allowedHeaders.contains(name)) {
        throw CfMeetUsageError(
            '${op.qualifiedName}: header "$name" is not allowed '
            '(allowed: ${allowedHeaders.join(', ')}; authentication and '
            'Idempotency-Key have their own parameters)');
      }
      if (!_headerValueSafe(v)) {
        throw CfMeetUsageError(
            '${op.qualifiedName}: invalid value for header "$name"');
      }
      sendHeaders[name] = v;
    });
    for (final h in op.headers) {
      if (h.required && !sendHeaders.containsKey(h.name)) {
        throw CfMeetUsageError(
            '${op.qualifiedName}: missing header "${h.name}"');
      }
    }

    List<int>? bytes;
    if (body != null) {
      if (op.body == 'NONE') {
        throw CfMeetUsageError('${op.qualifiedName} takes no request body');
      }
      if (body is RawBody) {
        bytes = body.bytes;
        sendHeaders['content-type'] = body.contentType;
      } else {
        bytes = utf8.encode(jsonEncode(body));
        sendHeaders['content-type'] = 'application/json';
      }
    } else if (op.body == 'REQUIRED') {
      throw CfMeetUsageError('${op.qualifiedName} requires a request body');
    }

    String? key;
    if (op.takesIdempotencyKey) {
      key = idempotencyKey ?? newIdempotencyKey();
      if (!isValidIdempotencyKey(key)) {
        throw const CfMeetUsageError(
            'invalid Idempotency-Key (1-255 visible ASCII characters)');
      }
      sendHeaders['idempotency-key'] = key;
    } else if (idempotencyKey != null) {
      throw CfMeetUsageError(
          '${op.qualifiedName} does not take an Idempotency-Key');
    }

    final effective = _authFor(op, auth ?? this.auth);
    if (auth != null) {
      enforceCredentialPolicy(auth,
          allowInBrowser: allowPlatformCredentialInBrowser);
    }

    final prefix = baseUri.path.endsWith('/')
        ? baseUri.path.substring(0, baseUri.path.length - 1)
        : baseUri.path;
    final url = Uri.parse('${baseUri.scheme}://${baseUri.authority}$prefix$path'
        '${qs.isEmpty ? '' : '?$qs'}');
    sendHeaders['accept'] = 'application/json';
    sendHeaders['user-agent'] = userAgent;

    return _execute(op, url, sendHeaders, bytes, key, effective,
        timeout ?? this.timeout, cancelToken);
  }

  /// The authentication to send for [op], or null for none. Refuses (before anything is
  /// sent) a configured mode the operation does not accept, so a credential never goes
  /// to a surface that does not expect it.
  CfMeetAuth? _authFor(OperationSpec op, CfMeetAuth? a) {
    if (a == null || a.mode == AuthMode.anonymous) return null;
    if (op.auth.contains(a.mode.wire)) return a;
    if (op.auth.contains('ANONYMOUS')) return null;
    throw CfMeetUsageError(
        '${op.qualifiedName} accepts ${op.auth.join(' / ')}, '
        'not ${a.mode.wire}: pass auth: for this call');
  }

  Future<ApiResponse> _execute(
    OperationSpec op,
    Uri url,
    Map<String, String> headers,
    List<int>? body,
    String? key,
    CfMeetAuth? auth,
    Duration timeout,
    CancelToken? cancel,
  ) async {
    final retries = op.retry == RetryClass.never ? 0 : retryOptions.maxRetries;
    for (var attempt = 1;; attempt++) {
      if (cancel?.isCancelled ?? false) {
        throw CfMeetCancelledError('cancelled before attempt $attempt',
            idempotencyKey: key);
      }
      final h = {...headers, if (auth != null) ...await auth.headers()};
      Duration? delay;
      String reason;
      try {
        final resp = await _transport.send(TransportRequest(
            method: op.method,
            url: url,
            headers: h,
            body: body,
            timeout: timeout,
            cancelToken: cancel));
        final decoded = _decode(resp);
        if (resp.status >= 200 && resp.status < 300) {
          return ApiResponse(
            status: resp.status,
            body: decoded,
            headers: resp.headers,
            requestId: resp.headers['x-request-id'],
            correlationId: resp.headers['x-correlation-id'],
            idempotentReplayed:
                resp.headers['idempotent-replayed']?.trim().toLowerCase() ==
                    'true',
            quota: QuotaInfo.fromHeaders(resp.headers),
            idempotencyKey: key,
            attempts: attempt,
          );
        }
        final retryAfter = parseRetryAfter(resp.headers['retry-after']);
        final error = CfMeetApiError.fromResponse(
            status: resp.status,
            headers: resp.headers,
            body: decoded,
            retryAfter: retryAfter);
        if (op.retry == RetryClass.never &&
            unknownOutcomeStatuses.contains(resp.status)) {
          throw UnknownOutcomeError(
              '${op.qualifiedName} answered ${resp.status}: it may have been performed',
              operation: op.qualifiedName,
              idempotencyKey: key,
              cause: error,
              requestId: error.requestId);
        }
        if (!shouldRetryStatus(op.retry, resp.status) || attempt > retries) {
          throw error;
        }
        if (retryAfter != null) {
          if (retryAfter > retryOptions.maxRetryAfter) throw error;
          delay = retryAfter;
        }
        reason = 'HTTP ${resp.status}';
      } on TransportException catch (e) {
        final CfMeetError failure;
        switch (e.kind) {
          case TransportFailure.cancelled:
            throw CfMeetCancelledError('cancelled',
                requestSent: e.requestSent, idempotencyKey: key);
          case TransportFailure.timeout:
            failure = CfMeetTimeoutError(
                '${op.qualifiedName}: no response within $timeout',
                timeout: timeout,
                requestSent: e.requestSent);
            reason = 'timeout';
          case TransportFailure.network:
            failure = CfMeetNetworkError(
                '${op.qualifiedName}: network error (${e.message})',
                requestSent: e.requestSent,
                cause: e.cause);
            reason = 'network';
        }
        if (op.retry == RetryClass.never && e.requestSent) {
          throw UnknownOutcomeError(
              '${op.qualifiedName}: $reason after the request was sent: it may have been performed',
              operation: op.qualifiedName,
              idempotencyKey: key,
              cause: failure);
        }
        if (!shouldRetryNetwork(op.retry) || attempt > retries) throw failure;
      }
      delay ??= retryOptions.backoff(attempt - 1, _random);
      onRetry?.call(RetryEvent(
          operation: op.qualifiedName,
          attempt: attempt,
          delay: delay,
          reason: reason,
          idempotencyKey: key));
      if (!await cancellableDelay(delay, cancel)) {
        throw CfMeetCancelledError('cancelled while waiting to retry',
            requestSent: true, idempotencyKey: key);
      }
    }
  }

  static Object? _decode(TransportResponse resp) {
    if (resp.body.isEmpty) return null;
    final text = utf8.decode(resp.body, allowMalformed: true);
    final type = resp.headers['content-type'] ?? '';
    if (type.contains('json') || text.startsWith('{') || text.startsWith('[')) {
      try {
        return jsonDecode(text);
      } on FormatException {
        return text;
      }
    }
    return text;
  }

  /// Every item of a cursor-paginated list operation, fetching pages lazily. Stops at
  /// the last page (no next cursor), after [maxPages], or when [cancelToken] is cancelled.
  Stream<Map<String, dynamic>> paginate(
    OperationSpec op, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    CancelToken? cancelToken,
    CfMeetAuth? auth,
    int? maxPages,
  }) async* {
    await for (final page in pages(op,
        pathParams: pathParams,
        query: query,
        headers: headers,
        cancelToken: cancelToken,
        auth: auth,
        maxPages: maxPages)) {
      final items = page.json[op.pagination!.itemsField];
      if (items is! List) {
        throw StateError(
            '${op.qualifiedName}: page has no "${op.pagination!.itemsField}" list');
      }
      for (final item in items) {
        if (item is! Map<String, dynamic>) {
          throw StateError(
              '${op.qualifiedName}: page item is not a JSON object');
        }
        yield item;
      }
    }
  }

  /// The raw pages of a cursor-paginated list operation.
  Stream<ApiResponse> pages(
    OperationSpec op, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    CancelToken? cancelToken,
    CfMeetAuth? auth,
    int? maxPages,
  }) async* {
    final p = op.pagination;
    if (p == null) {
      throw CfMeetUsageError(
          '${op.qualifiedName} is not a paginated list operation');
    }
    String? cursor = query[p.cursorParam]?.toString();
    final seen = <String>{};
    for (var n = 0; maxPages == null || n < maxPages; n++) {
      if (cancelToken?.isCancelled ?? false) return;
      final page = await request(op,
          pathParams: pathParams,
          query: {...query, if (cursor != null) p.cursorParam: cursor},
          headers: headers,
          cancelToken: cancelToken,
          auth: auth);
      yield page;
      final next = page.body is Map ? (page.body as Map)[p.nextField] : null;
      if (next == null || next is! String || next.isEmpty) return;
      if (!seen.add(next)) {
        throw StateError(
            '${op.qualifiedName}: the server repeated cursor; stopping');
      }
      cursor = next;
    }
  }

  /// Releases the transport's connections.
  void close({bool force = false}) => _transport.close(force: force);
}

/// The operations of one API domain: `client.sms.call('sendMessage', body: ...)`.
class DomainClient {
  DomainClient._(this._client, this.name);

  final CfMeetClient _client;

  /// The domain (`sms`, `calling`, ...).
  final String name;

  /// Names of this domain's operations, sorted.
  List<String> get operationNames =>
      (operationIndex[name]!.keys.toList()..sort());

  /// The checked operation [operationName].
  BoundOperation operation(String operationName) =>
      BoundOperation._(_client, findOperation(name, operationName));

  /// Sends the operation [operationName] (see [CfMeetClient.request]).
  Future<ApiResponse> call(
    String operationName, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    Object? body,
    String? idempotencyKey,
    CancelToken? cancelToken,
    Duration? timeout,
    CfMeetAuth? auth,
  }) =>
      operation(operationName).call(
          pathParams: pathParams,
          query: query,
          headers: headers,
          body: body,
          idempotencyKey: idempotencyKey,
          cancelToken: cancelToken,
          timeout: timeout,
          auth: auth);

  /// Items of the paginated list operation [operationName].
  Stream<Map<String, dynamic>> paginate(
    String operationName, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    CancelToken? cancelToken,
    CfMeetAuth? auth,
    int? maxPages,
  }) =>
      operation(operationName).paginate(
          pathParams: pathParams,
          query: query,
          headers: headers,
          cancelToken: cancelToken,
          auth: auth,
          maxPages: maxPages);
}

/// One catalogue operation bound to a client.
class BoundOperation {
  BoundOperation._(this._client, this.spec);

  final CfMeetClient _client;
  final OperationSpec spec;

  Future<ApiResponse> call({
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    Object? body,
    String? idempotencyKey,
    CancelToken? cancelToken,
    Duration? timeout,
    CfMeetAuth? auth,
  }) =>
      _client.request(spec,
          pathParams: pathParams,
          query: query,
          headers: headers,
          body: body,
          idempotencyKey: idempotencyKey,
          cancelToken: cancelToken,
          timeout: timeout,
          auth: auth);

  Stream<Map<String, dynamic>> paginate({
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    CancelToken? cancelToken,
    CfMeetAuth? auth,
    int? maxPages,
  }) =>
      _client.paginate(spec,
          pathParams: pathParams,
          query: query,
          headers: headers,
          cancelToken: cancelToken,
          auth: auth,
          maxPages: maxPages);
}
