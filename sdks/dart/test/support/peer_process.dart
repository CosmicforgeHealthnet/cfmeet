/// Drives an interop peer process (this SDK's `interop_peer.dart` or the TypeScript
/// SDK's `test/support/interop-peer.ts`) over JSON lines on stdin / stdout.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

class PeerProcess {
  PeerProcess._(this._process) {
    _process.stderr.transform(utf8.decoder).listen((d) {
      _stderr.write(d);
    });
    _process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen((line) {
      if (!line.trim().startsWith('{')) return; // tool chatter
      if (_waiters.isNotEmpty) {
        _waiters.removeAt(0).complete(line);
      } else {
        _lines.add(line);
      }
    }, onDone: () {
      _ended = true;
      for (final w in _waiters) {
        w.complete(null);
      }
      _waiters.clear();
    });
  }

  final Process _process;
  final List<String> _lines = [];
  final List<Completer<String?>> _waiters = [];
  final StringBuffer _stderr = StringBuffer();
  bool _ended = false;
  late final String deviceId;
  late final String sdk;

  String get diagnostics {
    final s = _stderr.toString();
    return s.length > 8000 ? s.substring(s.length - 8000) : s;
  }

  /// Starts the peer and waits for its `ready` line.
  static Future<PeerProcess> start(String executable, List<String> args,
      {String? workingDirectory,
      Duration readyTimeout = const Duration(minutes: 3)}) async {
    final p = PeerProcess._(await Process.start(executable, args,
        workingDirectory: workingDirectory));
    final ready = await p._next(readyTimeout);
    if (ready['event'] != 'ready' || ready['deviceId'] is! String) {
      p.kill();
      throw StateError(
          'peer $executable did not become ready: $ready\n${p.diagnostics}');
    }
    p.deviceId = ready['deviceId'] as String;
    p.sdk = '${ready['sdk']}';
    return p;
  }

  Future<Map<String, dynamic>> _next(Duration timeout) async {
    String? line;
    if (_lines.isNotEmpty) {
      line = _lines.removeAt(0);
    } else if (_ended) {
      line = null;
    } else {
      final w = Completer<String?>();
      _waiters.add(w);
      line = await w.future.timeout(timeout,
          onTimeout: () => throw TimeoutException(
              'peer answer timed out\n$diagnostics', timeout));
    }
    if (line == null) throw StateError('peer exited\n$diagnostics');
    return jsonDecode(line) as Map<String, dynamic>;
  }

  /// Sends one command; returns the answer (throws on `{"ok": false}`).
  Future<Map<String, dynamic>> request(Map<String, Object?> command,
      {Duration timeout = const Duration(minutes: 1)}) async {
    _process.stdin.writeln(jsonEncode(command));
    await _process.stdin.flush();
    final answer = await _next(timeout);
    if (answer['ok'] != true) {
      throw StateError(
          'peer ${command['cmd']} failed: ${answer['error'] ?? answer}\n$diagnostics');
    }
    return answer;
  }

  /// Asks the peer to exit; returns its exit code.
  Future<int> exit() async {
    _process.stdin.writeln(jsonEncode({'cmd': 'exit'}));
    await _process.stdin.flush();
    return _process.exitCode.timeout(const Duration(seconds: 15),
        onTimeout: () {
      kill();
      return -1;
    });
  }

  void kill() => _process.kill(ProcessSignal.sigkill);
}
