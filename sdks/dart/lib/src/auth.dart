/// Authentication of requests: the gateway's authentication modes and where each one's
/// token travels.
library;

import 'dart:async';

import 'errors.dart';
import 'platform.dart';

/// Authentication modes of the public API (the `auth` of an `OperationSpec`).
enum AuthMode {
  /// Customer platform API credential `cfmk_<sbx|test|live>_...` — server-side only.
  platformCredential('PLATFORM_CREDENTIAL'),

  /// A developer console user session (Bearer).
  developerSession('DEVELOPER_SESSION'),

  /// A Meeting Service user session (Bearer).
  meetingSession('MEETING_SESSION'),

  /// A meeting participant token (`X-Participant-Token`).
  participantToken('PARTICIPANT_TOKEN'),

  /// A Messaging client access token (`Authorization: CF-Client-Token <token>`).
  clientToken('CLIENT_TOKEN'),

  /// A Call Signaling Credential `cfsig_...` (Bearer).
  signalingCredential('SIGNALING_CREDENTIAL'),

  /// A single-use realtime ticket (WebSocket only; see `realtime.dart`).
  realtimeTicket('REALTIME_TICKET'),

  /// No credential.
  anonymous('ANONYMOUS');

  const AuthMode(this.wire);

  /// The name used in the contracts and the route manifest.
  final String wire;
}

/// Matches a platform credential: `cfmk_<sbx|test|live>_<public id>_<secret>`.
final RegExp platformCredentialPattern =
    RegExp(r'^cfmk_(sbx|test|live)_[A-Za-z0-9_-]+$');

/// How one request is authenticated. Tokens are never logged or included in errors.
class CfMeetAuth {
  CfMeetAuth._(this.mode, this._token);

  /// Server-side platform credential (`Authorization: Bearer cfmk_...`).
  factory CfMeetAuth.platformCredential(String credential) {
    _checkCredential(credential);
    return CfMeetAuth._(AuthMode.platformCredential, () => credential);
  }

  /// A developer console user session (`Authorization: Bearer <session>`).
  factory CfMeetAuth.developerSession(String token) =>
      CfMeetAuth._(AuthMode.developerSession, _fixed(token));

  /// A Meeting Service user session (`Authorization: Bearer <session>`).
  factory CfMeetAuth.meetingSession(String token) =>
      CfMeetAuth._(AuthMode.meetingSession, _fixed(token));

  /// A meeting participant token (`X-Participant-Token`).
  factory CfMeetAuth.participantToken(String token) =>
      CfMeetAuth._(AuthMode.participantToken, _fixed(token));

  /// A Messaging client access token (`Authorization: CF-Client-Token <token>`).
  factory CfMeetAuth.clientToken(String token) =>
      CfMeetAuth._(AuthMode.clientToken, _fixed(token));

  /// A Call Signaling Credential (`Authorization: Bearer cfsig_...`).
  factory CfMeetAuth.signalingCredential(String credential) =>
      CfMeetAuth._(AuthMode.signalingCredential, _fixed(credential));

  /// No credential.
  factory CfMeetAuth.anonymous() => CfMeetAuth._(AuthMode.anonymous, () => '');

  /// A token obtained asynchronously before every request (refreshing sessions, short
  /// lived client tokens / signaling credentials). [mode] says where it travels.
  factory CfMeetAuth.provider(
      AuthMode mode, FutureOr<String> Function() tokenProvider) {
    if (mode == AuthMode.realtimeTicket) {
      throw const CfMeetUsageError(
          'realtime tickets are minted per connection by the realtime helpers');
    }
    return CfMeetAuth._(mode, tokenProvider);
  }

  final AuthMode mode;
  final FutureOr<String> Function() _token;

  static FutureOr<String> Function() _fixed(String token) {
    if (token.isEmpty) throw const CfMeetUsageError('empty token');
    return () => token;
  }

  static void _checkCredential(String credential) {
    if (!platformCredentialPattern.hasMatch(credential)) {
      // Never echo the value.
      throw const CfMeetUsageError(
          'not a platform credential (expected cfmk_<sbx|test|live>_...)');
    }
  }

  /// The request headers carrying this authentication.
  Future<Map<String, String>> headers() async {
    if (mode == AuthMode.anonymous) return const {};
    final token = await _token();
    if (token.isEmpty) {
      throw const CfMeetUsageError(
          'the token provider returned an empty token');
    }
    if (!_headerSafe(token)) {
      throw const CfMeetUsageError(
          'the token contains characters not allowed in a header');
    }
    switch (mode) {
      case AuthMode.platformCredential:
        _checkCredential(token);
        return {'authorization': 'Bearer $token'};
      case AuthMode.developerSession ||
            AuthMode.meetingSession ||
            AuthMode.signalingCredential:
        if (token.startsWith('cfmk_')) {
          throw const CfMeetUsageError(
              'a platform credential must be configured with CfMeetAuth.platformCredential');
        }
        return {'authorization': 'Bearer $token'};
      case AuthMode.participantToken:
        return {'x-participant-token': token};
      case AuthMode.clientToken:
        return {'authorization': 'CF-Client-Token $token'};
      case AuthMode.realtimeTicket || AuthMode.anonymous:
        return const {};
    }
  }

  /// The raw token (realtime helpers that place it in a WebSocket handshake).
  Future<String> token() async => await _token();

  @override
  String toString() => 'CfMeetAuth(${mode.wire}, token: <redacted>)';
}

bool _headerSafe(String v) =>
    v.length <= 4096 && v.codeUnits.every((c) => c >= 0x21 && c <= 0x7e);

/// The browser policy: a platform credential is a server-side secret and is refused in a
/// browser build unless [allowInBrowser] is set explicitly (e.g. a test harness against a
/// sandbox). Throws [CfMeetUsageError].
void enforceCredentialPolicy(
  CfMeetAuth? auth, {
  bool isWeb = isWebPlatform,
  bool allowInBrowser = false,
}) {
  if (auth == null || auth.mode != AuthMode.platformCredential) return;
  if (isWeb && !allowInBrowser) {
    throw const CfMeetUsageError(
        'platform credentials (cfmk_...) are server-side secrets and are refused in '
        'browser builds; use a client token, participant token or user session, or mint '
        'short-lived tokens on your server');
  }
}
