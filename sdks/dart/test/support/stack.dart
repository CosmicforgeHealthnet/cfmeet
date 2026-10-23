/// Shared helpers of the real-stack parity suite (`test/stack_integration_test.dart`) and
/// the cross-SDK interop peer (`test/support/interop_peer.dart`): the environment file of
/// `cfmeet-cert-stack up --env-out <file>`, clients of its gateway, and Messaging MLS
/// device / group steps performed ONLY through the SDK's public API (operations through
/// the real API Gateway, cryptography through the FFI bridge).
library;

import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

/// One tenant of the stack environment.
class TenantEnv {
  TenantEnv(Map<String, dynamic> j)
      : tenantId = j['tenantId'] as String,
        applicationId = j['applicationId'] as String,
        environmentId = j['environmentId'] as String,
        serviceAccountId = j['serviceAccountId'] as String,
        organizationId = j['organizationId'] as String?,
        projectId = j['projectId'] as String?,
        principals = (j['principals'] as List).cast<String>(),
        sdkPrincipals = {
          for (final e in ((j['sdkPrincipals'] as Map?) ?? const {}).entries)
            e.key as String: (e.value as List).cast<String>()
        },
        limitedScopes =
            ((j['limited'] as Map<String, dynamic>?)?['scopes'] as List?)
                    ?.cast<String>() ??
                const [];

  final String tenantId;
  final String applicationId;
  final String environmentId;
  final String serviceAccountId;
  final String? organizationId;
  final String? projectId;
  final List<String> principals;

  /// Principals reserved per SDK suite (one per Messaging device role, [SdkPrincipal]).
  final Map<String, List<String>> sdkPrincipals;
  final List<String> limitedScopes;
}

/// The environment file of `cfmeet-cert-stack up`
/// (certification/src/bin/cfmeet-cert-stack.rs).
class StackEnv {
  StackEnv._(this.path, Map<String, dynamic> j)
      : gatewayUrl = j['gatewayUrl'] as String,
        credentials = (j['credentials'] as Map).cast<String, String>(),
        userSession = j['userSession'] as String,
        sandbox =
            TenantEnv((j['tenants'] as Map)['sandbox'] as Map<String, dynamic>),
        otherTenant = TenantEnv(
            (j['tenants'] as Map)['otherTenant'] as Map<String, dynamic>),
        sandboxRequestsPerMinute =
            (j['limits'] as Map)['sandboxRequestsPerMinute'] as int;

  factory StackEnv.read(String path) {
    final j = jsonDecode(File(path).readAsStringSync()) as Map<String, dynamic>;
    final creds = j['credentials'];
    for (final k in ['sandbox', 'otherTenant', 'sandboxLimited']) {
      if (creds is! Map || '${creds[k]}'.startsWith('cfmk_sbx_') == false) {
        throw StateError('stack env $path: credentials.$k missing '
            '(start the stack with cfmeet-cert-stack up --env-out <file>)');
      }
    }
    final env = StackEnv._(path, j);
    if (env.sandbox.principals.length < 2) {
      throw StateError('stack env $path: tenants.sandbox.principals (2)');
    }
    for (final sdk in ['typescript', 'dart']) {
      if ((env.sandbox.sdkPrincipals[sdk]?.length ?? 0) < 5) {
        throw StateError(
            'stack env $path: tenants.sandbox.sdkPrincipals.$sdk (5)');
      }
    }
    return env;
  }

  final String path;
  final String gatewayUrl;
  final Map<String, String> credentials;
  final String userSession;
  final TenantEnv sandbox;
  final TenantEnv otherTenant;
  final int sandboxRequestsPerMinute;

  /// A client of the gateway. Retryable classes honour a Retry-After up to the quota
  /// window.
  CfMeetClient client(CfMeetAuth? auth,
          {int maxRetries = 2,
          CfMeetTransport? transport,
          RetryOptions retryOptions = const RetryOptions(
              baseDelay: Duration(milliseconds: 100),
              maxRetryAfter: Duration(seconds: 61))}) =>
      CfMeetClient(
        baseUrl: gatewayUrl,
        auth: auth,
        maxRetries: maxRetries,
        retryOptions: retryOptions,
        transport: transport,
        timeout: const Duration(seconds: 20),
        userAgent: 'cfmeet-platform-sdk-dart/0.1.0 sdk-parity',
      );

  CfMeetClient credentialClient(String name, {int maxRetries = 2}) =>
      client(CfMeetAuth.platformCredential(credentials[name]!),
          maxRetries: maxRetries);
}

/// Waits for a fresh platform quota window when fewer than [needed] requests remain in
/// the current one (the TypeScript and Dart parity suites share the sandbox environment's
/// per-minute quota). Retryable operations wait out a 429 themselves (Retry-After); a
/// NEVER-class operation must not meet one.
Future<void> ensureQuotaHeadroom(CfMeetClient c, int needed) async {
  final q = (await c.gateway.call('getDeveloperPlatformContext')).quota;
  final remaining = q?.remainingMinute;
  if (remaining == null || remaining >= needed) return;
  await Future<void>.delayed(Duration(seconds: (q!.resetSeconds ?? 60) + 1));
}

