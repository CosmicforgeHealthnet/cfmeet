/// Cross-SDK interop peer (parity scenario `messaging.e2ee.crossSdk`): ONE Messaging
/// device run by the Dart SDK (the FFI crypto core, every request through the SDK's
/// operations), driven over JSON lines by another SDK's suite.
///
///   `dart run test/support/interop_peer.dart STACK_ENV_JSON PRINCIPAL_ID`
///
/// stdout, first line: `{"event":"ready","sdk":"dart","deviceId":...}` once the device is
/// registered and has published one KeyPackage. Then one JSON answer per stdin command:
///   {"cmd":"join","conversationId"}                    -> {"ok":true,"epoch"}
///   {"cmd":"receive","conversationId","afterSequence"} -> {"ok":true,"messages":[{sequence,messageId,plaintext,senderDeviceId}]}
///   {"cmd":"send","conversationId","text"}             -> {"ok":true,"messageId","sequence","ciphertext"}
///   {"cmd":"exit"}                                     -> exits 0
/// Failures answer `{"ok":false,"error":...}`. The TypeScript peer
/// (sdks/typescript/test/support/interop-peer.ts) speaks the same protocol.
library;

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

import 'native_library.dart';
import 'stack.dart';

void say(Object o) => stdout.writeln(jsonEncode(o));

Future<void> main(List<String> args) async {
  if (args.length != 2) {
    stderr.writeln('usage: interop_peer.dart <stack-env.json> <principalId>');
    exit(2);
  }
  try {
    final env = StackEnv.read(args[0]);
    final c = env.credentialClient('sandbox');
    final crypto = MessagingCrypto.open(path: nativeLibraryPath());
    final me = await registerDevice(
        c, crypto, args[1], 'sdk-parity interop peer (dart)');
    final groups = <String, Uint8List>{};
    say({'event': 'ready', 'sdk': 'dart', 'deviceId': me.deviceId});
    await for (final line
        in stdin.transform(utf8.decoder).transform(const LineSplitter())) {
      if (line.trim().isEmpty) continue;
      try {
        final cmd = jsonDecode(line) as Map<String, dynamic>;
        final cid = cmd['conversationId'] as String? ?? '';
        switch (cmd['cmd']) {
          case 'exit':
            await stdout.flush();
            c.close();
            exit(0);
          case 'join':
            final (groupId, epoch) = await joinFromWelcome(c, me, cid);
            groups[cid] = groupId;
            say({'ok': true, 'epoch': epoch});
          case 'receive':
            final groupId = groups[cid] ?? (throw StateError('not joined'));
            final got = await receiveDecrypted(c, crypto, env.sandbox, cid, me,
                groupId, cmd['afterSequence'] as int? ?? 0);
            say({
              'ok': true,
              'messages': [for (final m in got) m.toJson()]
            });
          case 'send':
            final groupId = groups[cid] ?? (throw StateError('not joined'));
            final (sent, ciphertext) = await sendEncrypted(c, crypto,
                env.sandbox, cid, me, groupId, cmd['text'] as String? ?? '');
            say({
              'ok': true,
              'messageId': sent.json['messageId'],
              'sequence': sent.json['sequence'],
              'ciphertext': ciphertext,
            });
          default:
            throw StateError('unknown command ${cmd['cmd']}');
        }
      } catch (e) {
        say({'ok': false, 'error': '$e'});
      }
    }
    exit(0);
  } catch (e) {
    say({'event': 'failed', 'error': '$e'});
    exit(1);
  }
}
