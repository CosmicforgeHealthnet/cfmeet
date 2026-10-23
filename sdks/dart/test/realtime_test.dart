import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

const ReconnectOptions fast = ReconnectOptions(
  baseDelay: Duration(milliseconds: 10),
  maxDelay: Duration(milliseconds: 40),
  handshakeTimeout: Duration(seconds: 5),
);

/// A local gateway stand-in: ticket endpoints and WebSocket upgrades.
class RealtimeServer {
  RealtimeServer._(this._server);

  static Future<RealtimeServer> start() async {
    final s = RealtimeServer._(
        await HttpServer.bind(InternetAddress.loopbackIPv4, 0));
    s._server.listen(s._handle);
    return s;
  }

  final HttpServer _server;
  final List<String> minted = [];
  final Set<String> used = {};
  final List<HttpHeaders> upgrades = [];
  final List<Uri> upgradeUris = [];
  int ticketStatus = 201;

  /// Called with each accepted WebSocket and its 1-based connection number.
  late Future<void> Function(ServerWs ws, int n) onSocket;

  /// When set, upgrades are refused with this status.
  int? refuseUpgrade;

  /// Refuse this many upgrades with 503 first.
  int refuseFirst = 0;
  int _connections = 0;

  String get url => 'http://127.0.0.1:${_server.port}';

  String _ticket() {
    final t = (minted.length + 1).toRadixString(16).padLeft(64, '0');
    minted.add(t);
    return t;
  }

  Future<void> _json(HttpRequest req, int status, Object body) async {
    req.response.statusCode = status;
    req.response.headers.contentType = ContentType.json;
    req.response.write(jsonEncode(body));
    await req.response.close();
  }

  Future<void> _handle(HttpRequest req) async {
    final path = req.uri.path;
    if (req.method == 'POST' && path == '/v1/realtime/tickets') {
      if (ticketStatus != 201) {
        return _json(req, ticketStatus, {
          'error': {
            'code': 'AUTHENTICATION_FAILED',
            'message': 'no',
            'requestId': 'r'
          }
        });
      }
      return _json(req, 201, {
        'ticket': _ticket(),
        'expiresAt': '2026-10-23T12:00:00Z',
        'path': '/v1/realtime',
        'subprotocol': 'cfmeet.messaging.v1',
        'ticketSubprotocolPrefix': 'cfmeet.ticket.',
      });
    }
    if (req.method == 'POST' && path.endsWith('/realtime/ticket')) {
      return _json(req, 200, {
        'ticket': _ticket(),
        'expiresAt': '2026-10-23T12:00:00Z',
        'websocketPath': path.replaceFirst('/ticket', ''),
        'cursor': 7,
        'scope': 'FULL',
      });
    }
    if (WebSocketTransformer.isUpgradeRequest(req)) {
      upgrades.add(req.headers);
      upgradeUris.add(req.uri);
      if (refuseFirst > 0) {
        refuseFirst--;
        return _json(req, 503, {
          'error': {
            'code': 'SERVICE_UNAVAILABLE',
            'message': 'busy',
            'requestId': 'r'
          }
        });
      }
      if (refuseUpgrade != null) {
        return _json(req, refuseUpgrade!, {
          'error': {
            'code': 'AUTHENTICATION_FAILED',
            'message': 'no',
            'requestId': 'r'
          }
        });
      }
      // Single-use tickets: refuse a reused one.
      final offered = (req.headers['sec-websocket-protocol'] ?? [])
          .expand((v) => v.split(','))
          .map((s) => s.trim())
          .toList();
      final ticket = offered.firstWhere((p) => p.startsWith('cfmeet.ticket.'),
          orElse: () => '');
      final queryTicket = req.uri.queryParameters['ticket'];
      for (final t in [
        ticket.replaceFirst('cfmeet.ticket.', ''),
        queryTicket
      ]) {
        if (t == null || t.isEmpty) continue;
        if (!used.add(t)) {
          return _json(req, 401, {
            'error': {
              'code': 'TICKET_REUSED',
              'message': 'no',
              'requestId': 'r'
            }
          });
        }
      }
      // Closed by the test handlers (ServerWs.close) or by the client.
      // ignore: close_sinks
      final ws = await WebSocketTransformer.upgrade(req,
          protocolSelector: (protocols) {
        if (protocols.contains('cfmeet.messaging.v1')) {
          return 'cfmeet.messaging.v1';
        }
        if (protocols.contains('cfmeet.signaling.v1')) {
          return 'cfmeet.signaling.v1';
        }
        return null;
      });
      // Drain incoming frames so the server answers close handshakes.
      final incoming = StreamController<Object?>.broadcast();
      ws.listen(incoming.add, onDone: () => unawaited(incoming.close()));
      await onSocket(ServerWs(ws, incoming.stream), ++_connections);
      return;
    }
    req.response.statusCode = 404;
    await req.response.close();
  }