/// Roles of `tenants.sandbox.sdkPrincipals.<sdk>` (Messaging limits device-registration
/// challenges per principal and hour: each role registering devices has its own
/// principal).
enum SdkPrincipal { e2eeOwner, e2eeMember, interopOwner, interopPeer, realtime }

// ---- encoding ----

String b64url(List<int> bytes) => base64Url.encode(bytes).replaceAll('=', '');
Uint8List unb64url(String s) => base64Url.decode(base64Url.normalize(s));
Uint8List hexBytes(String hex) => Uint8List.fromList([
      for (var i = 0; i < hex.length; i += 2)
        int.parse(hex.substring(i, i + 2), radix: 16)
    ]);

String uuid4() {
  final r = Random.secure();
  final b = List<int>.generate(16, (_) => r.nextInt(256));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  final h = b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();
  return '${h.substring(0, 8)}-${h.substring(8, 12)}-${h.substring(12, 16)}-'
      '${h.substring(16, 20)}-${h.substring(20)}';
}

// ---- Messaging devices and groups, through the SDK ----

Map<String, String> acting(String principalId) =>
    {'cf-acting-principal': principalId};

/// One principal's device (its keys stay in native memory).
class Member {
  Member(this.principalId, this.deviceId, this.device);
  final String principalId;
  final String deviceId;
  final MessagingDevice device;
}

/// Revokes every ACTIVE device of [principalId] (leftovers of earlier runs).
Future<int> revokeDevices(CfMeetClient c, String principalId) async {
  final dir = await c.messaging.call('listPrincipalDevices',
      pathParams: {'principalId': principalId}, headers: acting(principalId));
  var n = 0;
  for (final d in (dir.json['devices'] as List).cast<Map<String, dynamic>>()) {
    if (d['status'] != 'ACTIVE') continue;
    await c.messaging.call('revokeDevice',
        pathParams: {'deviceId': d['deviceId']},
        headers: acting(principalId),
        body: {'reason': 'REPLACED'});
    n++;
  }
  return n;
}

/// Registers a device (challenge -> registration KeyPackage from the bridge) and
/// publishes [packages] KeyPackages.
Future<Member> registerDevice(
    CfMeetClient c, MessagingCrypto crypto, String principalId, String label,
    {int packages = 1}) async {
  final ch = (await c.messaging.call('createDeviceRegistrationChallenge',
          headers: acting(principalId)))
      .json;
  final deviceId = ch['deviceId'] as String;
  final device = crypto.createDevice(deviceId);
  final kp = device.registrationKeyPackage(
      ch['challengeId'] as String, hexBytes(ch['nonce'] as String));
  final reg = (await c.messaging
          .call('registerDevice', headers: acting(principalId), body: {
    'challengeId': ch['challengeId'],
    'keyPackage': b64url(kp),
    'displayLabel': label,
  }))
      .json;
  if (reg['deviceId'] != deviceId || reg['status'] != 'ACTIVE') {
    throw StateError('device registration: $reg');
  }
  if (packages > 0) {
    await c.messaging.call('publishKeyPackages',
        pathParams: {'deviceId': deviceId},
        headers: acting(principalId),
        body: {
          'keyPackages': [
            for (var i = 0; i < packages; i++) b64url(device.keyPackage())
          ]
        });
  }
  return Member(principalId, deviceId, device);
}

/// The server-assigned MLS group id of a PRIVATE_E2EE conversation.
Future<Uint8List> groupIdOf(
    CfMeetClient c, String conversationId, String principalId) async {
  final conv = (await c.messaging.call('getConversation',
          pathParams: {'conversationId': conversationId},
          headers: acting(principalId)))
      .json;
  final gid = (conv['encryption'] as Map)['mlsGroupId'];
  if (gid is! String) throw StateError('no MLS group id: $conv');
  return unb64url(gid);
}

class Established {
  Established(this.groupId, this.commitSequence, this.welcomed);
  final Uint8List groupId;
  final int commitSequence;
  final List<String> welcomed;
}

/// The owner creates the MLS group, claims one KeyPackage of every device of [others]
/// (already members), commits their addition with a Welcome and submits it.
Future<Established> establishGroup(CfMeetClient c, String conversationId,
    Member owner, List<String> others) async {
  final groupId = await groupIdOf(c, conversationId, owner.principalId);
  owner.device.createGroup(groupId);
  final claim = (await c.messaging.call('claimKeyPackages',
          pathParams: {'conversationId': conversationId},
          headers: acting(owner.principalId),
          body: {'principalIds': others}))
      .json;
  final kps = (claim['keyPackages'] as List).cast<Map<String, dynamic>>();
  if (kps.isEmpty) throw StateError('no KeyPackage claimed');
  final out = owner.device.addDevices(
      groupId, [for (final k in kps) unb64url(k['keyPackage'] as String)]);
  final welcome = out.welcome;
  if (welcome == null) throw StateError('addDevices produced no Welcome');
  final welcomed = [for (final k in kps) k['deviceId'] as String];
  final r = (await c.messaging.call('submitMlsCommit',
          pathParams: {'conversationId': conversationId},
          headers: acting(owner.principalId),
          body: {
            'clientMessageId': uuid4(),
            'deviceId': owner.deviceId,
            'epoch': out.epoch,
            'commit': b64url(out.commit),
            'welcome': b64url(welcome),
            'welcomeDeviceIds': welcomed,
            'removedDeviceIds': const <String>[],
          }))
      .json;
  return Established(groupId, r['sequence'] as int, welcomed);
}

