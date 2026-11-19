/// The device side of the CF Meet Platform (ADR 0062 / 0063): one client per signed-in
/// partner user, authenticated by a CF Meet user token from YOUR server.
///
/// ```dart
/// final device = CfMeetDeviceClient(
///   baseUrl: 'https://api.cfmeet.com',
///   tokenProvider: () => myApi.fetchCfMeetToken(), // your server mints it
///   storage: PrefsStorage(prefs),                    // persists the installation id
/// );
/// final chat = await device.chat.connect();            // messages + realtime events
/// final calls = await device.calls.connect();          // incoming (server-placed) calls
/// final meeting = await device.meetings.join(conferenceId); // LiveKit url + token
/// await device.push.register(platform: PushPlatform.fcm, handle: fcmToken);
/// ```
library;

import 'dart:async';

import 'auth.dart';
import 'cancel.dart';
import 'client.dart';
import 'errors.dart';
import 'idempotency.dart';
import 'installation.dart';
import 'platform.dart';
import 'platform_routes.dart';
import 'realtime.dart';
import 'response.dart';
import 'retry.dart';
import 'transport.dart';
import 'user_token.dart';

/// A credential derived from the user token, renewed before it expires: one exchange at
/// a time, on use and (with `autoRenew`) in the background, and after a refusal.
class _Renewing<T extends Object> {
  _Renewing(
    this._exchange,
    this._expiresAt, {
    required this.renewBefore,
    required this.autoRenew,
    required this.now,
    this.onRenewed,
    this.onError,
  });

  final Future<T> Function(T? previous) _exchange;
  final DateTime Function(T credential) _expiresAt;
  final Duration renewBefore;
  final bool autoRenew;
  final DateTime Function() now;
  final void Function(T credential)? onRenewed;
  final void Function(Object error)? onError;

  T? current;
  DateTime _obtainedAt = DateTime.fromMillisecondsSinceEpoch(0);
  bool _stale = false;
  bool _closed = false;
  Future<T>? _inflight;
  Timer? _timer;

  DateTime _renewAt(T c) {
    final exp = _expiresAt(c);
    final half = exp.difference(_obtainedAt) ~/ 2;
    var before = half < renewBefore ? half : renewBefore;
    if (before.isNegative) before = Duration.zero;
    return exp.subtract(before);
  }

  Future<T> get() {
    final c = current;
    if (c != null && !_stale && now().isBefore(_renewAt(c))) {
      return Future.value(c);
    }
    return renew();
  }

  Future<T> renew() {
    if (_closed) return Future.error(StateError('the session is closed'));
    return _inflight ??= _run();
  }

  Future<T> _run() async {
    try {
      final c = await _exchange(current);
      current = c;
      _obtainedAt = now();
      _stale = false;
      _schedule(c);
      onRenewed?.call(c);
      return c;
    } finally {
      _inflight = null;
    }
  }

  void markStale() => _stale = true;

  void close() {
    _closed = true;
    _timer?.cancel();
  }

  void _schedule(T c) {
    _timer?.cancel();
    if (!autoRenew || _closed) return;
    var delay = _renewAt(c).difference(now());
    if (delay < const Duration(seconds: 1)) delay = const Duration(seconds: 1);
    _timer = Timer(delay, () {
      unawaited(
          renew().then<void>((_) {}, onError: (Object e) => onError?.call(e)));
    });
  }
}

DateTime _expiry(Object? value, String what) {
  final t = value is String ? DateTime.tryParse(value) : null;
  if (t == null) throw StateError('the $what expiry is malformed');
  return t;
}

bool _isAuthRefusal(Object e) => e is CfMeetApiError && e.status == 401;

/// Shared configuration of the device clients.
class _DeviceContext {
  _DeviceContext({
    required this.client,
    required this.realtime,
    required this.installationIds,
    required this.renewBefore,
    required this.autoRenew,
    required this.now,
  });

  final CfMeetClient client;
  final RealtimeClient realtime;
  final InstallationIdSource installationIds;
  final Duration renewBefore;
  final bool autoRenew;
  final DateTime Function() now;

  Future<String> installation(String? given) async {
    final id = given ?? await installationIds.get();
    if (!isValidInstallationId(id)) {
      throw const CfMeetUsageError(
          'installationId must be 8-128 characters of [A-Za-z0-9._-]');
    }
    return id;
  }
}

