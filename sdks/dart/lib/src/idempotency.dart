/// Idempotency keys.
library;

import 'dart:math';

final Random _secure = Random.secure();

/// A random (version 4) UUID from a cryptographically secure source.
String newIdempotencyKey() {
  final b = List<int>.generate(16, (_) => _secure.nextInt(256));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  String h(int i) => b[i].toRadixString(16).padLeft(2, '0');
  final s = List<String>.generate(16, h).join();
  return '${s.substring(0, 8)}-${s.substring(8, 12)}-${s.substring(12, 16)}-'
      '${s.substring(16, 20)}-${s.substring(20)}';
}

/// Idempotency-Key values the SDK sends: 1-255 visible ASCII characters.
bool isValidIdempotencyKey(String key) =>
    key.isNotEmpty &&
    key.length <= 255 &&
    key.codeUnits.every((c) => c >= 0x21 && c <= 0x7e);