/// A welcomed device joins from its pending Welcome and consumes it.
Future<(Uint8List, int)> joinFromWelcome(
    CfMeetClient c, Member m, String conversationId) async {
  final page = (await c.messaging.call('listWelcomes',
          pathParams: {'deviceId': m.deviceId}, headers: acting(m.principalId)))
      .json;
  final welcome = (page['welcomes'] as List)
      .cast<Map<String, dynamic>>()
      .where((w) => w['conversationId'] == conversationId)
      .firstOrNull;
  if (welcome == null) {
    throw StateError('no Welcome for $conversationId: $page');
  }
  final groupId = await groupIdOf(c, conversationId, m.principalId);
  final epoch = m.device.join(groupId, unb64url(welcome['welcome'] as String));
  await c.messaging.call('consumeWelcome',
      pathParams: {'deviceId': m.deviceId, 'welcomeId': welcome['welcomeId']},
      headers: acting(m.principalId));
  return (groupId, epoch);
}

/// Encrypts [text] under the CF Meet AAD on the bridge and sends it. Returns the send
/// response and the ciphertext (base64url).
Future<(ApiResponse, String)> sendEncrypted(
    CfMeetClient c,
    MessagingCrypto crypto,
    TenantEnv tenant,
    String conversationId,
    Member m,
    Uint8List groupId,
    String text) async {
  final clientMessageId = uuid4();
  final aad = crypto.messageAad(
    tenantId: tenant.tenantId,
    applicationId: tenant.applicationId,
    environmentId: tenant.environmentId,
    conversationId: conversationId,
    clientMessageId: clientMessageId,
    senderDeviceId: m.deviceId,
  );
  final ciphertext = b64url(m.device.encrypt(groupId, aad, utf8.encode(text)));
  final sent = await c.messaging.call('sendMessage',
      pathParams: {'conversationId': conversationId},
      headers: acting(m.principalId),
      body: {
        'clientMessageId': clientMessageId,
        'deviceId': m.deviceId,
        'epoch': m.device.epoch(groupId),
        'ciphertext': ciphertext,
      });
  return (sent, ciphertext);
}

/// One decrypted application message.
class Decrypted {
  Decrypted(this.sequence, this.messageId, this.plaintext, this.senderDeviceId);
  final int sequence;
  final String messageId;
  final String plaintext;
  final String senderDeviceId;

  Map<String, Object> toJson() => {
        'sequence': sequence,
        'messageId': messageId,
        'plaintext': plaintext,
        'senderDeviceId': senderDeviceId,
      };
}

/// Fetches the messages after [afterSequence] and processes those of OTHER devices on
/// [m]'s device, verifying each one's exact CF Meet AAD.
Future<List<Decrypted>> receiveDecrypted(
    CfMeetClient c,
    MessagingCrypto crypto,
    TenantEnv tenant,
    String conversationId,
    Member m,
    Uint8List groupId,
    int afterSequence) async {
  final page = (await c.messaging.call('listMessages',
          pathParams: {'conversationId': conversationId},
          query: {'afterSequence': afterSequence},
          headers: acting(m.principalId)))
      .json;
  final out = <Decrypted>[];
  for (final msg in (page['messages'] as List).cast<Map<String, dynamic>>()) {
    final e2ee = msg['e2ee'] as Map<String, dynamic>?;
    if (msg['lifecycle'] != 'ACTIVE' ||
        e2ee?['ciphertext'] is! String ||
        msg['senderDeviceId'] == m.deviceId) {
      continue;
    }
    if ((e2ee!['epoch'] as int? ?? 0) < m.device.epoch(groupId)) continue;
    final aad = msg['kind'] == 'APPLICATION'
        ? crypto.messageAad(
            tenantId: tenant.tenantId,
            applicationId: tenant.applicationId,
            environmentId: tenant.environmentId,
            conversationId: conversationId,
            clientMessageId: msg['clientMessageId'] as String,
            senderDeviceId: msg['senderDeviceId'] as String,
          )
        : null;
    final r = m.device.process(groupId, unb64url(e2ee['ciphertext'] as String),
        expectedAad: aad);
    if (r.kind == ProcessedKind.application) {
      out.add(Decrypted(msg['sequence'] as int, msg['messageId'] as String,
          utf8.decode(r.plaintext), msg['senderDeviceId'] as String));
    }
  }
  return out;
}
