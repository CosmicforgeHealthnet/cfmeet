import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

/// The real native library: `CFMEET_MESSAGING_CRYPTO_LIB` when set (CI builds it in a
/// previous step), otherwise built here with cargo into `$CARGO_TARGET_DIR` (or the
/// crate's own target directory). Never skipped: a missing toolchain fails the tests.
String nativeLibraryPath() {
  final explicit = Platform.environment['CFMEET_MESSAGING_CRYPTO_LIB'];
  if (explicit != null && explicit.isNotEmpty) {
    if (!File(explicit).existsSync()) {
      throw StateError('CFMEET_MESSAGING_CRYPTO_LIB=$explicit does not exist');
    }
    return explicit;
  }
  final repo =
      Directory.current.parent.parent.path; // sdks/dart -> repository root
  final crate = '$repo/packages/messaging-crypto/ffi';
  final build = Process.runSync(
      'cargo', ['build', '--manifest-path', '$crate/Cargo.toml'],
      runInShell: Platform.isWindows);
  if (build.exitCode != 0) {
    throw StateError(
        'cargo build of the messaging crypto FFI crate failed:\n${build.stderr}');
  }
  final targetDir = Platform.environment['CARGO_TARGET_DIR'] ?? '$crate/target';
  final path = '$targetDir/debug/${defaultMessagingCryptoLibraryName()}';
  if (!File(path).existsSync()) {
    throw StateError('built library not found at $path');
  }
  return path;
}
