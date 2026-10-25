import 'dart:convert';
import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

/// The shared vectors (generated from the reference signer in packages/webhooks).
Map<String, dynamic> loadVectors() {
  final f = File('../testdata/webhook-signature-vectors.json');
  return jsonDecode(f.readAsStringSync()) as Map<String, dynamic>;
}

WebhookVerificationFailure? outcome(Map<String, dynamic> v, int tolerance) {
  try {
    verifyWebhookSignature(
      secret: v['secret'] as String,
      signatureHeader: v['signature'] as String,
      timestampHeader: v['timestamp'] as String,
      deliveryId: v['deliveryId'] as String,
      rawBody: utf8.encode(v['body'] as String),
      now: DateTime.fromMillisecondsSinceEpoch((v['now'] as int) * 1000,
          isUtc: true),
      toleranceSeconds: tolerance,
    );
    return null;
  } on WebhookVerificationError catch (e) {
    return e.failure;
  }
}

void main() {
  final doc = loadVectors();
  final vectors = (doc['vectors'] as List).cast<Map<String, dynamic>>();
  final tolerance = doc['toleranceSeconds'] as int;

  test('the vector file is present and covers every outcome', () {
    expect(vectors.length, greaterThanOrEqualTo(10));
    expect(vectors.map((v) => v['expected']).toSet(),
        {'VALID', 'MISMATCH', 'STALE_TIMESTAMP', 'MALFORMED'});
    expect(tolerance, defaultWebhookToleranceSeconds);
  });

  for (final v in vectors) {
    test('vector ${v['name']} -> ${v['expected']}', () {
      final expected = switch (v['expected'] as String) {
        'VALID' => null,
        'MISMATCH' => WebhookVerificationFailure.mismatch,
        'STALE_TIMESTAMP' => WebhookVerificationFailure.staleTimestamp,
        'MALFORMED' => WebhookVerificationFailure.malformed,
        final other => throw StateError('unknown expectation $other'),
      };
      expect(outcome(v, tolerance), expected);
    });
  }

  group('constructEvent', () {
    final valid = vectors.firstWhere((v) => v['name'] == 'valid');
    Map<String, String> headers(Map<String, dynamic> v) => {
          'CF-Webhook-Id': v['deliveryId'] as String,
          'CF-Webhook-Event-Id': 'whe_0123456789abcdef0123456789abcdef',
          'CF-Webhook-Timestamp': v['timestamp'] as String,
          'CF-Webhook-Signature': v['signature'] as String,
        };
    DateTime nowOf(Map<String, dynamic> v) =>
        DateTime.fromMillisecondsSinceEpoch((v['now'] as int) * 1000,
            isUtc: true);

    test('verifies and decodes the valid vector (headers in any case)', () {
      final e = constructEvent(
          secret: valid['secret'] as String,
          headers: headers(valid),
          rawBody: utf8.encode(valid['body'] as String),
          now: nowOf(valid));
      expect(e.deliveryId, 'whd_vector_1');
      expect(e.eventId, 'whe_0123456789abcdef0123456789abcdef');
      expect(e.type, 'call.completed');
      expect((e.payload as Map)['data'], containsPair('note', 'héllo ✓'));
      expect(e.timestamp, DateTime.utc(2025, 10, 9, 8, 53, 20));
    });

    test('refuses every non-valid vector', () {
      for (final v in vectors.where((v) => v['expected'] != 'VALID')) {
        expect(
            () => constructEvent(
                secret: v['secret'] as String,
                headers: headers(v),
                rawBody: utf8.encode(v['body'] as String),
                now: nowOf(v)),
            throwsA(isA<WebhookVerificationError>()),
            reason: v['name'] as String);
      }
    });

    test('refuses missing headers and a re-serialized body', () {
      final h = headers(valid)..remove('CF-Webhook-Signature');
      expect(
          () => constructEvent(
              secret: valid['secret'] as String,
              headers: h,
              rawBody: utf8.encode(valid['body'] as String),
              now: nowOf(valid)),
          throwsA(isA<WebhookVerificationError>().having((e) => e.failure,
              'failure', WebhookVerificationFailure.malformed)));
      final reserialized = jsonEncode(jsonDecode(valid['body'] as String));
      expect(
          () => constructEvent(
              secret: valid['secret'] as String,
              headers: headers(valid),
              rawBody: utf8.encode('$reserialized '),
              now: nowOf(valid)),
          throwsA(isA<WebhookVerificationError>()));
    });

    test('errors never quote the secret', () {
      final v = vectors.firstWhere((v) => v['name'] == 'wrong_secret');
      try {
        verifyWebhookSignature(
            secret: v['secret'] as String,
            signatureHeader: v['signature'] as String,
            timestampHeader: v['timestamp'] as String,
            deliveryId: v['deliveryId'] as String,
            rawBody: utf8.encode(v['body'] as String),
            now: nowOf(v));
        fail('expected a mismatch');
      } on WebhookVerificationError catch (e) {
        expect(e.toString(), isNot(contains('whsec_')));
      }
    });
  });

  test('constant-time comparison', () {
    expect(constantTimeEquals([1, 2, 3], [1, 2, 3]), isTrue);
    expect(constantTimeEquals([1, 2, 3], [1, 2, 4]), isFalse);
    expect(constantTimeEquals([1, 2, 3], [1, 2]), isFalse);
    expect(constantTimeEquals([], []), isTrue);
  });
}
