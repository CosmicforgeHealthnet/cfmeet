import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

void main() {
  test('the SDK is pinned to the published gateway contract', () {
    expect(sdkStatus, 'scaffold');
    expect(contracts.map((c) => c.id), contains('cfmeet.api-gateway.http-api'));
  });
}
