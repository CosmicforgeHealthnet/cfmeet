/// Typed errors of the SDK.
library;

import 'response.dart';

/// Base of every error the SDK throws.
abstract class CfMeetError implements Exception {
  const CfMeetError(this.message);

  final String message;

  @override
  String toString() => '$runtimeType: $message';
}

/// The SDK was used incorrectly (unknown operation, missing path parameter, unsafe path
/// value, disallowed header, a platform credential in a browser, ...). Nothing was sent.
class CfMeetUsageError extends CfMeetError {
  const CfMeetUsageError(super.message);
}

/// An error answer of the API (`{"error":{"code","message","requestId"}}`).
class CfMeetApiError extends CfMeetError {
  const CfMeetApiError({
    required this.status,
    required this.code,
    required String message,
    this.requestId,
    this.retryAfter,
    this.quota,
    this.body,
    this.headers = const {},
  }) : super(message);

  /// Maps a non-2xx response to the most specific subclass.
  factory CfMeetApiError.fromResponse({
    required int status,
    required Map<String, String> headers,
    required Object? body,
    Duration? retryAfter,
  }) {
    String code = 'HTTP_$status';
    String message = 'HTTP $status';
    String? requestId = headers['x-request-id'];
    if (body is Map && body['error'] is Map) {
      final e = body['error'] as Map;
      if (e['code'] is String) code = e['code'] as String;
      if (e['message'] is String) message = e['message'] as String;
      if (e['requestId'] is String) requestId = e['requestId'] as String;
    }
    final quota = QuotaInfo.fromHeaders(headers);
    CfMeetApiError make(
      CfMeetApiError Function({
        required int status,
        required String code,
        required String message,
        String? requestId,
        Duration? retryAfter,
        QuotaInfo? quota,
        Object? body,
        Map<String, String> headers,
      }) ctor,
    ) =>
        ctor(
          status: status,
          code: code,
          message: message,
          requestId: requestId,
          retryAfter: retryAfter,
          quota: quota,
          body: body,
          headers: headers,
        );
    switch (status) {
      case 401:
        return make(CfMeetAuthenticationError.new);
      case 403:
        return make(CfMeetPermissionError.new);
      case 404:
        return make(CfMeetNotFoundError.new);
      case 400 || 413 || 414 || 422 || 431:
        return make(CfMeetValidationError.new);
      case 409 || 412:
        return make(CfMeetConflictError.new);
      case 429:
        return code == 'QUOTA_EXCEEDED'
            ? make(CfMeetQuotaExceededError.new)
            : make(CfMeetRateLimitedError.new);
      case 502 || 503 || 504:
        return make(CfMeetUpstreamUnavailableError.new);
      default:
        return make(CfMeetApiError.new);
    }
  }

  final int status;

  /// Stable error code (`AUTHENTICATION_FAILED`, `QUOTA_EXCEEDED`, ...).
  final String code;

  /// `x-request-id` (or the envelope's requestId): quote it to support.
  final String? requestId;

  /// The server's `Retry-After`, when it sent one.
  final Duration? retryAfter;
  final QuotaInfo? quota;

  /// The decoded error body.
  final Object? body;
  final Map<String, String> headers;

  @override
  String toString() =>
      '$runtimeType($status $code: $message${requestId == null ? '' : ', requestId: $requestId'})';
}

/// 401: missing, invalid, expired or revoked credential.
class CfMeetAuthenticationError extends CfMeetApiError {
  const CfMeetAuthenticationError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 403: authenticated but not allowed (scope, policy, origin).
class CfMeetPermissionError extends CfMeetApiError {
  const CfMeetPermissionError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 404: no such resource, or a route that is not enabled (default deny).
class CfMeetNotFoundError extends CfMeetApiError {
  const CfMeetNotFoundError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 400 / 413 / 414 / 422 / 431: the request is invalid.
class CfMeetValidationError extends CfMeetApiError {
  const CfMeetValidationError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 409 / 412: conflict (version, state, Idempotency-Key reused with another request).
class CfMeetConflictError extends CfMeetApiError {
  const CfMeetConflictError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 429 RATE_LIMITED (or another 429 code).
class CfMeetRateLimitedError extends CfMeetApiError {
  const CfMeetRateLimitedError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 429 QUOTA_EXCEEDED: the environment's platform request quota is exhausted.
class CfMeetQuotaExceededError extends CfMeetRateLimitedError {
  const CfMeetQuotaExceededError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// 502 / 503 / 504: the service behind the route is unavailable or timed out.
class CfMeetUpstreamUnavailableError extends CfMeetApiError {
  const CfMeetUpstreamUnavailableError({
    required super.status,
    required super.code,
    required super.message,
    super.requestId,
    super.retryAfter,
    super.quota,
    super.body,
    super.headers,
  });
}

/// The request could not be completed at the network level (DNS, connection refused,
/// connection reset, TLS). [requestSent] tells whether the request may have reached the
/// server.
class CfMeetNetworkError extends CfMeetError {
  const CfMeetNetworkError(super.message,
      {this.requestSent = false, this.cause});

  final bool requestSent;
  final Object? cause;
}

/// No response within the configured timeout.
class CfMeetTimeoutError extends CfMeetError {
  const CfMeetTimeoutError(super.message,
      {required this.timeout, this.requestSent = true});

  final Duration timeout;
  final bool requestSent;
}

/// The caller cancelled the request ([CancelToken.cancel]).
class CfMeetCancelledError extends CfMeetError {
  const CfMeetCancelledError(super.message,
      {this.requestSent = false, this.idempotencyKey});

  /// Whether the request may already have reached the server.
  final bool requestSent;
  final String? idempotencyKey;
}

/// A NEVER-retried (paid telecom) operation whose outcome is unknown: it timed out, the
/// connection was lost after the request was sent, or the answer was 500 / 502 / 504.
/// The operation MAY have been performed (and billed). The SDK does not retry; reconcile
/// (look the resource up) or deliberately replay the same [idempotencyKey].
class UnknownOutcomeError extends CfMeetError {
  const UnknownOutcomeError(
    super.message, {
    required this.operation,
    required this.idempotencyKey,
    required this.cause,
    this.requestId,
  });

  /// `domain.name` of the operation.
  final String operation;

  /// The Idempotency-Key sent; replaying it returns the original result if it happened.
  final String? idempotencyKey;

  /// The underlying [CfMeetTimeoutError], [CfMeetNetworkError] or [CfMeetApiError].
  final CfMeetError cause;
  final String? requestId;

  @override
  String toString() =>
      'UnknownOutcomeError($operation, idempotencyKey: $idempotencyKey, cause: $cause)';
}