  Future<void> close() => _server.close(force: true);
}

void main() {
  late RealtimeServer server;

  setUp(() async => server = await RealtimeServer.start());
  tearDown(() => server.close());

  RealtimeClient realtime(CfMeetAuth auth) => RealtimeClient(
      CfMeetClient(baseUrl: server.url, auth: auth, maxRetries: 0),
      options: fast);

  test(
      'messaging: fresh ticket per reconnect, subprotocol placement, stop on 4401',
      () async {
    server.onSocket = (ws, n) async {
      ws.add(jsonEncode({'type': 'welcome', 'connection': n}));
      if (n == 1) {
        await ws.close(1011, 'restarting');
        return;
      }
      await for (final frame in ws.frames) {
        ws.add(
            jsonEncode({'type': 'echo', 'frame': jsonDecode(frame as String)}));
        await ws.close(4401, 'authorization ended');
      }
    };
    final conn = realtime(CfMeetAuth.clientToken('ct-1')).messaging();
    final received = <Object?>[];
    conn.messages.listen((m) {
      received.add(m);
      if (m is Map && m['type'] == 'welcome' && m['connection'] == 2) {
        conn.send({'type': 'ping'});
      }
    });
    final closed = await conn.done.timeout(const Duration(seconds: 10));
    expect(closed.code, 4401);
    expect(closed.byClient, isFalse);
    expect(conn.connections, 2);
    expect(conn.state, RealtimeState.closed);
    expect(server.minted, hasLength(2));
    expect(server.used, server.minted.toSet());
    for (var i = 0; i < 2; i++) {
      final offered = server.upgrades[i].value('sec-websocket-protocol')!;
      expect(offered, 'cfmeet.messaging.v1, cfmeet.ticket.${server.minted[i]}');
      expect(server.upgradeUris[i].path, '/v1/realtime');
      expect(server.upgradeUris[i].query, isEmpty,
          reason: 'the ticket never travels in the URL');
    }
    expect(received, [
      {'type': 'welcome', 'connection': 1},
      {'type': 'welcome', 'connection': 2},
      {
        'type': 'echo',
        'frame': {'type': 'ping'}
      },
    ]);
  });

  test('an HTTP 401 at the upgrade stops without reconnecting', () async {
    server.refuseUpgrade = 401;
    server.onSocket = (ws, n) async {};
    final conn = realtime(CfMeetAuth.clientToken('ct-1')).messaging();
    final closed = await conn.done.timeout(const Duration(seconds: 10));
    expect(closed.error, isA<RealtimeHandshakeException>());
    expect((closed.error as RealtimeHandshakeException).status, 401);
    expect((closed.error as RealtimeHandshakeException).code,
        'AUTHENTICATION_FAILED');
    expect(server.upgrades, hasLength(1));
  });

  test('a refused ticket mint (401) stops without reconnecting', () async {
    server.ticketStatus = 401;
    server.onSocket = (ws, n) async {};
    final conn = realtime(CfMeetAuth.clientToken('bad')).messaging();
    final closed = await conn.done.timeout(const Duration(seconds: 10));
    expect(closed.error, isA<CfMeetAuthenticationError>());
    expect(server.upgrades, isEmpty);
  });

  test('a 503 at the upgrade is retried with backoff and a new ticket',
      () async {
    server.refuseFirst = 2;
    server.onSocket = (ws, n) async {
      ws.add('{"type":"welcome"}');
    };
    final conn = realtime(CfMeetAuth.clientToken('ct')).messaging();
    final first =
        await conn.messages.first.timeout(const Duration(seconds: 10));
    expect(first, {'type': 'welcome'});
    expect(server.minted.length, server.upgrades.length);
    expect(server.minted, hasLength(3));
    final closed = await conn.close();
    expect(closed.byClient, isTrue);
  });

  test('explicit close stops the reconnect loop', () async {
    server.onSocket = (ws, n) async {
      ws.add('{"type":"welcome"}');
    };
    final conn = realtime(CfMeetAuth.clientToken('ct')).messaging();
    await conn.messages.first.timeout(const Duration(seconds: 10));
    final closed = await conn.close();
    expect(closed.byClient, isTrue);
    await Future<void>.delayed(const Duration(milliseconds: 200));
    expect(conn.connections, 1);
    expect(server.minted, hasLength(1));
    expect(() => conn.send({'type': 'ping'}), throwsStateError);
  });

  test('gives up after maxConsecutiveFailures when the server is unreachable',
      () async {
    final port = server.url.split(':').last;
    await server.close();
    final conn = RealtimeConnection(
      prepare: () async => RealtimeHandshake(
          url: Uri.parse('ws://127.0.0.1:$port/v1/realtime'),
          protocols: const ['cfmeet.messaging.v1'],
          headers: const {},
          timeout: const Duration(seconds: 2)),
      options: const ReconnectOptions(
          baseDelay: Duration(milliseconds: 5),
          maxDelay: Duration(milliseconds: 10),
          maxConsecutiveFailures: 3),
    );
    final closed = await conn.done.timeout(const Duration(seconds: 10));
    expect(closed.error, isA<RealtimeHandshakeException>());
    expect(conn.connections, 0);
    server = await RealtimeServer.start();
  });

  test('meetings: ticket in the query, resume after the last sequence',
      () async {
    server.onSocket = (ws, n) async {
      // Frames as the Meeting Service sends them: durable events wrap the envelope
      // (`{"type":"event","event":{...,"sequence":n}}`); transient ones have no sequence.
      Map<String, Object?> event(String id, int? sequence,
              {bool transient = false}) =>
          {
            'type': 'event',
            if (transient) 'transient': true,
            'event': {'eventId': id, 'eventType': 'x', 'sequence': sequence},
          };
      if (n == 1) {
        ws.add(jsonEncode({'type': 'subscribed', 'cursor': 7}));
        ws.add(jsonEncode(event('e1', 8)));
        ws.add(jsonEncode(event('e2', 9)));
        ws.add(jsonEncode(event('t1', null, transient: true)));
        await Future<void>.delayed(const Duration(milliseconds: 50));
        await ws.close(1011, 'restart');
      } else {
        ws.add(jsonEncode(event('e3', 10)));
      }
    };
    final conn = realtime(CfMeetAuth.participantToken('pt'))
        .meetings('11111111-2222-4333-8444-555555555555');
    final seqs = <int>[];
    final done = Completer<void>();
    conn.messages.listen((m) {
      final sequence = meetingResumeCursor(m);
      if ((m as Map)['type'] != 'event' || sequence == null) return;
      seqs.add(sequence);
      if (seqs.length == 3) done.complete();
    });
    await done.future.timeout(const Duration(seconds: 10));
    await conn.close();
    expect(seqs, [8, 9, 10]);
    expect(server.upgradeUris, hasLength(2));
    expect(server.upgradeUris[0].path,
        '/api/v2/conferences/11111111-2222-4333-8444-555555555555/realtime');
    expect(server.upgradeUris[0].queryParameters, {
      'ticket': server.minted[0],
      'cursor': '7'
    }); // the ticket's cursor at first
    expect(server.upgradeUris[1].queryParameters, {
      'ticket': server.minted[1],
      'cursor': '9'
    }); // then the last durable event's sequence
  });

  test('meetingResumeCursor reads the contract frames', () {
    expect(
        meetingResumeCursor({
          'type': 'event',
          'event': {'sequence': 12}
        }),
        12);
    expect(
        meetingResumeCursor({
          'type': 'event',
          'transient': true,
          'event': {'sequence': null}
        }),
        isNull);
    expect(meetingResumeCursor({'type': 'resync_required', 'cursor': 40}), 40);
    expect(meetingResumeCursor({'type': 'subscribed', 'cursor': 3}), isNull);
    expect(meetingResumeCursor({'sequence': 5}), isNull,
        reason: 'not a contract frame');
    expect(meetingResumeCursor('pong'), isNull);
  });

  test('calling signaling: credential header, subprotocol, stop on 4001',
      () async {
    var n = 0;
    server.onSocket = (ws, i) async {
      ws.add('{"type":"signaling.ready"}');
      await ws.close(4001, 'credential expired');
    };
    final conn = realtime(
            CfMeetAuth.platformCredential('cfmk_test_${'c' * 24}_${'d' * 64}'))
        .callingSignaling(
            auth: CfMeetAuth.provider(
                AuthMode.signalingCredential, () => 'cfsig_tok${++n}'));
    final closed = await conn.done.timeout(const Duration(seconds: 10));
    expect(closed.code, 4001);
    expect(server.upgrades.single.value('authorization'), 'Bearer cfsig_tok1');
    expect(server.upgrades.single.value('sec-websocket-protocol'),
        'cfmeet.signaling.v1');
    expect(
        () => realtime(CfMeetAuth.clientToken('x'))
            .callingSignaling(auth: CfMeetAuth.clientToken('x')),
        throwsA(isA<CfMeetUsageError>()));
  });
}

/// A server-side socket whose incoming frames are already being drained.
class ServerWs {
  ServerWs(this._ws, this._incoming);

  final WebSocket _ws;
  final Stream<Object?> _incoming;

  void add(String s) => _ws.add(s);
  Future<void> close(int code, String reason) => _ws.close(code, reason);
  Stream<Object?> get frames => _incoming;
}
