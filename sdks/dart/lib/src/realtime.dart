/// Realtime WebSocket channels: Messaging realtime (`/v1/realtime`), Meeting conference
/// events (`/api/v2/conferences/{id}/realtime`) and Calling signaling
/// (`/v1/calling/signaling`), with ticket minting, reconnect with backoff and a FRESH
/// ticket / credential per connection, stopping on authentication failures and on
/// explicit close.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'auth.dart';
import 'cancel.dart';
import 'client.dart';
import 'errors.dart';
import 'operation.dart';
import 'realtime_io.dart' if (dart.library.js_interop) 'realtime_stub.dart'
    as platform_realtime;

/// What a connector needs to open one WebSocket.
class RealtimeHandshake {
  const RealtimeHandshake({
    required this.url,
    required this.protocols,
    required this.headers,
    required this.timeout,
  });

  final Uri url;

  /// Offered subprotocols (`Sec-WebSocket-Protocol`), in order.
  final List<String> protocols;

  /// Extra handshake headers (native platforms only; browsers cannot set them).
  final Map<String, String> headers;
  final Duration timeout;

  @override
  String toString() =>
      'RealtimeHandshake($url)'; // never the ticket / credential
}

/// The upgrade was refused with an HTTP answer (or failed at the network level when
/// [status] is null).
class RealtimeHandshakeException implements Exception {
  const RealtimeHandshakeException(this.message, {this.status, this.code});

  final String message;
  final int? status;

  /// The error envelope's code, when the refusal carried one.
  final String? code;

  bool get isAuthFailure => status == 401 || status == 403;

  @override
  String toString() =>
      'RealtimeHandshakeException($status ${code ?? ''}: $message)';
}

/// One open WebSocket.
abstract interface class RealtimeSocket {
  /// Incoming frames: `String` (text) or `List<int>` (binary).
  Stream<Object?> get stream;

  /// The subprotocol the server selected.
  String? get protocol;
  void add(String text);
  Future<void> close([int? code, String? reason]);
  int? get closeCode;
  String? get closeReason;
}

/// Opens a WebSocket (the default uses `dart:io`; browser builds pass their own).
typedef RealtimeConnector = Future<RealtimeSocket> Function(
    RealtimeHandshake handshake);

/// State of a [RealtimeConnection].
enum RealtimeState { connecting, open, reconnecting, closed }

/// Why a [RealtimeConnection] stopped.
class RealtimeClosed {
  const RealtimeClosed(
      {this.code, this.reason, this.error, required this.byClient});

  final int? code;
  final String? reason;

  /// The authentication / handshake failure or the exhausted reconnect error.
  final Object? error;

  /// True after [RealtimeConnection.close].
  final bool byClient;

  @override
  String toString() =>
      'RealtimeClosed(code: $code, byClient: $byClient${error == null ? '' : ', error: $error'})';
}

/// Close codes after which the connection does NOT reconnect: authorization ended
/// (Messaging 4401), the signaling credential expired or was revoked (Calling 4001),
/// and 4403 (forbidden).
const Set<int> defaultAuthFailureCloseCodes = {4001, 4401, 4403};

/// Reconnect configuration.
class ReconnectOptions {
  const ReconnectOptions({
    this.baseDelay = const Duration(milliseconds: 500),
    this.maxDelay = const Duration(seconds: 30),
    this.maxConsecutiveFailures,
    this.authFailureCloseCodes = defaultAuthFailureCloseCodes,
    this.handshakeTimeout = const Duration(seconds: 15),
  });

  final Duration baseDelay;
  final Duration maxDelay;

  /// Give up after this many failed connection attempts in a row (null: never).
  final int? maxConsecutiveFailures;
  final Set<int> authFailureCloseCodes;
  final Duration handshakeTimeout;

  Duration delay(int failures, Random random) {
    final ceiling = min(maxDelay.inMilliseconds,
        baseDelay.inMilliseconds * pow(2, min(failures, 16)).toInt());
    // Equal jitter: at least half the step, so a flapping server is not hammered.
    final half = ceiling ~/ 2;
    return Duration(
        milliseconds: half + random.nextInt(max(1, ceiling - half + 1)));
  }
}

/// A reconnecting WebSocket. Each (re)connection calls `prepare` for a fresh handshake
/// (a new single-use ticket, a refreshed credential, the latest resume cursor).
class RealtimeConnection {
  RealtimeConnection({
    required Future<RealtimeHandshake> Function() prepare,
    RealtimeConnector? connector,
    this.options = const ReconnectOptions(),
    void Function(Object? message)? onMessage,
    Random? random,
  })  : _prepare = prepare,
        _connector = connector ?? platform_realtime.defaultRealtimeConnector,
        _onMessage = onMessage,
        _random = random ?? Random.secure() {
    unawaited(_run());
  }