/// A CF Meet device client for one partner user.
class CfMeetDeviceClient {
  /// [tokenProvider] returns a fresh CF Meet user token (`cfut_...`) from YOUR server,
  /// which mints it for the signed-in user (`client.users.upsertWithToken`).
  factory CfMeetDeviceClient({
    required String baseUrl,
    required FutureOr<String> Function() tokenProvider,
    Duration refreshBeforeExpiry = const Duration(seconds: 60),
    String? installationId,
    KeyValueStorage? storage,
    String installationIdKey = defaultInstallationIdKey,
    CfMeetTransport? transport,
    RetryOptions retryOptions = const RetryOptions(),
    Duration timeout = const Duration(seconds: 30),
    RealtimeConnector? connector,
    ReconnectOptions reconnectOptions = const ReconnectOptions(),
    Duration renewBeforeExpiry = const Duration(seconds: 60),
    bool autoRenew = true,
    bool allowInsecureHttp = false,
    DateTime Function()? now,
  }) {
    if (installationId != null && !isValidInstallationId(installationId)) {
      throw const CfMeetUsageError(
          'installationId must be 8-128 characters of [A-Za-z0-9._-]');
    }
    final clock = now ?? DateTime.now;
    final auth = CfMeetAuth.userToken(tokenProvider,
        refreshBeforeExpiry: refreshBeforeExpiry, now: clock);
    final client = CfMeetClient(
      baseUrl: baseUrl,
      auth: auth,
      transport: transport,
      retryOptions: retryOptions,
      timeout: timeout,
      allowInsecureHttp: allowInsecureHttp,
    );
    final store = InstallationIdStore(storage: storage, key: installationIdKey);
    final ctx = _DeviceContext(
      client: client,
      realtime: RealtimeClient(client,
          connector: connector, options: reconnectOptions),
      installationIds:
          installationId != null ? fixedInstallationId(installationId) : store,
      renewBefore: renewBeforeExpiry,
      autoRenew: autoRenew,
      now: clock,
    );
    return CfMeetDeviceClient._(client, auth, store, ctx);
  }

  CfMeetDeviceClient._(this.client, this.auth, this.installationIds, this._ctx)
      : chat = ChatClient._(_ctx),
        calls = CallsClient._(_ctx),
        meetings = MeetingsDeviceClient._(_ctx),
        push = PushClient._(_ctx);

  /// The underlying API client (authenticated with the user token).
  final CfMeetClient client;

  /// The user-token auth (cached, refreshed before expiry and after a refusal).
  final CfMeetAuth auth;

  /// The persisted installation id store.
  final InstallationIdStore installationIds;
  final _DeviceContext _ctx;

  final ChatClient chat;
  final CallsClient calls;
  final MeetingsDeviceClient meetings;
  final PushClient push;

  /// This installation's id (generated and persisted on first use).
  Future<String> installationId() => _ctx.installationIds.get();

  /// A valid user token (from the cache, or the provider).
  Future<String> userToken() => auth.token();

  /// The UNVERIFIED claims of the current user token: display only.
  Future<UserTokenClaims?> claims() async =>
      decodeUserTokenClaims(await userToken());

  /// Releases the HTTP connections.
  void close() => client.close();
}

// ---------------------------------------------------------------------------------------
// Chat

/// `POST /v1/messaging/user-sessions` answer: a MANAGED-only partner session device and
/// its client token.
class MessagingUserSession {
  MessagingUserSession(this.json)
      : clientTokenExpiresAt =
            _expiry(json['clientTokenExpiresAt'], 'client token'),
        sessionDeviceExpiresAt =
            _expiry(json['sessionDeviceExpiresAt'], 'session device') {
    if (json['clientToken'] is! String ||
        json['principalId'] is! String ||
        json['sessionDeviceId'] is! String) {
      throw StateError('the messaging user session response is malformed');
    }
  }

  final Map<String, dynamic> json;
  final DateTime clientTokenExpiresAt;
  final DateTime sessionDeviceExpiresAt;

  String get principalId => json['principalId'] as String;
  String get sessionDeviceId => json['sessionDeviceId'] as String;
  String get clientToken => json['clientToken'] as String;

  DateTime get expiresAt =>
      clientTokenExpiresAt.isBefore(sessionDeviceExpiresAt)
          ? clientTokenExpiresAt
          : sessionDeviceExpiresAt;

