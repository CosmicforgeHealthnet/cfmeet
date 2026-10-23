import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

void main() {
  test('the SDK is pinned to the published gateway contract', () {
    expect(sdkStatus, 'beta');
    expect(contracts.map((c) => c.id), contains('cfmeet.api-gateway.http-api'));
    expect(
        contracts.map((c) => c.id), contains('cfmeet.api-gateway.public-api'));
  });

  test('the operation catalogue is complete and consistent', () {
    expect(operations.length, greaterThan(400));
    final names = <String>{};
    for (final op in operations) {
      expect(names.add(op.qualifiedName), isTrue,
          reason: 'duplicate ${op.qualifiedName}');
      expect(op.path, startsWith('/'));
      expect(['GET', 'POST', 'PUT', 'PATCH', 'DELETE'], contains(op.method));
      for (final p in op.pathParams) {
        expect(op.path, contains('{$p}'), reason: op.qualifiedName);
      }
      if (op.paidTelecom) expect(op.retry, RetryClass.never);
    }
    expect(RealtimeClient.channels.every((o) => o.isWebSocket), isTrue);
    expect(operations.where((o) => o.isWebSocket), hasLength(3));
  });
}
