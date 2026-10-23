/// Platforms without `dart:io`: no default WebSocket connector.
library;

import 'errors.dart';
import 'realtime.dart';

/// Browser builds pass a [RealtimeConnector] (e.g. over `package:web_socket_channel`).
Future<RealtimeSocket> defaultRealtimeConnector(RealtimeHandshake handshake) =>
    throw const CfMeetUsageError(
        'no default WebSocket connector on this platform: pass RealtimeClient(connector: ...)');