  @override
  String toString() => 'MessagingUserSession($sessionDeviceId)';
}

/// `device.chat`: starts chat sessions.
class ChatClient {
  ChatClient._(this._ctx);

  final _DeviceContext _ctx;

  /// Exchanges the user token for a partner session device, then (unless
  /// `realtime: false`) opens the realtime channel.
  Future<ChatSession> connect(
      {String? installationId, bool realtime = true}) async {
    final id = await _ctx.installation(installationId);
    final session = ChatSession._(_ctx, id);
    await session._start(realtime);
    return session;
  }
}

/// One device's chat: REST with the client token, and the realtime event stream.
class ChatSession {
  ChatSession._(this._ctx, this.installationId) {
    _credential = _Renewing<MessagingUserSession>(
      _exchange,
      (s) => s.expiresAt,
      renewBefore: _ctx.renewBefore,
      autoRenew: _ctx.autoRenew,
      now: _ctx.now,
      onRenewed: (s) {
        if (!_renewals.isClosed) _renewals.add(s);
      },
      onError: _addError,
    );
    clientAuth = CfMeetAuth.provider(AuthMode.clientToken, () => clientToken());
  }

  final _DeviceContext _ctx;
  final String installationId;
  late final _Renewing<MessagingUserSession> _credential;

  /// Client-token auth for any CLIENT_TOKEN operation (renewed transparently).
  late final CfMeetAuth clientAuth;

  final StreamController<Object?> _messages =
      StreamController<Object?>.broadcast();
  final StreamController<MessagingUserSession> _renewals =
      StreamController<MessagingUserSession>.broadcast();
  final Completer<RealtimeClosed> _done = Completer<RealtimeClosed>();
  RealtimeConnection? _conn;
  StreamSubscription<RealtimeState>? _states;
  bool _closed = false;
  int _reopened = 0;

  /// Realtime frames (decoded JSON), e.g. `{"type": "message.created", ...}`.
  Stream<Object?> get messages => _messages.stream;

  /// Every (re)issued session device / client token.
  Stream<MessagingUserSession> get renewals => _renewals.stream;

  /// Completes once the session ended for good.
  Future<RealtimeClosed> get done => _done.future;

  /// The live realtime connection (null with `realtime: false` or after close).
  RealtimeConnection? get realtime => _conn;

  /// The user's principal id (the acting principal of every request).
  String get principalId {
    final c = _credential.current;
    if (c == null) throw StateError('the chat session is not connected');
    return c.principalId;
  }

  /// The current session device.
  MessagingUserSession? get sessionDevice => _credential.current;

  /// A valid client token (renewing the session device when it is about to expire).
  Future<String> clientToken() async => (await _credential.get()).clientToken;

  Future<void> _start(bool realtime) async {
    await _credential.get();
    if (realtime && !_closed) _openRealtime();
  }

  /// Sends a MANAGED message; a `clientMessageId` is generated when absent so a resend
  /// is deduplicated. [extra] adds other send fields (`replyToMessageId`, ...).
  Future<ApiResponse> sendMessage(String conversationId, String text,
          {String? clientMessageId,
          Map<String, Object?> extra = const {},
          CancelToken? cancelToken}) =>
      request('sendMessage',
          pathParams: {'conversationId': conversationId},
          body: {
            ...extra,
            'text': text,
            'clientMessageId': clientMessageId ?? newIdempotencyKey(),
          },
          cancelToken: cancelToken);

  /// One page of a conversation's messages.
  Future<ApiResponse> listMessages(String conversationId,
          {int? afterSequence,
          int? beforeSequence,
          int? limit,
          CancelToken? cancelToken}) =>
      request('listMessages',
          pathParams: {'conversationId': conversationId},
          query: {
            if (afterSequence != null) 'afterSequence': afterSequence,
            if (beforeSequence != null) 'beforeSequence': beforeSequence,
            if (limit != null) 'limit': limit,
          },
          cancelToken: cancelToken);

  /// One page of the user's conversations.
  Future<ApiResponse> listConversations(
          {int? limit, String? cursor, CancelToken? cancelToken}) =>
      request('listConversations',
          query: {
            if (limit != null) 'limit': limit,
            if (cursor != null) 'cursor': cursor,
          },
          cancelToken: cancelToken);

