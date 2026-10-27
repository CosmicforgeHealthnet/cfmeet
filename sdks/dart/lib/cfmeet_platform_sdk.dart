/// CF Meet platform SDK for Dart (Part VI Section 52).
///
/// * [CfMeetClient]: every public operation the API Gateway routes (the generated
///   catalogue [operations]), with authentication, typed errors, retries by retry class,
///   idempotency keys, timeouts, cancellation and cursor pagination;
/// * [RealtimeClient]: the Messaging, Meeting and Calling WebSocket channels;
/// * [verifyWebhookSignature] / [constructEvent]: webhook signature verification;
/// * [MessagingCrypto] (native platforms): the Messaging E2EE core over `dart:ffi`.
///
/// Everything under `lib/src/generated/` is produced by `sdks/generate.py` from the
/// published contracts and must not be edited.
library;

export 'src/auth.dart';
export 'src/cancel.dart';
export 'src/client.dart';
export 'src/crypto/messaging_crypto.dart'
    if (dart.library.js_interop) 'src/none.dart';
export 'src/errors.dart';
export 'src/generated/contracts.dart';
export 'src/generated/operations.dart';
export 'src/idempotency.dart';
export 'src/io_exports.dart' if (dart.library.js_interop) 'src/none_io.dart';
export 'src/operation.dart';
export 'src/platform.dart';
export 'src/realtime.dart';
export 'src/response.dart';
export 'src/retry.dart';
export 'src/transport.dart';
export 'src/webhooks.dart';
