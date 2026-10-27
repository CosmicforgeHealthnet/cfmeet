// A webhook receiver: verifies CF-Webhook-Signature over the RAW body, refuses stale
// timestamps, and deduplicates on the event id (retries and replays reuse it).
//
//   CFMEET_WEBHOOK_SECRET=whsec_... dart run example/webhook_receiver.dart
import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

Future<void> main() async {
  final secret = Platform.environment['CFMEET_WEBHOOK_SECRET']!;
  final port = int.parse(Platform.environment['PORT'] ?? '8080');
  final seen = <String>{}; // use durable storage in production
  final server = await HttpServer.bind(InternetAddress.anyIPv4, port);
  stdout.writeln('listening on :$port');
  await for (final request in server) {
    if (request.method != 'POST') {
      request.response.statusCode = HttpStatus.methodNotAllowed;
      await request.response.close();
      continue;
    }
    final raw = await request
        .fold<List<int>>(<int>[], (acc, chunk) => acc..addAll(chunk));
    final headers = <String, String>{};
    request.headers.forEach((name, values) => headers[name] = values.join(','));
    try {
      final event =
          constructEvent(secret: secret, headers: headers, rawBody: raw);
      if (seen.add(event.eventId)) {
        stdout.writeln('event ${event.type} ${event.eventId}');
      } else {
        stdout.writeln('duplicate ${event.eventId} ignored');
      }
      request.response.statusCode = HttpStatus.noContent;
    } on WebhookVerificationError catch (e) {
      stdout.writeln('refused: ${e.failure.name}');
      request.response.statusCode = HttpStatus.badRequest;
    }
    await request.response.close();
  }
}
