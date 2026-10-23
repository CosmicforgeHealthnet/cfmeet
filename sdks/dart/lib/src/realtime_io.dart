/// `dart:io` WebSocket connector. The upgrade is performed over [HttpClient] so that an
/// HTTP refusal (401 / 403 / 429 ...) is visible as a status, then the detached socket
/// becomes a client [WebSocket].
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:crypto/crypto.dart';

import 'realtime.dart';

const String _guid = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

class _IoSocket implements RealtimeSocket {
  _IoSocket(this._ws);

  final WebSocket _ws;

  @override
  Stream<Object?> get stream => _ws;
  @override
  String? get protocol => _ws.protocol;
  @override
  void add(String text) => _ws.add(text);
  @override
  Future<void> close([int? code, String? reason]) => _ws.close(code, reason);
  @override
  int? get closeCode => _ws.closeCode;
  @override
  String? get closeReason => _ws.closeReason;
}

/// Opens a client WebSocket with `dart:io`. [httpClient] overrides the client (tests,
/// proxies, certificates).
RealtimeConnector ioRealtimeConnector(
    {HttpClient? httpClient, Duration? pingInterval}) {
  return (RealtimeHandshake h) async {
    final client = httpClient ?? HttpClient();
    try {
      final httpUrl =
          h.url.replace(scheme: h.url.scheme == 'wss' ? 'https' : 'http');
      final random = Random.secure();
      final key =
          base64.encode(List<int>.generate(16, (_) => random.nextInt(256)));
      final HttpClientResponse resp;
      try {
        final req = await client.openUrl('GET', httpUrl).timeout(h.timeout);
        req.followRedirects = false;
        req.headers
          ..set(HttpHeaders.connectionHeader, 'Upgrade')
          ..set(HttpHeaders.upgradeHeader, 'websocket')
          ..set('sec-websocket-version', '13')
          ..set('sec-websocket-key', key);
        if (h.protocols.isNotEmpty) {
          req.headers.set('sec-websocket-protocol', h.protocols.join(', '));
        }
        h.headers.forEach(req.headers.set);
        resp = await req.close().timeout(h.timeout);
      } on TimeoutException {
        throw const RealtimeHandshakeException('handshake timed out');
      } on SocketException catch (e) {
        throw RealtimeHandshakeException(
            'connection failed (${e.osError?.errorCode})');
      } on HttpException {
        throw const RealtimeHandshakeException('handshake failed');
      }
      if (resp.statusCode != HttpStatus.switchingProtocols) {
        String? code;
        try {
          final body = await utf8.decodeStream(resp).timeout(h.timeout);
          final decoded = jsonDecode(body);
          if (decoded is Map && decoded['error'] is Map) {
            final c = (decoded['error'] as Map)['code'];
            if (c is String) code = c;
          }
        } catch (_) {
          // Not a JSON error envelope.
        }
        throw RealtimeHandshakeException('upgrade refused',
            status: resp.statusCode, code: code);
      }
      final accept = resp.headers.value('sec-websocket-accept');
      final expected =
          base64.encode(sha1.convert(utf8.encode('$key$_guid')).bytes);
      if (accept != expected) {
        throw const RealtimeHandshakeException('invalid Sec-WebSocket-Accept');
      }
      final selected = resp.headers.value('sec-websocket-protocol');
      if (selected != null &&
          (!h.protocols.contains(selected) ||
              selected.startsWith('cfmeet.ticket.'))) {
        throw const RealtimeHandshakeException(
            'the server selected an unexpected subprotocol');
      }
      // Ownership of the socket passes to the returned RealtimeSocket (closed by
      // RealtimeConnection).
      // ignore: close_sinks
      final socket = await resp.detachSocket();
      // ignore: close_sinks
      final ws = WebSocket.fromUpgradedSocket(socket,
          protocol: selected, serverSide: false);
      ws.pingInterval = pingInterval ?? const Duration(seconds: 30);
      return _IoSocket(ws);
    } finally {
      if (httpClient == null) client.close();
    }
  };
}

/// The default connector of this platform.
final RealtimeConnector defaultRealtimeConnector = ioRealtimeConnector();
