/// Real-stack integration: runs against the gateway of a stack started by
/// `scripts/certification/stack.py`, which writes a JSON file named by `CFMEET_STACK_ENV`
/// (at least `{"gatewayUrl": ..., "credentials": {"sandbox": "cfmk_sbx_..."}}`; optional
/// `"quotaEnabled": false` when the stack runs without platform quotas). Without it, the
/// suite prints SKIPPED and passes.
library;

import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

void main() {
  final envFile = Platform.environment['CFMEET_STACK_ENV'];
  if (envFile == null || envFile.isEmpty) {
    test('real-stack integration', () {
      // ignore: avoid_print
      print('SKIPPED: real-stack integration (CFMEET_STACK_ENV is not set)');
    });
    return;
  }
  final stack =
      jsonDecode(File(envFile).readAsStringSync()) as Map<String, dynamic>;
  final gateway = stack['gatewayUrl'] as String;
  final sandbox =
      (stack['credentials'] as Map<String, dynamic>)['sandbox'] as String;
  final quotaEnabled = stack['quotaEnabled'] != false;

  CfMeetClient client(String credential) => CfMeetClient(
        baseUrl: gateway,
        auth: CfMeetAuth.platformCredential(credential),
        allowInsecureHttp: true,
        retryOptions: const RetryOptions(
            maxRetries: 1, baseDelay: Duration(milliseconds: 50)),
        timeout: const Duration(seconds: 20),
      );

  test('GET /discovery', () async {
    final r = await client(sandbox).discovery();
    expect(r.status, 200);
    expect(r.json['service'], 'cfmeet.api-gateway');
    expect(r.json['families'], isA<List<dynamic>>());
    expect(r.requestId, isNotNull);
  });

  test('GET /v1/developer-platform/context with the sandbox credential',
      () async {
    final r = await client(sandbox).gateway.call('getDeveloperPlatformContext');
    expect(r.status, 200);
    expect(r.body, isA<Map<String, dynamic>>());
    expect(r.requestId, isNotNull);
    if (quotaEnabled) {
      expect(r.quota, isNotNull,
          reason: 'x-cf-quota-* headers on a credential request');
      expect(r.quota!.limitMinute, isNotNull);
      expect(r.quota!.remainingMinute, isNotNull);
      expect(r.quota!.resetSeconds, isNotNull);
      expect(r.quota!.limitDay, isNotNull);
    }
  });

  test('an unrouted path is denied by default (404, typed)', () async {
    final suffix = Random.secure().nextInt(1 << 30);
    final unrouted = OperationSpec(
      domain: 'gateway',
      name: 'unrouted',
      operationId: 'unrouted',
      method: 'GET',
      path: '/v1/sdk-certification-unrouted-$suffix',
      protocol: 'REST',
      auth: const ['PLATFORM_CREDENTIAL'],
      scope: null,
      pathParams: const [],
      query: const [],
      headers: const [],
      body: 'NONE',
      idempotency: 'NONE',
      retry: RetryClass.safe,
      paidTelecom: false,
      pagination: null,
      success: const [200],
    );
    await expectLater(
        client(sandbox).request(unrouted),
        throwsA(isA<CfMeetNotFoundError>()
            .having((e) => e.code, 'code', 'NOT_FOUND')
            .having((e) => e.requestId, 'requestId', isNotNull)));
  });

  test('an invalid credential is a typed 401', () async {
    final forged = 'cfmk_sbx_${'0' * 24}_${'0' * 64}';
    await expectLater(
        client(forged).gateway.call('getDeveloperPlatformContext'),
        throwsA(isA<CfMeetAuthenticationError>()
            .having((e) => e.status, 'status', 401)
            .having((e) => e.code, 'code', 'AUTHENTICATION_FAILED')
            .having((e) => e.toString(), 'toString', isNot(contains(forged)))));
  });
}
