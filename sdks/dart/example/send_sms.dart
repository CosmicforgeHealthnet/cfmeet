// Server-side: send an SMS with an Idempotency-Key, and reconcile an unknown outcome.
//
//   CFMEET_CREDENTIAL=cfmk_sbx_... dart run example/send_sms.dart +15550100 "hello"
//
// `sms.sendMessage` is a paid telecom operation: the SDK NEVER retries it automatically.
// Persist the Idempotency-Key with your order BEFORE sending; after an unknown outcome,
// replaying the same key deliberately returns the original result if the first attempt
// was performed (`idempotentReplayed`), and sends once otherwise.
import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

Future<void> main(List<String> args) async {
  if (args.length != 2) {
    stderr.writeln('usage: send_sms.dart <to E.164> <text>');
    exitCode = 64;
    return;
  }
  final client = CfMeetClient(
    baseUrl: Platform.environment['CFMEET_API_URL'] ?? 'https://api.cfmeet.com',
    auth: CfMeetAuth.platformCredential(
        Platform.environment['CFMEET_CREDENTIAL']!),
  );
  final key = newIdempotencyKey(); // store it with the business record first
  final body = {
    'to': args[0],
    'body': args[1],
    if (Platform.environment['CFMEET_SMS_FROM'] != null)
      'numberResourceId': Platform.environment['CFMEET_SMS_FROM'],
  };
  try {
    final sent =
        await client.sms.call('sendMessage', body: body, idempotencyKey: key);
    stdout.writeln('sent: ${sent.json['id']} (request ${sent.requestId}, '
        'replayed: ${sent.idempotentReplayed})');
  } on UnknownOutcomeError catch (e) {
    // Timeout / connection loss / 500 / 502 / 504: it may have been sent (and billed).
    stdout.writeln(
        'outcome unknown (${e.cause.runtimeType}); replaying key ${e.idempotencyKey}');
    final replay = await client.sms
        .call('sendMessage', body: body, idempotencyKey: e.idempotencyKey);
    stdout.writeln(
        'reconciled: ${replay.json['id']} (replayed: ${replay.idempotentReplayed})');
  } on CfMeetQuotaExceededError catch (e) {
    stderr.writeln(
        'quota exhausted (${e.quota?.window}); retry after ${e.retryAfter}');
    exitCode = 75;
  } on CfMeetApiError catch (e) {
    stderr.writeln('refused: ${e.status} ${e.code} (request ${e.requestId})');
    exitCode = 1;
  } finally {
    client.close();
  }
}