  /// Moves the user's read cursor to [sequence] (monotonic).
  Future<ApiResponse> markRead(String conversationId, int sequence,
          {CancelToken? cancelToken}) =>
      request('setReadCursor',
          pathParams: {'conversationId': conversationId},
          body: {'sequence': sequence},
          cancelToken: cancelToken);

  Future<ApiResponse> unreadCount(String conversationId,
          {CancelToken? cancelToken}) =>
      request('getUnreadCount',
          pathParams: {'conversationId': conversationId},
          cancelToken: cancelToken);

  /// Any Messaging operation a partner session device may call, with the client token
  /// and `cf-acting-principal` set. A refused client token is renewed and the call
  /// retried once.
  Future<ApiResponse> request(
    String name, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Map<String, String> headers = const {},
    Object? body,
    CancelToken? cancelToken,
  }) async {
    final op = findOperation('messaging', name);
    if (!op.auth.contains(AuthMode.clientToken.wire)) {
      throw CfMeetUsageError('messaging.$name does not accept a client token');
    }
    if (_closed) throw StateError('the chat session is closed');
    final acting = op.headers.any((h) => h.name == 'cf-acting-principal');
    Future<ApiResponse> call() async {
      final s = await _credential.get();
      return _ctx.client.request(op,
          pathParams: pathParams,
          query: query,
          headers: {
            ...headers,
            if (acting) 'cf-acting-principal': s.principalId,
          },
          body: body,
          cancelToken: cancelToken,
          auth: CfMeetAuth.clientToken(s.clientToken));
    }

    try {
      return await call();
    } on CfMeetApiError catch (e) {
      if (!_isAuthRefusal(e)) rethrow;
      _credential.markStale();
      return call();
    }
  }

  /// Ends the session: closes the realtime channel and stops renewing.
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _credential.close();
    final conn = _conn;
    _conn = null;
    await _states?.cancel();
    await conn?.close();
    _finish(const RealtimeClosed(byClient: true, reason: 'client closed'));
  }

  void _finish(RealtimeClosed info) {
    _closed = true;
    _credential.close();
    if (!_done.isCompleted) _done.complete(info);
    unawaited(_messages.close());
    unawaited(_renewals.close());
  }

  void _addError(Object e) {
    if (!_messages.isClosed) _messages.addError(e);
  }

  Future<MessagingUserSession> _exchange(MessagingUserSession? previous) async {
    Future<ApiResponse> send(String? sessionDeviceId) =>
        _ctx.client.request(DeviceRoutes.messagingUserSession, body: {
          'installationId': installationId,
          if (sessionDeviceId != null) 'sessionDeviceId': sessionDeviceId,
        });
    ApiResponse r;
    try {
      r = await send(previous?.sessionDeviceId);
    } on CfMeetApiError catch (e) {
      // The device ended (expired, revoked): start a new one for this installation.
      if (previous == null || e.status != 404) rethrow;
      r = await send(null);
    }
    return MessagingUserSession(r.json);
  }

  void _openRealtime() {
    final conn = _ctx.realtime.messaging(
      auth: clientAuth,
      onMessage: (m) {
        if (!_messages.isClosed) _messages.add(m);
      },
    );
    _conn = conn;
    unawaited(_states?.cancel());
    _states = conn.states.listen((s) {
      if (s == RealtimeState.open) _reopened = 0;
    });
    unawaited(conn.done.then((info) => _onDone(conn, info)));
  }

  Future<void> _onDone(RealtimeConnection conn, RealtimeClosed info) async {
    if (_closed || !identical(_conn, conn)) return;
    _conn = null;
    final err = info.error;
    final refused = (err != null && _isAuthRefusal(err)) ||
        (!info.byClient && (info.code == 4401 || info.code == 4001));
    // The client token was refused (device ended, token revoked): one fresh exchange
    // with the user token, then a new channel. Twice in a row: give up.
    if (refused && _reopened < 1) {
      _reopened++;
      _credential.markStale();
      try {
        await _credential.get();
        if (!_closed) _openRealtime();
      } catch (e) {
        _addError(e);
        _finish(RealtimeClosed(
            code: info.code, reason: info.reason, error: e, byClient: false));
      }
      return;
    }
    _finish(info);
  }
}

// ---------------------------------------------------------------------------------------
// Calls

