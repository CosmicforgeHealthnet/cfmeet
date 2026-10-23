/// A deterministic in-memory [RealtimeConnector] for application tests: observe each
/// handshake (URL, subprotocols, headers, ticket placement), push server frames, close
/// with a code, and script refused or failed handshakes, so reconnect logic is testable
/// without a network.
library;

import 'dart:async';
import 'dart:convert';

import '../realtime.dart';

/// One fake WebSocket the client opened through a [FakeRealtimeConnector].
class FakeRealtimeSocket implements RealtimeSocket {
  FakeRealtimeSocket._(this.handshake, this.index, this.protocol) {
    _incoming = StreamController<Object?>(onListen: () {
      if (!_listening.isCompleted) _listening.complete();
    });
  }

  /// What the client asked for: URL, offered subprotocols, handshake headers.
  final RealtimeHandshake handshake;

  /// 1-based connection number.
  final int index;

  @override
  final String? protocol;

  late final StreamController<Object?> _incoming;
  final Completer<void> _listening = Completer<void>();
  final List<String> _sent = [];
  bool _closed = false;
  bool _closedByClient = false;
  int? _closeCode;
  String? _closeReason;

  /// Completes once the client reads the socket (it considers the connection open).
  Future<void> get listening => _listening.future;

  @override
  Stream<Object?> get stream => _incoming.stream;

  /// Frames the client sent, in order.
  List<String> get sent => List.unmodifiable(_sent);

  /// [sent] JSON-decoded (non-JSON frames as text).
  List<Object?> get sentJson => [
        for (final f in _sent)
          () {
            try {
              return jsonDecode(f);
            } on FormatException {
              return f;
            }
          }(),
      ];

  bool get isClosed => _closed;

  /// True when the client closed this socket (e.g. `RealtimeConnection.close`).
  bool get closedByClient => _closedByClient;

  @override
  int? get closeCode => _closeCode;
  @override
  String? get closeReason => _closeReason;

  Uri get url => handshake.url;
  List<String> get protocols => handshake.protocols;

  /// Handshake headers, names lower-case.
  Map<String, String> get headers =>
      {for (final e in handshake.headers.entries) e.key.toLowerCase(): e.value};

  /// The ticket of a `cfmeet.ticket.<ticket>` subprotocol (Messaging).
  String? get ticketSubprotocol {
    for (final p in handshake.protocols) {
      if (p.startsWith(RealtimeClient.ticketSubprotocolPrefix)) {
        return p.substring(RealtimeClient.ticketSubprotocolPrefix.length);
      }
    }
    return null;
  }

  /// The `ticket` query parameter (Meetings).
  String? get queryTicket => handshake.url.queryParameters['ticket'];

  /// The ticket wherever the channel carries it.
  String? get ticket => ticketSubprotocol ?? queryTicket;

  /// The `authorization` handshake header (Calling signaling).
  String? get authorization => headers['authorization'];

  @override
  void add(String text) {
    if (_closed) throw StateError('the fake socket is closed');
    _sent.add(text);
  }

  @override
  Future<void> close([int? code, String? reason]) async {
    if (_closed) return;
    _closedByClient = true;
    _finish(code ?? 1005, reason);
  }

  /// Delivers a text frame.
  void pushText(String text) {
    _checkOpen();
    _incoming.add(text);
  }

  /// Delivers [json] as a JSON text frame.
  void pushJson(Object? json) => pushText(jsonEncode(json));

  /// Delivers a binary frame.
  void pushBinary(List<int> bytes) {
    _checkOpen();
    _incoming.add(bytes);
  }

  /// The server closes the connection with [code] (e.g. 1011 → reconnect, 4401 → stop).
  void closeFromServer(int code, [String reason = '']) {
    _checkOpen();
    _finish(code, reason);
  }

  /// The connection drops with a transport error (no close frame: code null).
  void failWithError([Object error = 'connection reset']) {
    _checkOpen();
    _incoming.addError(error);
    _finish(null, null);
  }

  void _checkOpen() {
    if (_closed) throw StateError('the fake socket is closed');
  }

  void _finish(int? code, String? reason) {
    _closed = true;
    _closeCode = code;
    _closeReason = reason;
    // Not awaited: the done event is delivered when (if ever) the client listens.
    unawaited(_incoming.close());
  }

  @override
  String toString() => 'FakeRealtimeSocket(#$index ${handshake.url.path})';
}

/// A [RealtimeConnector] that opens [FakeRealtimeSocket]s:
/// `RealtimeClient(client, connector: fake.connector)`.
class FakeRealtimeConnector {
  FakeRealtimeConnector({this.selectProtocol});

  /// The subprotocol the fake server selects (default: the first offered one that is
  /// neither a ticket nor a credential, or null).
  final String? Function(RealtimeHandshake handshake)? selectProtocol;

  final List<RealtimeHandshake> _handshakes = [];
  final List<FakeRealtimeSocket> _sockets = [];
  final List<Object> _failures = [];
  final Map<int, Completer<FakeRealtimeSocket>> _waiters = {};

  /// The connector to pass to `RealtimeClient` / `RealtimeConnection`.
  RealtimeConnector get connector => _connect;

  /// Every handshake attempted (refused ones included), in order.
  List<RealtimeHandshake> get handshakes => List.unmodifiable(_handshakes);

  /// Every socket opened, in order.
  List<FakeRealtimeSocket> get sockets => List.unmodifiable(_sockets);

  /// The latest socket (throws when none was opened).
  FakeRealtimeSocket get last => _sockets.last;

  /// The next handshake fails with an HTTP refusal ([status] 401 / 403 stop the
  /// connection; others are retried).
  void refuseNext({required int status, String? code}) =>
      _failures.add(RealtimeHandshakeException('upgrade refused',
          status: status, code: code));

  /// The next handshake fails at the network level (retried with backoff).
  void failNext([Object? error]) => _failures
      .add(error ?? const RealtimeHandshakeException('connection failed'));

  /// Completes with the [n]-th (1-based) socket once the client reads it.
  Future<FakeRealtimeSocket> connection(int n) async {
    if (n < 1) {
      throw RangeError.value(n, 'n', 'connections are numbered from 1');
    }
    final FakeRealtimeSocket socket;
    if (_sockets.length >= n) {
      socket = _sockets[n - 1];
    } else {
      socket = await (_waiters[n] ??= Completer<FakeRealtimeSocket>()).future;
    }
    await socket.listening;
    return socket;
  }

  Future<RealtimeSocket> _connect(RealtimeHandshake handshake) async {
    _handshakes.add(handshake);
    if (_failures.isNotEmpty) throw _failures.removeAt(0);
    final socket = FakeRealtimeSocket._(
        handshake,
        _sockets.length + 1,
        selectProtocol != null
            ? selectProtocol!(handshake)
            : handshake.protocols
                .where((p) =>
                    !p.startsWith(RealtimeClient.ticketSubprotocolPrefix) &&
                    !p.startsWith('bearer.'))
                .firstOrNull);
    _sockets.add(socket);
    _waiters.remove(socket.index)?.complete(socket);
    return socket;
  }
}