  final Future<RealtimeHandshake> Function() _prepare;
  final RealtimeConnector _connector;
  final void Function(Object? message)? _onMessage;
  final Random _random;
  final ReconnectOptions options;

  final StreamController<Object?> _messages =
      StreamController<Object?>.broadcast();
  final StreamController<RealtimeState> _states =
      StreamController<RealtimeState>.broadcast();
  final Completer<RealtimeClosed> _done = Completer<RealtimeClosed>();
  final CancelToken _stop = CancelToken();
  RealtimeSocket? _socket;
  RealtimeState _state = RealtimeState.connecting;

  /// Successful connections so far (1 after the first open).
  int connections = 0;

  /// Incoming frames: decoded JSON for JSON text frames, otherwise the raw frame.
  Stream<Object?> get messages => _messages.stream;
  Stream<RealtimeState> get states => _states.stream;
  RealtimeState get state => _state;

  /// Completes once the connection stopped for good.
  Future<RealtimeClosed> get done => _done.future;

  void _setState(RealtimeState s) {
    _state = s;
    if (!_states.isClosed) _states.add(s);
  }

  /// Sends a JSON frame (or a raw String). Throws [StateError] when not open.
  void send(Object frame) {
    final s = _socket;
    if (s == null || _state != RealtimeState.open) {
      throw StateError('the realtime connection is not open');
    }
    s.add(frame is String ? frame : jsonEncode(frame));
  }

  /// Closes the connection; it will not reconnect.
  Future<RealtimeClosed> close(
      [int code = 1000, String reason = 'client closed']) async {
    _stop.cancel(reason);
    await _socket?.close(code, reason);
    return done;
  }

  Future<void> _run() async {
    var failures = 0;
    RealtimeClosed? result;
    while (!_stop.isCancelled) {
      _setState(connections == 0
          ? RealtimeState.connecting
          : RealtimeState.reconnecting);
      RealtimeSocket socket;
      try {
        final handshake = await _prepare();
        if (_stop.isCancelled) break;
        socket = await _connector(handshake);
      } catch (e) {
        if (_isAuthFailure(e)) {
          result = RealtimeClosed(error: e, byClient: false);
          break;
        }
        failures++;
        final limit = options.maxConsecutiveFailures;
        if (limit != null && failures >= limit) {
          result = RealtimeClosed(error: e, byClient: false);
          break;
        }
        if (!await cancellableDelay(
            options.delay(failures - 1, _random), _stop)) {
          break;
        }
        continue;
      }
      if (_stop.isCancelled) {
        await socket.close(1000, 'client closed');
        break;
      }
      _socket = socket;
      failures = 0;
      connections++;
      _setState(RealtimeState.open);
      try {
        await for (final frame in socket.stream) {
          final decoded = _decode(frame);
          _onMessage?.call(decoded);
          if (!_messages.isClosed) _messages.add(decoded);
        }
      } catch (_) {
        // A transport error ends this connection like a close; reconnect below.
      }
      _socket = null;
      final code = socket.closeCode;
      if (_stop.isCancelled) {
        result = RealtimeClosed(
            code: code, reason: socket.closeReason, byClient: true);
        break;
      }
      if (code != null && options.authFailureCloseCodes.contains(code)) {
        result = RealtimeClosed(
            code: code, reason: socket.closeReason, byClient: false);
        break;
      }
      failures++;
      if (!await cancellableDelay(
          options.delay(failures - 1, _random), _stop)) {
        break;
      }
    }
    result ??= RealtimeClosed(byClient: true, reason: _stop.reason);
    _setState(RealtimeState.closed);
    if (!_done.isCompleted) _done.complete(result);
    await _messages.close();
    await _states.close();
  }

  static bool _isAuthFailure(Object e) =>
      (e is RealtimeHandshakeException && e.isAuthFailure) ||
      e is CfMeetAuthenticationError ||
      e is CfMeetPermissionError ||
      e is CfMeetUsageError;

  static Object? _decode(Object? frame) {
    if (frame is String && (frame.startsWith('{') || frame.startsWith('['))) {
      try {
        return jsonDecode(frame);
      } on FormatException {
        return frame;
      }
    }
    return frame;
  }
}

/// Mints tickets and opens the three realtime channels through a [CfMeetClient].
class RealtimeClient {
  RealtimeClient(this.client,
      {this.connector, this.options = const ReconnectOptions()});

  final CfMeetClient client;
  final RealtimeConnector? connector;
  final ReconnectOptions options;

  static const String messagingSubprotocol = 'cfmeet.messaging.v1';
  static const String ticketSubprotocolPrefix = 'cfmeet.ticket.';
  static const String signalingSubprotocol = 'cfmeet.signaling.v1';

  Uri _wsUrl(String path, [String query = '']) {
    final base = client.baseUri;
    final prefix = base.path.endsWith('/')
        ? base.path.substring(0, base.path.length - 1)
        : base.path;
    return Uri.parse(
        '${base.scheme == 'https' ? 'wss' : 'ws'}://${base.authority}$prefix$path'
        '${query.isEmpty ? '' : '?$query'}');
  }

