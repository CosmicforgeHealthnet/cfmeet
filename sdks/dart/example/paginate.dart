// Walk every item of a cursor-paginated list (the SDK follows `nextCursor`).
//
//   CFMEET_CREDENTIAL=cfmk_sbx_... dart run example/paginate.dart
import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

Future<void> main() async {
  final client = CfMeetClient(
    baseUrl: Platform.environment['CFMEET_API_URL'] ?? 'https://api.cfmeet.com',
    auth: CfMeetAuth.platformCredential(
        Platform.environment['CFMEET_CREDENTIAL']!),
  );
  final cancel = CancelToken();
  ProcessSignal.sigint
      .watch()
      .first
      .then((_) => cancel.cancel('interrupted'))
      .ignore();
  var n = 0;
  try {
    await for (final message in client.sms.paginate('listMessages',
        query: {'limit': 50, 'direction': 'INBOUND'}, cancelToken: cancel)) {
      n++;
      stdout.writeln('${message['id']} ${message['status']}');
    }
  } on CfMeetCancelledError {
    stdout.writeln('stopped');
  } finally {
    client.close();
  }
  stdout.writeln('$n message(s)');
}
