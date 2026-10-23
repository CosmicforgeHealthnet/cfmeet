/// Webhook signature verification (one protocol for every CF Meet service; reference:
/// packages/webhooks/src/signing.rs, ADR 0028).
///
/// ```text
/// CF-Webhook-Id:        <delivery id>
/// CF-Webhook-Event-Id:  <event id>       (stable across retries: dedupe on it)
/// CF-Webhook-Timestamp: <unix seconds>
/// CF-Webhook-Signature: v1=<hex HMAC-SHA256(secret, "v1:" + ts + ":" + delivery id + ":" + raw body)>
/// ```
library;

import 'dart:convert';

import 'package:crypto/crypto.dart';

import 'errors.dart';

const String webhookIdHeader = 'cf-webhook-id';
const String webhookEventIdHeader = 'cf-webhook-event-id';
const String webhookTimestampHeader = 'cf-webhook-timestamp';
const String webhookSignatureHeader = 'cf-webhook-signature';

/// Default timestamp tolerance (seconds) on either side of the receiver's clock.
const int defaultWebhookToleranceSeconds = 300;

/// Why a delivery was refused.
enum WebhookVerificationFailure {
  /// A missing header, a timestamp that is not an integer, an unknown signature version
  /// or a signature that is not hex.
  malformed,

  /// The timestamp is outside the tolerance window (replay or clock skew).
  staleTimestamp,

  /// The signature does not match (wrong secret, or tampered body / id / timestamp).
  mismatch,
}

/// A webhook delivery that failed verification. Never quotes the secret or signature.
class WebhookVerificationError extends CfMeetError {
  const WebhookVerificationError(this.failure, String message) : super(message);

  final WebhookVerificationFailure failure;
}

/// Verifies one delivery's signature over the RAW body bytes (do not re-serialize JSON).
/// [secret] is the full signing secret string (`whsec_...`), used as UTF-8 bytes.
/// Throws [WebhookVerificationError]; returns normally when valid.
void verifyWebhookSignature({
  required String secret,
  required String signatureHeader,
  required String timestampHeader,
  required String deliveryId,
  required List<int> rawBody,
  DateTime? now,
  int toleranceSeconds = defaultWebhookToleranceSeconds,
}) {
  final ts = timestampHeader.trim();
  if (!RegExp(r'^[+-]?\d{1,18}$').hasMatch(ts)) {
    throw const WebhookVerificationError(
        WebhookVerificationFailure.malformed, 'malformed CF-Webhook-Timestamp');
  }
  final timestamp = int.parse(ts);
  final nowSeconds = (now ?? DateTime.now()).millisecondsSinceEpoch ~/ 1000;
  if ((nowSeconds - timestamp).abs() > toleranceSeconds) {
    throw const WebhookVerificationError(
        WebhookVerificationFailure.staleTimestamp,
        'timestamp outside the tolerance window');
  }
  final sig = signatureHeader.trim();
  if (!sig.startsWith('v1=')) {
    throw const WebhookVerificationError(
        WebhookVerificationFailure.malformed, 'unknown signature version');
  }
  final given = _hex(sig.substring(3));
  if (given == null) {
    throw const WebhookVerificationError(
        WebhookVerificationFailure.malformed, 'signature is not hex');
  }
  final input = <int>[...utf8.encode('v1:$timestamp:$deliveryId:'), ...rawBody];
  final expected = Hmac(sha256, utf8.encode(secret)).convert(input).bytes;
  if (!constantTimeEquals(expected, given)) {
    throw const WebhookVerificationError(
        WebhookVerificationFailure.mismatch, 'signature mismatch');
  }
}

/// Compares two byte strings in time independent of their contents.
bool constantTimeEquals(List<int> a, List<int> b) {
  var diff = a.length ^ b.length;
  for (var i = 0; i < a.length; i++) {
    diff |= a[i] ^ (i < b.length ? b[i] : 0);
  }
  return diff == 0;
}

List<int>? _hex(String s) {
  if (s.isEmpty || s.length.isOdd) return null;
  final out = <int>[];
  for (var i = 0; i < s.length; i += 2) {
    final v = int.tryParse(s.substring(i, i + 2), radix: 16);
    if (v == null || s[i] == '+' || s[i] == '-') return null;
    out.add(v);
  }
  return out;
}

/// A verified webhook delivery.
class WebhookEvent {
  const WebhookEvent({
    required this.deliveryId,
    required this.eventId,
    required this.timestamp,
    required this.type,
    required this.payload,
  });

  /// `CF-Webhook-Id`: one delivery series.
  final String deliveryId;

  /// `CF-Webhook-Event-Id` (or the payload's `id`): stable across retries and replays.
  /// Deduplicate on it.
  final String eventId;
  final DateTime timestamp;

  /// The payload's `type` (e.g. `call.completed`), when present.
  final String? type;

  /// The decoded JSON payload.
  final Object? payload;
}

/// Verifies a delivery and decodes it. [headers] may use any letter case.
WebhookEvent constructEvent({
  required String secret,
  required Map<String, String> headers,
  required List<int> rawBody,
  DateTime? now,
  int toleranceSeconds = defaultWebhookToleranceSeconds,
}) {
  final h = {for (final e in headers.entries) e.key.toLowerCase(): e.value};
  String need(String name) {
    final v = h[name];
    if (v == null || v.isEmpty) {
      throw WebhookVerificationError(
          WebhookVerificationFailure.malformed, 'missing $name');
    }
    return v;
  }

  final deliveryId = need(webhookIdHeader);
  final timestamp = need(webhookTimestampHeader);
  verifyWebhookSignature(
    secret: secret,
    signatureHeader: need(webhookSignatureHeader),
    timestampHeader: timestamp,
    deliveryId: deliveryId,
    rawBody: rawBody,
    now: now,
    toleranceSeconds: toleranceSeconds,
  );
  Object? payload;
  if (rawBody.isNotEmpty) {
    try {
      payload = jsonDecode(utf8.decode(rawBody));
    } on FormatException {
      throw const WebhookVerificationError(
          WebhookVerificationFailure.malformed, 'the body is not JSON');
    }
  }
  final Map<Object?, Object?> map =
      payload is Map ? payload : const <Object?, Object?>{};
  final eventId = h[webhookEventIdHeader] ??
      (map['id'] is String ? map['id'] as String : null);
  if (eventId == null || eventId.isEmpty) {
    throw const WebhookVerificationError(
        WebhookVerificationFailure.malformed, 'missing cf-webhook-event-id');
  }
  return WebhookEvent(
    deliveryId: deliveryId,
    eventId: eventId,
    timestamp: DateTime.fromMillisecondsSinceEpoch(
        int.parse(timestamp.trim()) * 1000,
        isUtc: true),
    type: map['type'] is String ? map['type'] as String : null,
    payload: payload,
  );
}