/// `POST /v1/calling/user-sessions` answer.
class CallingUserSession {
  CallingUserSession(this.json)
      : expiresAt = _expiry(
            json['signalingCredentialExpiresAt'], 'signaling credential') {
    if (json['signalingCredential'] is! String ||
        json['endpointId'] is! String ||
        json['principalId'] is! String) {
      throw StateError('the calling user session response is malformed');
    }
  }

  final Map<String, dynamic> json;
  final DateTime expiresAt;

  String get principalId => json['principalId'] as String;
  String get endpointId => json['endpointId'] as String;
  String get signalingCredential => json['signalingCredential'] as String;

  @override
  String toString() => 'CallingUserSession($endpointId)';
}

/// An incoming call offer (from `call.incoming`, or `recoverInvitations()`).
class IncomingInvitation {
  const IncomingInvitation({
    required this.invitationId,
    required this.callId,
    required this.callKind,
    required this.callVersion,
    required this.expiresAt,
    required this.callerDisplayName,
  });

  final String invitationId;
  final String callId;
  final String callKind;

  /// The Call version to answer on (`expectedVersion`).
  final int callVersion;
  final String? expiresAt;
  final String? callerDisplayName;

  @override
  String toString() => 'IncomingInvitation($invitationId, call $callId)';
}

/// Device endpoint kinds.
enum DeviceKind {
  web('WEB'),
  mobile('MOBILE');

  const DeviceKind(this.wire);
  final String wire;
}

/// `device.calls`: registers this installation as a call endpoint of the user.
class CallsClient {
  CallsClient._(this._ctx);

  final _DeviceContext _ctx;

  /// Exchanges the user token for the endpoint and its signaling credential, then opens
  /// signaling (unless `signaling: false`). [kind] defaults to WEB in browser builds and
  /// MOBILE elsewhere.
  Future<DeviceCallSession> connect(
      {String? installationId, DeviceKind? kind, bool signaling = true}) async {
    final id = await _ctx.installation(installationId);
    final session = DeviceCallSession._(
        _ctx, id, kind ?? (isWebPlatform ? DeviceKind.web : DeviceKind.mobile));
    await session._start(signaling);
    return session;
  }
}

/// One device's call endpoint: signaling events and the invitation commands.
class DeviceCallSession {
  DeviceCallSession._(this._ctx, this.installationId, this.kind) {
    _credential = _Renewing<CallingUserSession>(
      (_) => _exchange(),
      (s) => s.expiresAt,
      renewBefore: _ctx.renewBefore,
      autoRenew: _ctx.autoRenew,
      now: _ctx.now,
      onError: (e) {
        if (!_signals.isClosed) _signals.addError(e);
      },
    );
    signalingAuth = CfMeetAuth.provider(
        AuthMode.signalingCredential, () => signalingCredential());
  }

  final _DeviceContext _ctx;
  final String installationId;
  final DeviceKind kind;
  late final _Renewing<CallingUserSession> _credential;

  /// Signaling-credential auth for any SIGNALING_CREDENTIAL operation.
  late final CfMeetAuth signalingAuth;

  final StreamController<Map<String, dynamic>> _signals =
      StreamController<Map<String, dynamic>>.broadcast();
  final StreamController<IncomingInvitation> _invitations =
      StreamController<IncomingInvitation>.broadcast();
  final Completer<RealtimeClosed> _done = Completer<RealtimeClosed>();
  final Map<String, String> _invitationCall = {};
  final Map<String, int> _callVersion = {};
  RealtimeConnection? _conn;
  StreamSubscription<RealtimeState>? _states;
  bool _closed = false;
  int _reopened = 0;

  /// Every frame of the signaling channel (call signals and control frames).
  Stream<Map<String, dynamic>> get signals => _signals.stream;

  /// Incoming calls: alert the user (CallKit / ConnectionService / your UI), then
  /// [accept] or [decline].
  Stream<IncomingInvitation> get invitations => _invitations.stream;

  Future<RealtimeClosed> get done => _done.future;
  RealtimeConnection? get realtime => _conn;

  String get principalId => _session().principalId;
  String get endpointId => _session().endpointId;

  /// A valid signaling credential (exchanged again when it is about to expire).
  Future<String> signalingCredential() async =>
      (await _credential.get()).signalingCredential;

  Future<void> _start(bool signaling) async {
    await _credential.get();
    if (signaling && !_closed) _openSignaling();
  }

