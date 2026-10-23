import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

final String credential = 'cfmk_live_${'0' * 24}_${'1' * 64}';

void main() {
  group('browser credential policy', () {
    test('a platform credential is refused in a browser build by default', () {
      expect(
          () => enforceCredentialPolicy(
              CfMeetAuth.platformCredential(credential),
              isWeb: true),
          throwsA(isA<CfMeetUsageError>()
              .having((e) => e.message, 'message', contains('server-side'))));
    });

    test('an explicit opt-in allows it (test harnesses)', () {
      enforceCredentialPolicy(CfMeetAuth.platformCredential(credential),
          isWeb: true, allowInBrowser: true);
    });

    test('browser-safe modes are always allowed', () {
      for (final a in [
        CfMeetAuth.clientToken('c'),
        CfMeetAuth.participantToken('p'),
        CfMeetAuth.meetingSession('s'),
        CfMeetAuth.developerSession('d'),
        CfMeetAuth.signalingCredential('cfsig_x'),
        CfMeetAuth.anonymous(),
      ]) {
        enforceCredentialPolicy(a, isWeb: true);
      }
      enforceCredentialPolicy(null, isWeb: true);
    });

    test('native builds accept a platform credential', () {
      enforceCredentialPolicy(CfMeetAuth.platformCredential(credential),
          isWeb: false);
      expect(isWebPlatform, isFalse); // these tests run on the VM
      CfMeetClient(
          baseUrl: 'https://api.example.test',
          auth: CfMeetAuth.platformCredential(credential));
    });

    test('the error never echoes the credential', () {
      try {
        enforceCredentialPolicy(CfMeetAuth.platformCredential(credential),
            isWeb: true);
        fail('expected a refusal');
      } on CfMeetUsageError catch (e) {
        expect(e.toString(), isNot(contains('1111')));
      }
    });
  });
}
