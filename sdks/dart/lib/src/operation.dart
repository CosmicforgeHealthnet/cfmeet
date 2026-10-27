/// The operation model the generated catalogue (`generated/operations.dart`) is written
/// in. Field names and constructors are fixed by `sdks/generate.py`.
library;

/// How an operation may be retried automatically (see `sdks/generate.py`).
enum RetryClass {
  /// GET: retried on network errors, 408 / 429 / 502 / 503 / 504.
  safe,

  /// PUT / DELETE, or a mutation carrying an Idempotency-Key: retried on the same
  /// conditions with the SAME key.
  idempotent,

  /// Other mutations: retried only on 429 (a refusal: nothing was done).
  noRetry,

  /// Paid telecom mutations: never retried automatically. An unknown outcome surfaces as
  /// an `UnknownOutcomeError` carrying the Idempotency-Key for deliberate reconciliation.
  never,
}

/// A query or header parameter of an operation.
class ParamSpec {
  const ParamSpec(this.name, {required this.required});

  final String name;
  final bool required;

  @override
  String toString() => required ? '$name (required)' : name;
}

/// Cursor pagination of a list operation.
class PaginationSpec {
  const PaginationSpec({
    required this.cursorParam,
    required this.nextField,
    required this.itemsField,
  });

  /// The query parameter carrying the cursor (`cursor`).
  final String cursorParam;

  /// The response field holding the next cursor (`nextCursor`); absent or null at the end.
  final String nextField;

  /// The response field holding the page's items.
  final String itemsField;
}

/// One public operation the API Gateway routes.
class OperationSpec {
  const OperationSpec({
    required this.domain,
    required this.name,
    required this.operationId,
    required this.method,
    required this.path,
    required this.protocol,
    required this.auth,
    required this.scope,
    required this.pathParams,
    required this.query,
    required this.headers,
    required this.body,
    required this.idempotency,
    required this.retry,
    required this.paidTelecom,
    required this.pagination,
    required this.success,
  });

  /// API family: `calling`, `messaging`, `sms`, ...
  final String domain;

  /// Unique (per domain) lowerCamelCase operation name.
  final String name;
  final String operationId;

  /// `GET`, `POST`, `PUT`, `PATCH` or `DELETE`.
  final String method;

  /// Path template with `{param}` placeholders.
  final String path;

  /// `REST` or `WEBSOCKET`.
  final String protocol;

  /// Accepted authentication modes (`PLATFORM_CREDENTIAL`, `CLIENT_TOKEN`, ...).
  final List<String> auth;

  /// Platform scope the credential must hold, if any.
  final String? scope;
  final List<String> pathParams;
  final List<ParamSpec> query;

  /// Documented request headers (lower-case), besides Idempotency-Key.
  final List<ParamSpec> headers;

  /// `NONE`, `OPTIONAL` or `REQUIRED`.
  final String body;

  /// Idempotency-Key header: `NONE`, `OPTIONAL` or `REQUIRED`.
  final String idempotency;
  final RetryClass retry;

  /// A billable telecom mutation (retry class [RetryClass.never]).
  final bool paidTelecom;
  final PaginationSpec? pagination;

  /// Documented 2xx statuses.
  final List<int> success;

  bool get isWebSocket => protocol == 'WEBSOCKET';
  bool get takesIdempotencyKey => idempotency != 'NONE';

  /// `domain.name`.
  String get qualifiedName => '$domain.$name';

  @override
  String toString() => '$qualifiedName ($method $path)';
}