  CallingUserSession _session() {
    final c = _credential.current;
    if (c == null) throw StateError('the call session is not connected');
    return c;
  }

  /// Tells CF Meet the device is ringing (PENDING → RINGING).
  Future<ApiResponse> acknowledge(String invitationId,
          {CancelToken? cancelToken}) =>
      request('acknowledgeInvitation',
          pathParams: {'invitation_id': invitationId},
          cancelToken: cancelToken);

  /// Answers the call; [expectedVersion] defaults to the latest call version seen.
  Future<ApiResponse> accept(String invitationId,
          {int? expectedVersion, CancelToken? cancelToken}) async =>
      request('acceptInvitation',
          pathParams: {'invitation_id': invitationId},
          body: {'expectedVersion': _version(invitationId, expectedVersion)},
          cancelToken: cancelToken);

  /// Declines the call.
  Future<ApiResponse> decline(String invitationId,
          {int? expectedVersion, CancelToken? cancelToken}) async =>
      request('declineInvitation',
          pathParams: {'invitation_id': invitationId},
          body: {'expectedVersion': _version(invitationId, expectedVersion)},
          cancelToken: cancelToken);

  /// The media credential of an accepted call's session (`sessionId` of the accept
  /// result): the media gateway `url` and join `token`.
  Future<ApiResponse> mediaCredential(String sessionId,
          {CancelToken? cancelToken}) =>
      request('issueOwnMediaCredential',
          pathParams: {'session_id': sessionId},
          body: const <String, Object?>{},
          cancelToken: cancelToken);

  /// The endpoint's actionable offers (e.g. after a push woke the app).
  Future<List<IncomingInvitation>> recoverInvitations(
      {CancelToken? cancelToken}) async {
    final r = await request('recoverInvitations', cancelToken: cancelToken);
    final out = <IncomingInvitation>[];
    final data = r.json['data'];
    if (data is! List) return out;
    for (final item in data) {
      if (item is! Map<String, dynamic>) continue;
      final id = item['invitationId'];
      final callId = item['callId'];
      final version = item['callVersion'];
      if (id is! String || callId is! String || version is! int) continue;
      _remember(id, callId, version);
      if (item['actionable'] == true) {
        out.add(IncomingInvitation(
          invitationId: id,
          callId: callId,
          callKind: item['callKind'] as String? ?? '',
          callVersion: version,
          expiresAt: item['expiresAt'] as String?,
          callerDisplayName: item['callerDisplayName'] as String?,
        ));
      }
    }
    return out;
  }

