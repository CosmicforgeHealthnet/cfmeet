// Browser build probe (CI compiles it with `dart compile js` and runs it with node): the
// SDK compiles for browsers, and a browser build refuses a platform credential by
// default, before anything is sent.
import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

void main() {
  if (!isWebPlatform) throw StateError('run the dart2js build of this probe');
  try {
    CfMeetClient(
      baseUrl: 'https://api.cfmeet.com',
      auth: CfMeetAuth.platformCredential('cfmk_sbx_probe_probe'),
    );
  } on CfMeetUsageError catch (e) {
    print('refused as expected: ${e.message}');
    print('operations: ${operations.length}');
    return;
  }
  throw StateError('a browser build accepted a platform credential');
}
