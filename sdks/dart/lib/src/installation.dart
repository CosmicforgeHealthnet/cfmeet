/// The installation id (build contract §4): a stable id per app installation, generated
/// by the SDK and persisted by the app. Storage is pluggable ([KeyValueStorage]; e.g. a
/// `shared_preferences` adapter in Flutter); the default is in memory (a new id per
/// process: fine on servers and in tests, NOT on devices).
library;

import 'dart:async';
import 'dart:math';

/// `[A-Za-z0-9._-]{8,128}`.
final RegExp installationIdPattern = RegExp(r'^[A-Za-z0-9._-]{8,128}$');

/// The storage key the SDK uses by default.
const String defaultInstallationIdKey = 'cfmeet.installationId';

/// Key-value storage for the installation id.
///
/// ```dart
/// class PrefsStorage implements KeyValueStorage {
///   PrefsStorage(this.prefs);
///   final SharedPreferences prefs;
///   @override
///   String? getItem(String key) => prefs.getString(key);
///   @override
///   Future<void> setItem(String key, String value) => prefs.setString(key, value);
/// }
/// ```
abstract interface class KeyValueStorage {
  FutureOr<String?> getItem(String key);
  FutureOr<void> setItem(String key, String value);
}

/// In-memory storage (the default).
class MemoryKeyValueStorage implements KeyValueStorage {
  MemoryKeyValueStorage([Map<String, String>? initial])
      : values = {...?initial};

  final Map<String, String> values;

  @override
  String? getItem(String key) => values[key];

  @override
  void setItem(String key, String value) => values[key] = value;
}

bool isValidInstallationId(String id) => installationIdPattern.hasMatch(id);

final Random _secure = Random.secure();

/// A new random installation id (`inst-` + 32 hex characters).
String generateInstallationId() {
  final hex = List<String>.generate(
      16, (_) => _secure.nextInt(256).toRadixString(16).padLeft(2, '0'));
  return 'inst-${hex.join()}';
}

/// Anything that yields the installation id.
abstract interface class InstallationIdSource {
  Future<String> get();
}

class _FixedInstallationId implements InstallationIdSource {
  const _FixedInstallationId(this.id);
  final String id;

  @override
  Future<String> get() => Future.value(id);
}

/// A fixed installation id.
InstallationIdSource fixedInstallationId(String id) => _FixedInstallationId(id);

/// Reads the persisted installation id, creating and saving one on first use.
class InstallationIdStore implements InstallationIdSource {
  InstallationIdStore(
      {KeyValueStorage? storage, this.key = defaultInstallationIdKey})
      : _storage = storage ?? MemoryKeyValueStorage();

  final KeyValueStorage _storage;
  final String key;
  Future<String>? _pending;

  /// The installation id (concurrent callers share one read / write).
  @override
  Future<String> get() => _pending ??= _load();

  /// Replaces the id with a new one (e.g. when the app wants a new device identity).
  Future<String> reset() async {
    final id = generateInstallationId();
    await _storage.setItem(key, id);
    _pending = Future.value(id);
    return id;
  }

  Future<String> _load() async {
    try {
      final stored = await _storage.getItem(key);
      if (stored != null && isValidInstallationId(stored)) return stored;
      final id = generateInstallationId();
      await _storage.setItem(key, id);
      return id;
    } catch (_) {
      _pending = null;
      rethrow;
    }
  }
}
