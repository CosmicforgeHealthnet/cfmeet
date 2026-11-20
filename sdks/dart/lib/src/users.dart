/// Partner users and CF Meet user tokens (ADR 0062; build contract §3): SERVER-SIDE
/// helpers over `/v1/users*`, authenticated with the platform credential. Your server
/// creates a user per person in your app (keyed by your own opaque `externalId`, never a
/// phone number) and mints short-lived user tokens for their devices.
library;

import 'cancel.dart';
import 'client.dart';
import 'errors.dart';
import 'platform_routes.dart';
import 'response.dart';

/// A partner user (a Platform Access principal of your environment).
class PartnerUser {
  const PartnerUser(this.json);

  factory PartnerUser.fromJson(Object? json) {
    if (json is! Map<String, dynamic> || json['id'] is! String) {
      throw StateError('the partner user is malformed');
    }
    return PartnerUser(json);
  }

  /// The decoded object (fields this SDK version does not know are kept).
  final Map<String, dynamic> json;

  String get id => json['id'] as String;
  String get externalId => json['externalId'] as String? ?? '';
  String? get displayName => json['displayName'] as String?;
  String? get imageUrl => json['imageUrl'] as String?;

  /// `ACTIVE`, `SUSPENDED` or `DEACTIVATED`.
  String get status => json['status'] as String? ?? '';

  @override
  String toString() => 'PartnerUser($id, $status)';
}

/// A meeting grant of a user token.
class MeetingGrant {
  const MeetingGrant(this.conferenceId, this.role);

  final String conferenceId;

  /// `HOST`, `CO_HOST`, `PRESENTER`, `PARTICIPANT` or `VIEWER`.
  final String role;

  Map<String, Object?> toJson() => {'conferenceId': conferenceId, 'role': role};
}

/// What a user token lets the device do (each part optional; a subset of your key's
/// scopes).
class UserTokenGrants {
  const UserTokenGrants({
    this.msg = const [],
    this.call = const [],
    this.meetings = const [],
  });

  /// Chat: `read` and / or `send` (needs messages.read / messages.send).
  final List<String> msg;

  /// Calls: `receive` (needs calls.write).
  final List<String> call;

  /// At most 10 conferences (needs meetings.write).
  final List<MeetingGrant> meetings;

  Map<String, Object?> toJson() => {
        if (msg.isNotEmpty) 'msg': msg,
        if (call.isNotEmpty) 'call': call,
        if (meetings.isNotEmpty) 'mtg': [for (final m in meetings) m.toJson()],
      };
}

/// A minted user token: hand [token] to the device (returned once, never stored).
class IssuedUserToken {
  const IssuedUserToken(this.json);

  final Map<String, dynamic> json;

  /// `cfut_<sbx|test|live>_...`.
  String get token => json['token'] as String;
  String get tokenId => json['tokenId'] as String? ?? '';
  DateTime? get expiresAt => json['expiresAt'] is String
      ? DateTime.tryParse(json['expiresAt'] as String)
      : null;

  /// The user (upsert only).
  PartnerUser? get user => json['user'] is Map<String, dynamic>
      ? PartnerUser(json['user'] as Map<String, dynamic>)
      : null;

  @override
  String toString() => 'IssuedUserToken($tokenId, token: <redacted>)';
}

/// `client.users`: partner users and user tokens (server-side, platform credential).
class UsersClient {
  UsersClient(this._client);

  final CfMeetClient _client;

  /// `POST /v1/users`: creates the user, or returns the existing one (same externalId).
  Future<PartnerUser> create({
    required String externalId,
    String? displayName,
    String? imageUrl,
    String? idempotencyKey,
    CancelToken? cancelToken,
  }) async =>
      PartnerUser.fromJson((await _create(
              externalId, displayName, imageUrl, idempotencyKey, cancelToken))
          .body);

  /// Like [create], telling whether the user was created (201) or existed (200).
  Future<({PartnerUser user, bool created})> upsert({
    required String externalId,
    String? displayName,
    String? imageUrl,
    String? idempotencyKey,
    CancelToken? cancelToken,
  }) async {
    final r = await _create(
        externalId, displayName, imageUrl, idempotencyKey, cancelToken);
    return (user: PartnerUser.fromJson(r.body), created: r.status == 201);
  }

  Future<ApiResponse> _create(String externalId, String? displayName,
          String? imageUrl, String? idempotencyKey, CancelToken? cancelToken) =>
      _client.request(UserRoutes.createUser,
          body: {
            'externalId': externalId,
            if (displayName != null) 'displayName': displayName,
            if (imageUrl != null) 'imageUrl': imageUrl,
          },
          idempotencyKey: idempotencyKey,
          cancelToken: cancelToken);

