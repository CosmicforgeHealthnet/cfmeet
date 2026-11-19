/// The CF Meet user token (ADR 0062): `cfut_<sbx|test|live>_<compact JWS>`, minted for
/// one partner user by the partner's server and handed to the device through a token
/// provider.
///
/// The SDK never verifies the token (CF Meet services do, live); it only decodes the JWS
/// payload to learn `exp`, so it asks the provider for a fresh token ~60 s before expiry
/// instead of waiting for a 401. A token whose payload cannot be decoded is used until
/// the first refusal.
library;

import 'dart:async';
import 'dart:convert';

import 'errors.dart';

/// The wire prefix of a user token.
const String userTokenPrefix = 'cfut_';

/// Maximum wire length of a user token (build contract §1).
const int userTokenMaxLength = 2048;

final RegExp _userTokenPattern = RegExp(
    r'^cfut_(sbx|test|live)_([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]*)$');

/// 401 error codes after which a request made with a user token is retried ONCE with a
/// fresh token from the provider.
const Set<String> userTokenRefreshCodes = {
  'USER_TOKEN_EXPIRED',
  'USER_TOKEN_REVOKED',
  'USER_TOKEN_INVALID',
  'AUTHENTICATION_FAILED',
  'AUTHENTICATION_REQUIRED',
};

/// True when [token] has the shape of a CF Meet user token (not a verification).
bool isUserToken(String token) =>
    token.length <= userTokenMaxLength && _userTokenPattern.hasMatch(token);

/// The UNVERIFIED claims of a user token: for display and scheduling only, never for an
/// access decision.
class UserTokenClaims {
  const UserTokenClaims(this.json);

  /// Every claim as decoded.
  final Map<String, dynamic> json;

  /// The partner user's principal id (`sub`).
  String? get subject => json['sub'] is String ? json['sub'] as String : null;

  /// The environment id (`eid`).
  String? get environmentId =>
      json['eid'] is String ? json['eid'] as String : null;

  /// `SANDBOX`, `TEST` or `PRODUCTION` (`ekd`).
  String? get environmentKind =>
      json['ekd'] is String ? json['ekd'] as String : null;

  /// Expiry (`exp`), UTC.
  DateTime? get expiresAt {
    final exp = json['exp'];
    if (exp is! num) return null;
    return DateTime.fromMillisecondsSinceEpoch((exp * 1000).toInt(),
        isUtc: true);
  }

  /// The grants (`grt`: `msg`, `call`, `mtg`).
  Map<String, dynamic> get grants => json['grt'] is Map<String, dynamic>
      ? json['grt'] as Map<String, dynamic>
      : const {};
}

/// Decodes the payload of a user token WITHOUT verifying it. Null when the token is not a
/// well-formed `cfut_` JWS.
UserTokenClaims? decodeUserTokenClaims(String token) {
  if (token.length > userTokenMaxLength) return null;
  final m = _userTokenPattern.firstMatch(token);
  if (m == null) return null;
  try {
    final bytes = base64Url.decode(base64Url.normalize(m[3]!));
    final claims = jsonDecode(utf8.decode(bytes));
    return claims is Map<String, dynamic> ? UserTokenClaims(claims) : null;
  } on FormatException {
    return null;
  }
}

/// Caches the user token returned by a provider: one provider call at a time, a fresh
/// token [refreshBeforeExpiry] before `exp`, and [invalidate] after a refusal.
class UserTokenCache {
  UserTokenCache(
    this._provider, {
    this.refreshBeforeExpiry = const Duration(seconds: 60),
    DateTime Function()? now,
  }) : _now = now ?? DateTime.now {
    if (refreshBeforeExpiry.isNegative) {
      throw const CfMeetUsageError('refreshBeforeExpiry must be >= 0');
    }
  }

  final FutureOr<String> Function() _provider;
  final DateTime Function() _now;

  /// How long before `exp` the provider is asked again.
  final Duration refreshBeforeExpiry;

  String? _token;
  DateTime? _expiresAt;
  Future<String>? _inflight;

  /// Expiry of the cached token, null when unknown or none is cached.
  DateTime? get expiresAt => _token == null ? null : _expiresAt;

  /// The cached token, or a fresh one when none is cached or it expires soon.
  Future<String> get() {
    final t = _token;
    final exp = _expiresAt;
    if (t != null &&
        (exp == null || _now().isBefore(exp.subtract(refreshBeforeExpiry)))) {
      return Future.value(t);
    }
    return refresh();
  }

  /// Asks the provider for a new token (concurrent callers share one call).
  Future<String> refresh() => _inflight ??= _fetch();

  Future<String> _fetch() async {
    try {
      final token = await _provider();
      if (!isUserToken(token)) {
        // Never echo the value.
        throw const CfMeetUsageError(
            'the token provider did not return a CF Meet user token (cfut_...)');
      }
      _token = token;
      _expiresAt = decodeUserTokenClaims(token)?.expiresAt;
      return token;
    } finally {
      _inflight = null;
    }
  }

  /// Forgets the cached token (only [token], when given, so a newer one is kept).
  void invalidate([String? token]) {
    if (token == null || token == _token) {
      _token = null;
      _expiresAt = null;
    }
  }
}