  /// Any SIGNALING_CREDENTIAL calling operation; a refused credential is exchanged
  /// again and the call retried once.
  Future<ApiResponse> request(
    String name, {
    Map<String, Object?> pathParams = const {},
    Map<String, Object?> query = const {},
    Object? body,
    CancelToken? cancelToken,
  }) async {
    final op = findOperation('calling', name);
    if (!op.auth.contains(AuthMode.signalingCredential.wire)) {
      throw CfMeetUsageError(
          'calling.$name does not accept a signaling credential');
    }
    if (_closed) throw StateError('the call session is closed');
    Future<ApiResponse> call() async {
      final credential = await signalingCredential();
      return _ctx.client.request(op,
          pathParams: pathParams,
          query: query,
          body: body,
          cancelToken: cancelToken,
          auth: CfMeetAuth.signalingCredential(credential));
    }

    try {
      return await call();
    } on CfMeetApiError catch (e) {
      if (!_isAuthRefusal(e)) rethrow;
      _credential.markStale();
      return call();
    }
  }

  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _credential.close();
    final conn = _conn;
    _conn = null;
    await _states?.cancel();
    await conn?.close();
    _finish(const RealtimeClosed(byClient: true, reason: 'client closed'));
  }

  void _finish(RealtimeClosed info) {
    _closed = true;
    _credential.close();
    if (!_done.isCompleted) _done.complete(info);
    unawaited(_signals.close());
    unawaited(_invitations.close());
  }

  void _remember(String? invitationId, String callId, int version) {
    if (invitationId != null) _invitationCall[invitationId] = callId;
    if (version > (_callVersion[callId] ?? -1)) _callVersion[callId] = version;
  }

  int _version(String invitationId, int? expectedVersion) {
    if (expectedVersion != null) return expectedVersion;
    final callId = _invitationCall[invitationId];
    final v = callId == null ? null : _callVersion[callId];
    if (v == null) {
      throw CfMeetUsageError(
          'unknown call version for invitation $invitationId: pass expectedVersion');
    }
    return v;
  }

  Future<CallingUserSession> _exchange() async {
    final r = await _ctx.client.request(DeviceRoutes.callingUserSession,
        body: {'installationId': installationId, 'kind': kind.wire});
    return CallingUserSession(r.json);
  }

  void _onFrame(Object? frame) {
    if (frame is! Map<String, dynamic>) return;
    final type = frame['type'];
    final callId = frame['callId'];
    final version = frame['callVersion'];
    if (type is String &&
        type.startsWith('call.') &&
        callId is String &&
        version is int) {
      final inv = frame['invitation'];
      final invitationId = inv is Map && inv['invitationId'] is String
          ? inv['invitationId'] as String
          : null;
      _remember(invitationId, callId, version);
      if (type == 'call.incoming' && invitationId != null && inv is Map) {
        if (!_invitations.isClosed) {
          _invitations.add(IncomingInvitation(
            invitationId: invitationId,
            callId: callId,
            callKind: frame['callKind'] as String? ?? '',
            callVersion: version,
            expiresAt: inv['expiresAt'] as String?,
            callerDisplayName: inv['callerDisplayName'] as String?,
          ));
        }
      }
    }
    if (!_signals.isClosed) _signals.add(frame);
  }

  void _openSignaling() {
    final conn = _ctx.realtime
        .callingSignaling(auth: signalingAuth, onMessage: _onFrame);
    _conn = conn;
    unawaited(_states?.cancel());
    _states = conn.states.listen((s) {
      if (s == RealtimeState.open) _reopened = 0;
    });
    unawaited(conn.done.then((info) => _onDone(conn, info)));
  }

  Future<void> _onDone(RealtimeConnection conn, RealtimeClosed info) async {
    if (_closed || !identical(_conn, conn)) return;
    _conn = null;
    final err = info.error;
    // 4001: the credential expired or was revoked; 401 at the exchange or upgrade.
    final refused = (err != null &&
            (_isAuthRefusal(err) ||
                (err is RealtimeHandshakeException && err.status == 401))) ||
        (!info.byClient && info.code == 4001);
    if (refused && _reopened < 1) {
      _reopened++;
      _credential.markStale();
      try {
        await _credential.get();
        if (!_closed) _openSignaling();
      } catch (e) {
        if (!_signals.isClosed) _signals.addError(e);
        _finish(RealtimeClosed(
            code: info.code, reason: info.reason, error: e, byClient: false));
      }
      return;
    }
    _finish(info);
  }
}

// ---------------------------------------------------------------------------------------
// Meetings

/// The RTC (LiveKit) credential of a joined participant.
class RtcCredential {
  const RtcCredential(this.json);

  final Map<String, dynamic> json;

  /// e.g. `livekit`.
  String get provider => json['provider'] as String? ?? '';

  /// The RTC server URL (`wss://...`).
  String get url => json['url'] as String? ?? '';

  /// The short-lived provider credential (a LiveKit access token).
  String get credential => json['credential'] as String? ?? '';
  DateTime? get expiresAt => json['expiresAt'] is String
      ? DateTime.tryParse(json['expiresAt'] as String)
      : null;

  @override
  String toString() => 'RtcCredential($provider $url, credential: <redacted>)';
}

/// `device.meetings`: joins conferences the user token grants (`grt.mtg`).
class MeetingsDeviceClient {
  MeetingsDeviceClient._(this._ctx);

  final _DeviceContext _ctx;

  /// `POST /api/v2/conferences/{id}/join` with the user token. The role comes from the
  /// grant and the display name from the user profile. Hand [MeetingJoin.rtc] to the
  /// LiveKit client SDK (`room.connect(rtc.url, rtc.credential)`).
  ///
  /// [installationId] is accepted for symmetry and NOT sent (the join body has no
  /// installation field): a re-join of the same user keeps the same participant.
  Future<MeetingJoin> join(
    String conferenceId, {
    String? installationId,
    List<Map<String, Object?>>? consents,
    String? previousSessionId,
    CancelToken? cancelToken,
  }) async {
    final r = await _ctx.client.request(DeviceRoutes.joinConference,
        pathParams: {'id': conferenceId},
        body: {
          if (consents != null) 'consents': consents,
          if (previousSessionId != null) 'previousSessionId': previousSessionId,
        },
        cancelToken: cancelToken);
    final j = r.json;
    if (j['participant'] is! Map) {
      throw StateError('the join response is malformed');
    }
    return MeetingJoin._(_ctx, conferenceId, r);
  }
}