  /// `GET /v1/users/{userId}`.
  Future<PartnerUser> get(String userId, {CancelToken? cancelToken}) async =>
      PartnerUser.fromJson((await _client.request(UserRoutes.getUser,
              pathParams: {'userId': userId}, cancelToken: cancelToken))
          .body);

  /// `GET /v1/users` (one page): `{items, nextCursor}`.
  Future<ApiResponse> list(
          {String? externalId,
          int? limit,
          String? cursor,
          CancelToken? cancelToken}) =>
      _client.request(UserRoutes.listUsers,
          query: {
            if (externalId != null) 'externalId': externalId,
            if (limit != null) 'limit': limit,
            if (cursor != null) 'cursor': cursor,
          },
          cancelToken: cancelToken);

  /// Every user of every page.
  Stream<PartnerUser> all({int? limit, CancelToken? cancelToken}) => _client
      .paginate(UserRoutes.listUsers,
          query: {if (limit != null) 'limit': limit}, cancelToken: cancelToken)
      .map(PartnerUser.new);

  /// The user with this externalId, or null.
  Future<PartnerUser?> findByExternalId(String externalId,
      {CancelToken? cancelToken}) async {
    final page =
        await list(externalId: externalId, limit: 1, cancelToken: cancelToken);
    final items = page.json['items'];
    return items is List && items.isNotEmpty
        ? PartnerUser.fromJson(items.first)
        : null;
  }

  /// `PATCH /v1/users/{userId}`: profile, or `status: SUSPENDED` (ends every device
  /// credential). `clear*` sets the field to null.
  Future<PartnerUser> update(
    String userId, {
    String? displayName,
    String? imageUrl,
    String? status,
    bool clearDisplayName = false,
    bool clearImageUrl = false,
    CancelToken? cancelToken,
  }) async {
    final body = <String, Object?>{
      if (displayName != null || clearDisplayName) 'displayName': displayName,
      if (imageUrl != null || clearImageUrl) 'imageUrl': imageUrl,
      if (status != null) 'status': status,
    };
    final r = await _client.request(UserRoutes.updateUser,
        pathParams: {'userId': userId}, body: body, cancelToken: cancelToken);
    return PartnerUser.fromJson(r.body);
  }

  /// `DELETE /v1/users/{userId}`: tombstones the user.
  Future<void> delete(String userId, {CancelToken? cancelToken}) async {
    await _client.request(UserRoutes.deleteUser,
        pathParams: {'userId': userId}, cancelToken: cancelToken);
  }

  /// `POST /v1/users/{userId}/revoke-sessions`: tokens minted before now stop working.
  /// Returns `tokensNotBefore`.
  Future<String?> revokeSessions(String userId,
      {CancelToken? cancelToken}) async {
    final r = await _client.request(UserRoutes.revokeUserSessions,
        pathParams: {'userId': userId}, cancelToken: cancelToken);
    return r.json['tokensNotBefore'] as String?;
  }

  /// `POST /v1/users/{userId}/tokens`: mints a user token for the device.
  Future<IssuedUserToken> createToken(
    String userId, {
    required UserTokenGrants grants,
    int? ttlSeconds,
    String? idempotencyKey,
    CancelToken? cancelToken,
  }) async {
    final r = await _client.request(UserRoutes.issueUserToken,
        pathParams: {'userId': userId},
        body: {
          'grants': grants.toJson(),
          if (ttlSeconds != null) 'ttlSeconds': ttlSeconds,
        },
        idempotencyKey: idempotencyKey,
        cancelToken: cancelToken);
    return _token(r);
  }

  /// `POST /v1/users/tokens`: creates or updates the user (by externalId) and mints a
  /// token in one call.
  Future<IssuedUserToken> upsertWithToken({
    required String externalId,
    String? displayName,
    String? imageUrl,
    required UserTokenGrants grants,
    int? ttlSeconds,
    String? idempotencyKey,
    CancelToken? cancelToken,
  }) async {
    final r = await _client.request(UserRoutes.upsertUserToken,
        body: {
          'externalId': externalId,
          if (displayName != null) 'displayName': displayName,
          if (imageUrl != null) 'imageUrl': imageUrl,
          'grants': grants.toJson(),
          if (ttlSeconds != null) 'ttlSeconds': ttlSeconds,
        },
        idempotencyKey: idempotencyKey,
        cancelToken: cancelToken);
    return _token(r);
  }

  static IssuedUserToken _token(ApiResponse r) {
    final j = r.json;
    if (j['tokenRedacted'] == true) {
      throw const CfMeetUsageError(
          'the user token response was an idempotent replay without the token '
          '(tokenRedacted): mint a new token with a new Idempotency-Key');
    }
    final t = j['token'];
    if (t is! String || !t.startsWith('cfut_')) {
      throw StateError('the user token response is malformed');
    }
    return IssuedUserToken(j);
  }
}