  /// `POST /v1/realtime/tickets` (Messaging; client token): a single-use ticket.
  Future<Map<String, dynamic>> messagingTicket(
      {CfMeetAuth? auth, CancelToken? cancelToken}) async {
    final r = await client
        .operation('messaging', 'issueRealtimeTicket')
        .call(auth: auth, cancelToken: cancelToken);
    final t = r.json;
    if (t['ticket'] is! String || (t['ticket'] as String).isEmpty) {
      throw StateError('the ticket response carries no ticket');
    }
    return t;
  }

  /// `POST /api/v2/conferences/{id}/realtime/ticket` (Meetings): a one-shot ticket and the
  /// current event cursor.
  Future<Map<String, dynamic>> meetingTicket(String conferenceId,
      {CfMeetAuth? auth, CancelToken? cancelToken}) async {
    final r = await client.operation('meetings', 'v2RealtimeTicket').call(
        pathParams: {'id': conferenceId}, auth: auth, cancelToken: cancelToken);
    final t = r.json;
    if (t['ticket'] is! String || (t['ticket'] as String).isEmpty) {
      throw StateError('the ticket response carries no ticket');
    }
    return t;
  }

  /// The Messaging realtime gateway: subprotocols `cfmeet.messaging.v1` and
  /// `cfmeet.ticket.<ticket>` (the ticket never travels in the URL). A fresh ticket is
  /// minted (with [auth], a client token, or the client's default) for every connection.
  RealtimeConnection messaging(
      {CfMeetAuth? auth, void Function(Object?)? onMessage}) {
    final spec = client.operation('messaging', 'connectRealtime').spec;
    return RealtimeConnection(
      connector: connector,
      options: options,
      onMessage: onMessage,
      prepare: () async {
        final t = await messagingTicket(auth: auth);
        final ticket = t['ticket'] as String;
        if (!RegExp(r'^[A-Za-z0-9._~-]+$').hasMatch(ticket)) {
          throw StateError('malformed realtime ticket');
        }
        return RealtimeHandshake(
          url: _wsUrl(buildPath(spec, const {})),
          protocols: [messagingSubprotocol, '$ticketSubprotocolPrefix$ticket'],
          headers: const {},
          timeout: options.handshakeTimeout,
        );
      },
    );
  }

  /// A conference's event stream. Each connection mints a ticket and resumes after the
  /// last `sequence` received (or [cursor] / the ticket's cursor at first).
  RealtimeConnection meetings(String conferenceId,
      {CfMeetAuth? auth, int? cursor, void Function(Object?)? onMessage}) {
    final spec = client.operation('meetings', 'v2RealtimeSubscribe').spec;
    final path = buildPath(spec, {'id': conferenceId});
    int? last = cursor;
    return RealtimeConnection(
      connector: connector,
      options: options,
      onMessage: (m) {
        if (m is Map && m['sequence'] is int) last = m['sequence'] as int;
        onMessage?.call(m);
      },
      prepare: () async {
        final t = await meetingTicket(conferenceId, auth: auth);
        final from = last ?? (t['cursor'] is int ? t['cursor'] as int : null);
        final q = 'ticket=${Uri.encodeQueryComponent(t['ticket'] as String)}'
            '${from == null ? '' : '&cursor=$from'}';
        return RealtimeHandshake(
          url: _wsUrl(path, q),
          protocols: const [],
          headers: const {},
          timeout: options.handshakeTimeout,
        );
      },
    );
  }

  /// The endpoint's Calling signaling channel, authenticated with its signaling
  /// credential (`cfsig_...`) in the handshake's Authorization header (native) and the
  /// `cfmeet.signaling.v1` subprotocol. Use a [CfMeetAuth.provider] to present a fresh
  /// credential on every reconnect.
  RealtimeConnection callingSignaling(
      {required CfMeetAuth auth, void Function(Object?)? onMessage}) {
    if (auth.mode != AuthMode.signalingCredential) {
      throw const CfMeetUsageError(
          'the signaling channel needs a signaling credential');
    }
    final spec = client.operation('calling', 'openSignalingChannel').spec;
    return RealtimeConnection(
      connector: connector,
      options: options,
      onMessage: onMessage,
      prepare: () async => RealtimeHandshake(
        url: _wsUrl(buildPath(spec, const {})),
        protocols: const [signalingSubprotocol],
        headers: await auth.headers(),
        timeout: options.handshakeTimeout,
      ),
    );
  }

  /// The WebSocket operations of the catalogue.
  static List<OperationSpec> get channels => [
        findOperation('messaging', 'connectRealtime'),
        findOperation('meetings', 'v2RealtimeSubscribe'),
        findOperation('calling', 'openSignalingChannel'),
      ];
}