/// A joined conference: the RTC credential, the participant token and follow-ups.
class MeetingJoin {
  MeetingJoin._(this._ctx, this.conferenceId, this.response)
      : participantToken = response.json['participantToken'] is String
            ? response.json['participantToken'] as String
            : null,
        _rtc = response.json['rtc'] is Map<String, dynamic>
            ? RtcCredential(response.json['rtc'] as Map<String, dynamic>)
            : null;

  final _DeviceContext _ctx;
  final String conferenceId;
  final ApiResponse response;

  /// The participant credential for every other participant operation.
  final String? participantToken;
  RtcCredential? _rtc;

  Map<String, dynamic> get json => response.json;

  /// Auth for participant operations, null when no token was issued.
  CfMeetAuth? get participantAuth => participantToken == null
      ? null
      : CfMeetAuth.participantToken(participantToken!);

  /// The RTC credential; null while waiting in the lobby (`rtcStatus`
  /// `NOT_ISSUED_WAITING`): call [renewMediaCredential] once admitted.
  RtcCredential? get rtc => _rtc;

  /// A fresh media credential for this participant session.
  Future<RtcCredential> renewMediaCredential({CancelToken? cancelToken}) async {
    final session = json['participantSession'];
    final sessionId = session is Map ? session['id'] : null;
    if (sessionId is! String) {
      throw StateError('the join returned no participant session');
    }
    final r = await _ctx.client.operation('meetings', 'v2MediaCredential').call(
        pathParams: {'id': conferenceId},
        body: {'participantSessionId': sessionId},
        auth: _participant(),
        cancelToken: cancelToken);
    final rtc = r.json['rtc'];
    if (rtc is! Map<String, dynamic>) {
      throw StateError('the media credential response is malformed');
    }
    return _rtc = RtcCredential(rtc);
  }

  /// The conference realtime event stream, as this participant.
  RealtimeConnection realtime({void Function(Object?)? onMessage}) =>
      _ctx.realtime
          .meetings(conferenceId, auth: _participant(), onMessage: onMessage);

  CfMeetAuth _participant() {
    final a = participantAuth;
    if (a == null) throw StateError('the join issued no participant token');
    return a;
  }
}

// ---------------------------------------------------------------------------------------
// Push

/// Push platforms of a partner device.
enum PushPlatform {
  /// The regular APNs token (chat).
  apns('APNS'),

  /// The PushKit VoIP token (incoming calls).
  apnsVoip('APNS_VOIP'),

  /// The Firebase Cloud Messaging registration token.
  fcm('FCM');

  const PushPlatform(this.wire);
  final String wire;
}

/// `device.push`: registers this installation's push handle for the user.
class PushClient {
  PushClient._(this._ctx);

  final _DeviceContext _ctx;

  /// `PUT /v1/messaging/user-push-registrations/{installationId}`. Call again whenever
  /// the OS rotates the handle.
  Future<Map<String, dynamic>> register({
    String? installationId,
    required PushPlatform platform,
    required String handle,
    CancelToken? cancelToken,
  }) async {
    if (handle.isEmpty || handle.length > 4096) {
      throw const CfMeetUsageError('handle must be 1-4096 characters');
    }
    final id = await _ctx.installation(installationId);
    final r = await _ctx.client.request(DeviceRoutes.registerUserPush,
        pathParams: {'installationId': id},
        body: {'platform': platform.wire, 'handle': handle},
        cancelToken: cancelToken);
    return r.json;
  }

  /// `DELETE …/{installationId}?platform=`: one platform, or all when omitted.
  Future<void> unregister(
      {String? installationId,
      PushPlatform? platform,
      CancelToken? cancelToken}) async {
    final id = await _ctx.installation(installationId);
    await _ctx.client.request(DeviceRoutes.unregisterUserPush,
        pathParams: {'installationId': id},
        query: {if (platform != null) 'platform': platform.wire},
        cancelToken: cancelToken);
  }
}
