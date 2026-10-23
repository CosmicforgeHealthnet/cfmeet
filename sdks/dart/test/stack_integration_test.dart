/// Real-stack SDK parity certification (Part VI §52 / §20.2), gated on
/// `CFMEET_STACK_ENV`.
///
/// Start the stack and point `CFMEET_STACK_ENV` at the environment file it writes:
///
/// ```sh
/// make certification-stack ENV_OUT=/tmp/stack.json   # or: cfmeet-cert-stack up --env-out FILE
/// CFMEET_STACK_ENV=/tmp/stack.json dart test test/stack_integration_test.dart
/// ```
///
/// Every scenario of `sdks/testdata/parity-scenarios.json` is implemented here AND in the
/// TypeScript SDK's `test/integration.test.ts`; the first test (which runs without a stack
/// too) fails when this suite's set of scenario ids differs from the manifest's. Without
/// `CFMEET_STACK_ENV` the stack scenarios print SKIPPED and pass; with it, any failure
/// fails. The FFI library comes from `CFMEET_MESSAGING_CRYPTO_LIB` (or is built with
/// cargo); the cross-SDK scenario runs the TypeScript SDK's interop peer
/// (`node sdks/typescript/build-test/test/support/interop-peer.js`, built by
/// `npm test` / `tsc -p tsconfig.test.json`).
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

import 'support/native_library.dart';
import 'support/peer_process.dart';
import 'support/stack.dart';

/// Counts the requests the SDK hands to the real transport.
class CountingTransport implements CfMeetTransport {
  CountingTransport(this._inner);
  final CfMeetTransport _inner;
  final List<TransportRequest> sent = [];

  @override
  Future<TransportResponse> send(TransportRequest request) {
    sent.add(request);
    return _inner.send(request);
  }

  @override
  void close({bool force = false}) => _inner.close(force: force);
}

/// Holds the SECOND meeting realtime ticket request until [gate] completes.
class GatedTicketTransport implements CfMeetTransport {
  GatedTicketTransport(this._inner);
  final CfMeetTransport _inner;
  final Completer<void> gate = Completer<void>();
  final Completer<void> secondTicketRequested = Completer<void>();
  int mints = 0;

  @override
  Future<TransportResponse> send(TransportRequest request) async {
    if (request.method == 'POST' &&
        request.url.path.endsWith('/realtime/ticket') &&
        ++mints == 2) {
      secondTicketRequested.complete();
      await gate.future;
    }
    return _inner.send(request);
  }

  @override
  void close({bool force = false}) => _inner.close(force: force);
}

Future<Object> refusal(Future<Object?> f) async {
  try {
    await f;
  } catch (e) {
    return e;
  }
  fail('expected the call to be refused');
}

void main() {
  final manifest =
      jsonDecode(File('../testdata/parity-scenarios.json').readAsStringSync())
          as Map<String, dynamic>;
  final ids = [
    for (final s
        in (manifest['scenarios'] as List).cast<Map<String, dynamic>>())
      s['id'] as String
  ];
  final envFile = Platform.environment['CFMEET_STACK_ENV'];
  late final StackEnv env = StackEnv.read(envFile!);
  late final MessagingCrypto crypto =
      MessagingCrypto.open(path: nativeLibraryPath());
  CfMeetClient sandbox() => env.credentialClient('sandbox');
  // This suite's principal for a Messaging device role.
  String mine(SdkPrincipal role) =>
      env.sandbox.sdkPrincipals['dart']![role.index];
  CfMeetClient session() =>
      env.client(CfMeetAuth.developerSession(env.userSession));
  final repo =
      Directory.current.parent.parent.path; // sdks/dart -> repository root

  Future<String> e2eeConversation(
      CfMeetClient c, String owner, List<String> members) async {
    final conv = await c.messaging.call('createConversation',
        headers: acting(owner),
        body: {'contextClass': 'GROUP', 'securityProfile': 'PRIVATE_E2EE'});
    expect(conv.status, 201);
    expect(conv.json['securityProfile'], 'PRIVATE_E2EE');
    final enc = conv.json['encryption'] as Map<String, dynamic>;
    expect(enc['serverCanReadContent'], false,
        reason: 'the server cannot read E2EE content');
    expect(enc['mlsGroupId'], matches(RegExp(r'^[A-Za-z0-9_-]{43}$')));
    final cid = conv.json['conversationId'] as String;
    for (final p in members) {
      final m = await c.messaging.call('addMember',
          pathParams: {'conversationId': cid},
          headers: acting(owner),
          body: {'principalId': p});
      expect(m.status, anyOf(200, 201));
    }
    return cid;
  }

  final scenarios = <String, Future<void> Function()>{
    'discovery': () async {
      final r = await CfMeetClient(baseUrl: env.gatewayUrl).discovery();
      expect(r.status, 200);
      expect(r.json['service'], 'cfmeet.api-gateway');
      expect(r.json['families'], isA<List<dynamic>>());
      expect(r.requestId, isNotNull);
    },
    'context.credential': () async {
      final r = await sandbox().gateway.call('getDeveloperPlatformContext');
      expect(r.status, 200);
      expect(r.json['tenantId'], env.sandbox.tenantId);
      expect(r.json['applicationId'], env.sandbox.applicationId);
      expect(r.json['environmentId'], env.sandbox.environmentId);
      expect(r.json['serviceAccountId'], env.sandbox.serviceAccountId);
      expect(r.json['authority'], 'PLATFORM_ACCESS');
      expect(r.json['forwardedBy'], 'cfmeet.api-gateway');
      expect(r.requestId, isNotNull);
      expect(r.correlationId, isNotNull);
    },
    'errors.defaultDeny': () async {
      final unrouted = OperationSpec(
        domain: 'gateway',
        name: 'unrouted',
        operationId: 'unrouted',
        method: 'GET',
        path: '/v1/sdk-certification-unrouted-${uuid4().substring(0, 8)}',
        protocol: 'REST',
        auth: const ['PLATFORM_CREDENTIAL'],
        scope: null,
        pathParams: const [],
        query: const [],
        headers: const [],
        body: 'NONE',
        idempotency: 'NONE',
        retry: RetryClass.safe,
        paidTelecom: false,
        pagination: null,
        success: const [200],
      );
      final e = await refusal(sandbox().request(unrouted));
      expect(
          e,
          isA<CfMeetNotFoundError>()
              .having((e) => e.status, 'status', 404)
              .having((e) => e.code, 'code', 'NOT_FOUND')
              .having((e) => e.requestId, 'requestId', isNotNull));
    },
    'errors.invalidCredential': () async {
      final forged = 'cfmk_sbx_${'0' * 24}_${'0' * 64}';
      final e = await refusal(env
          .client(CfMeetAuth.platformCredential(forged), maxRetries: 0)
          .gateway
          .call('getDeveloperPlatformContext'));
      expect(
          e,
          isA<CfMeetAuthenticationError>()
              .having((e) => e.status, 'status', 401)
              .having((e) => e.code, 'code', 'AUTHENTICATION_FAILED')
              .having((e) => e.requestId, 'requestId', isNotNull)
              .having((e) => e.toString(), 'toString', isNot(contains(forged)))
              .having(
                  (e) => jsonEncode(e.body), 'body', isNot(contains(forged))));
    },
    'errors.missingScope': () async {
      expect(env.sandbox.limitedScopes, isNot(contains('calls.read')));
      final c = env.credentialClient('sandboxLimited');
      final e = await refusal(c.calling.call('listCalls'));
      expect(
          e,
          isA<CfMeetPermissionError>()
              .having((e) => e.status, 'status', 403)
              .having((e) => e.code, 'code', 'INSUFFICIENT_SCOPE')
              .having((e) => e.requestId, 'requestId', isNotNull));
      // The credential itself is valid: a route within its scopes answers.
      expect((await c.developerPlatform.call('listOrganizations')).status, 200);
    },
    'isolation.otherTenant': () async {
      final t = env.sandbox;
      final own = await sandbox().developerPlatform.call('getOrganization',
          pathParams: {'organizationId': t.organizationId});
      expect(own.status, 200);
      expect(own.json['id'], t.organizationId);
      final other = env.credentialClient('otherTenant');
      for (final call in [
        () => other.developerPlatform.call('getOrganization',
            pathParams: {'organizationId': t.organizationId}),
        () => other.developerPlatform
            .call('getProject', pathParams: {'projectId': t.projectId}),
        () => other.developerPlatform.call('getEnvironment',
            pathParams: {'environmentId': t.environmentId}),
      ]) {
        final e = await refusal(call());
        expect(e,
            isA<CfMeetNotFoundError>().having((e) => e.status, 'status', 404));
        final leaked = jsonEncode((e as CfMeetApiError).body);
        for (final id in [t.organizationId!, t.projectId!, t.environmentId]) {
          expect(leaked, isNot(contains(id)), reason: 'leak: $leaked');
        }
      }
    },
    'developerPlatform.session.list': () async {
      final t = env.sandbox;
      final c = session();
      final orgs = await c.developerPlatform.call('listOrganizations');
      expect(orgs.status, 200);
      expect((orgs.json['items'] as List).map((o) => (o as Map)['id']),
          contains(t.organizationId));
      final projects = await c.developerPlatform.call('listProjects',
          pathParams: {'organizationId': t.organizationId});
      expect(projects.status, 200);
      expect((projects.json['items'] as List).map((p) => (p as Map)['id']),
          contains(t.projectId));
    },
    'developerPlatform.session.idempotentCreate': () async {
      final t = env.sandbox;
      final c = session();
      final key = 'sdk-parity-dart-${uuid4()}';
      Future<ApiResponse> create(String name) =>
          c.developerPlatform.call('createProject',
              pathParams: {'organizationId': t.organizationId},
              body: {'displayName': name},
              idempotencyKey: key);
      final first = await create('SDK parity (Dart)');
      expect(first.status, 201);
      expect(first.idempotencyKey, key);
      expect(first.idempotentReplayed, false);
      final replay = await create('SDK parity (Dart)');
      expect(replay.status, 201);
      expect(replay.json['id'], first.json['id'],
          reason: 'the replay returns the same project');
      // The Developer Platform replays without an Idempotent-Replayed header today: the
      // SDK reports exactly what the gateway sent.
      expect(
          replay.idempotentReplayed,
          replay.headers['idempotent-replayed']?.trim().toLowerCase() ==
              'true');
      final e = await refusal(create('SDK parity (Dart), other body'));
      expect(
          e,
          isA<CfMeetValidationError>()
              .having((e) => e.status, 'status', 422)
              .having((e) => e.code, 'code', 'IDEMPOTENCY_KEY_REUSED'));
    },
    'idempotency.replayHeader': () async {
      final t = env.sandbox;
      final c = sandbox();
      final key = 'sdk-parity-dart-${uuid4()}';
      final reference = 'sdk-parity-dart-${uuid4().substring(0, 8)}';
      final expiresAt =
          DateTime.now().toUtc().add(const Duration(days: 1)).toIso8601String();
      Map<String, Object?> body(String ref) => {
            'externalContextReference': ref,
            'participants': [
              for (final p in t.principals.take(2)) {'principalId': p}
            ],
            'allowedChannels': ['VOICE'],
            'directionPolicy': 'BIDIRECTIONAL',
            'expiresAt': expiresAt,
          };
      final first = await c.privacyRelay
          .call('createContext', body: body(reference), idempotencyKey: key);
      expect(first.status, 201);
      expect(first.idempotencyKey, key);
      expect(first.idempotentReplayed, false);
      final replay = await c.privacyRelay
          .call('createContext', body: body(reference), idempotencyKey: key);
      expect(replay.status, 201);
      expect(replay.idempotentReplayed, true,
          reason: 'Idempotent-Replayed: true is surfaced');
      expect((replay.json['context'] as Map)['contextId'],
          (first.json['context'] as Map)['contextId']);
      final e = await refusal(c.privacyRelay.call('createContext',
          body: body('$reference-x'), idempotencyKey: key));
      expect(
          e,
          isA<CfMeetConflictError>()
              .having((e) => e.status, 'status', 409)
              .having((e) => e.code, 'code', 'IDEMPOTENCY_CONFLICT'));
    },
    'pagination.cursor': () async {
      final t = env.sandbox;
      final c = session();
      final created = <String>[];
      for (final n in [1, 2]) {
        final r = await c.developerPlatform.call('createProject',
            pathParams: {'organizationId': t.organizationId},
            body: {'displayName': 'SDK parity page $n (Dart)'});
        created.add(r.json['id'] as String);
      }
      final op = c.developerPlatform.operation('listProjects');
      var pages = 0;
      await for (final page in c.pages(op.spec,
          pathParams: {'organizationId': t.organizationId},
          query: {'limit': 1})) {
        pages++;
        expect(page.status, 200);
        expect((page.json['items'] as List).length, lessThanOrEqualTo(1));
      }
      final items = await c.developerPlatform
          .paginate('listProjects',
              pathParams: {'organizationId': t.organizationId},
              query: {'limit': 1})
          .map((p) => p['id'] as String)
          .toList();
      expect(items.length, greaterThanOrEqualTo(3));
      expect(pages, greaterThanOrEqualTo(items.length));
      expect(items.toSet().length, items.length, reason: 'no item twice');
      expect(items, containsAll([t.projectId, ...created]));
    },
    'quota.headers': () async {
      final r = await sandbox().gateway.call('getDeveloperPlatformContext');
      final q = r.quota;
      expect(q, isNotNull, reason: 'x-cf-quota-* headers are surfaced');
      expect(q!.limitMinute, env.sandboxRequestsPerMinute);
      expect(q.remainingMinute, isA<int>());
      expect(q.remainingMinute!, lessThan(q.limitMinute!),
          reason: 'this request was counted');
      expect(q.resetSeconds, isA<int>());
      expect(q.limitDay, isA<int>());
    },
    'retry.neverPaidTelecom': () async {
      final [alice, bob, ...] = env.sandbox.principals;
      final counting = CountingTransport(IoTransport());
      // Retries are ON for this client (5 after the first attempt, short backoff).
      final c = env.client(
          CfMeetAuth.platformCredential(env.credentials['sandbox']!),
          maxRetries: 5,
          transport: counting,
          retryOptions: const RetryOptions(
              baseDelay: Duration(milliseconds: 10),
              maxDelay: Duration(milliseconds: 50)));
      // A 429 here would not be the outcome under test.
      await ensureQuotaHeadroom(c, 10);
      final spec = c.calling.operation('createCall').spec;
      expect(spec.retry, RetryClass.never);
      expect(spec.paidTelecom, isTrue);
      String? endpointId;
      for (var i = 0; endpointId == null; i++) {
        try {
          final ep = await c.calling.call('registerEndpoint', body: {
            'principalId': alice,
            'endpointKey': 'sdk-parity-dart-${uuid4()}',
            'endpointKind': 'MOBILE',
            'capabilities': {'audio': true, 'video': true},
          });
          endpointId = ep.json['endpointId'] as String;
        } on CfMeetApiError {
          // Calling learns principals from the identity feed (eventually).
          if (i >= 50) rethrow;
          await Future<void>.delayed(const Duration(milliseconds: 200));
        }
      }
      bool isCreateCall(TransportRequest r) =>
          r.method == 'POST' && r.url.path == '/v1/calls';
      final e = await refusal(c.calling.call('createCall', body: {
        'kind': 'VOICE',
        'initiatorPrincipalId': alice,
        'initiatorEndpointId': endpointId,
        'targetPrincipalIds': [bob],
      }));
      expect(
          e,
          isA<CfMeetValidationError>()
              .having((e) => e.status, 'status', 422)
              .having((e) => e.code, 'code', 'CAPABILITY_UNAVAILABLE'));
      final sent = counting.sent.where(isCreateCall).toList();
      expect(sent, hasLength(1),
          reason: 'exactly one request for a NEVER-class operation');
      expect(sent.single.headers['idempotency-key'],
          matches(RegExp(r'^[0-9a-f-]{36}$')),
          reason: 'an Idempotency-Key was generated and sent');
    },
    'messaging.e2ee': () async {
      final t = env.sandbox;
      final alicePrincipal = mine(SdkPrincipal.e2eeOwner);
      final bobPrincipal = mine(SdkPrincipal.e2eeMember);
      final c = sandbox();
      await revokeDevices(c, alicePrincipal);
      await revokeDevices(c, bobPrincipal);
      final alice = await registerDevice(
          c, crypto, alicePrincipal, 'sdk-parity dart A',
          packages: 0);
      final bob =
          await registerDevice(c, crypto, bobPrincipal, 'sdk-parity dart B');
      final cid = await e2eeConversation(c, alicePrincipal, [bobPrincipal]);
      final g = await establishGroup(c, cid, alice, [bobPrincipal]);
      expect(g.welcomed, [bob.deviceId]);
      final (_, epoch) = await joinFromWelcome(c, bob, cid);
      expect(epoch, alice.device.epoch(g.groupId), reason: 'same epoch');

      final secret = 'parity secret from dart ${uuid4()}';
      final (sent, ciphertext) =
          await sendEncrypted(c, crypto, t, cid, alice, g.groupId, secret);
      expect(sent.status, 201);
      expect(sent.json['kind'], 'APPLICATION');
      expect(sent.json['securityProfile'], 'PRIVATE_E2EE');
      expect((sent.json['e2ee'] as Map)['ciphertext'], ciphertext,
          reason: 'the server stores the bridge ciphertext as is');
      expect(sent.json['text'], isNull, reason: 'no plaintext field');
      expect(jsonEncode(sent.json), isNot(contains('parity secret')));

      final listed = await c.messaging.call('listMessages',
          pathParams: {'conversationId': cid}, headers: acting(bobPrincipal));
      final stored = (listed.json['messages'] as List)
          .cast<Map<String, dynamic>>()
          .firstWhere((m) => m['messageId'] == sent.json['messageId']);
      expect((stored['e2ee'] as Map)['ciphertext'], ciphertext,
          reason: 'stored body is the ciphertext');
      expect(jsonEncode(listed.json), isNot(contains('parity secret')));

      final got = await receiveDecrypted(
          c, crypto, t, cid, bob, g.groupId, g.commitSequence);
      expect([
        for (final m in got) (m.plaintext, m.senderDeviceId)
      ], [
        (secret, alice.deviceId)
      ], reason: 'decrypted on the receiving device');

      // Plaintext is refused on an E2EE conversation.
      final e = await refusal(c.messaging.call('sendMessage',
          pathParams: {'conversationId': cid},
          headers: acting(alicePrincipal),
          body: {'clientMessageId': uuid4(), 'text': secret}));
      expect(
          e,
          isA<CfMeetValidationError>().having((e) => e.status, 'status',
              allOf(greaterThanOrEqualTo(400), lessThan(500))));
    },
    'messaging.e2ee.crossSdk': () async {
      final t = env.sandbox;
      final alicePrincipal = mine(SdkPrincipal.interopOwner);
      final bobPrincipal = mine(SdkPrincipal.interopPeer);
      final c = sandbox();
      await revokeDevices(c, alicePrincipal);
      await revokeDevices(c, bobPrincipal);
      // Bob's device is run by the TypeScript SDK (the wasm crypto core) in its own
      // process.
      final peerJs =
          '$repo/sdks/typescript/build-test/test/support/interop-peer.js';
      if (!File(peerJs).existsSync()) {
        fail('$peerJs is missing: build the TypeScript SDK tests first '
            '(cd sdks/typescript && npm ci && npx tsc -p tsconfig.test.json)');
      }
      final peer =
          await PeerProcess.start('node', [peerJs, env.path, bobPrincipal]);
      try {
        expect(peer.sdk, 'typescript');
        final alice = await registerDevice(
            c, crypto, alicePrincipal, 'sdk-parity dart (interop owner)',
            packages: 0);
        final cid = await e2eeConversation(c, alicePrincipal, [bobPrincipal]);
        final g = await establishGroup(c, cid, alice, [bobPrincipal]);
        expect(g.welcomed, [peer.deviceId],
            reason: "the TypeScript device's KeyPackage was claimed");
        final joined =
            await peer.request({'cmd': 'join', 'conversationId': cid});
        expect(joined['epoch'], alice.device.epoch(g.groupId));

        final toTs = 'dart -> typescript ${uuid4()}';
        final (sent, _) =
            await sendEncrypted(c, crypto, t, cid, alice, g.groupId, toTs);
        final atPeer = await peer.request({
          'cmd': 'receive',
          'conversationId': cid,
          'afterSequence': g.commitSequence
        });
        expect([
          for (final m
              in (atPeer['messages'] as List).cast<Map<String, dynamic>>())
            (m['plaintext'], m['senderDeviceId'])
        ], [
          (toTs, alice.deviceId)
        ], reason: 'the TypeScript SDK decrypted the Dart message');

        final toDart = 'typescript -> dart ${uuid4()}';
        final reply = await peer
            .request({'cmd': 'send', 'conversationId': cid, 'text': toDart});
        expect(reply['ciphertext'], isA<String>());
        final got = await receiveDecrypted(
            c, crypto, t, cid, alice, g.groupId, sent.json['sequence'] as int);
        expect([
          for (final m in got) (m.plaintext, m.senderDeviceId)
        ], [
          (toDart, peer.deviceId)
        ], reason: 'the Dart SDK decrypted the TypeScript message');
        expect(await peer.exit(), 0);
      } finally {
        peer.kill();
      }
    },
    'realtime.messaging': () async {
      final principal = mine(SdkPrincipal.realtime);
      final c = sandbox();
      await revokeDevices(c, principal);
      final device = await registerDevice(
          c, crypto, principal, 'sdk-parity dart realtime',
          packages: 0);
      final token = await c.messaging.call('issueClientToken',
          headers: acting(principal), body: {'deviceId': device.deviceId});
      expect(token.json['tokenType'], 'CF-Client-Token');
      final conn = RealtimeClient(c).messaging(
          auth: CfMeetAuth.clientToken(token.json['token'] as String));
      final frames = StreamIterator(
          conn.messages.where((m) => m is Map).cast<Map<dynamic, dynamic>>());
      Future<Map<dynamic, dynamic>> next(String type) async {
        while (await frames.moveNext().timeout(const Duration(seconds: 15))) {
          if (frames.current['type'] == type) return frames.current;
        }
        throw StateError('closed before a $type frame');
      }

      final welcome = await next('welcome');
      expect(welcome['protocolVersion'], 1);
      expect(conn.state, RealtimeState.open);
      expect(conn.connections, 1);
      conn.send({'type': 'ping', 'requestId': 'parity-1'});
      final pong = await next('pong');
      expect(pong['requestId'], 'parity-1');
      final closed = await conn.close();
      expect(closed.byClient, isTrue);
      expect(conn.state, RealtimeState.closed);
      expect(conn.connections, 1, reason: 'no reconnect');
      await frames.cancel();
    },
    'realtime.meetings': () async {
      final c = session();
      final meetingSession = CfMeetAuth.meetingSession(env.userSession);
      final meeting = await c.meetings.call('v2CreateInstantMeeting',
          body: {'title': 'SDK parity (Dart)'}, auth: meetingSession);
      expect(meeting.status, anyOf(200, 201));
      final m = meeting.json;
      final conferenceId =
          (m['conference'] as Map?)?['id'] ?? m['conferenceId'] ?? m['id'];
      expect(conferenceId, isA<String>(), reason: '$m');
      final conn = RealtimeClient(c)
          .meetings(conferenceId as String, auth: meetingSession);
      final subscribed = await conn.messages
          .where((f) => f is Map && f['type'] == 'subscribed')
          .first
          .timeout(const Duration(seconds: 15));
      expect((subscribed as Map)['scope'], 'FULL');
      final closed = await conn.close();
      expect(closed.byClient, isTrue);
      expect(conn.connections, 1, reason: 'no reconnect');
    },
    'realtime.meetings.resume': () async {
      // The helper's SECOND ticket request is held until the test opens the gate, so an
      // event can be committed while it is disconnected (and before that ticket exists).
      final gated = GatedTicketTransport(IoTransport());
      final c = env.client(CfMeetAuth.developerSession(env.userSession),
          transport: gated);
      final meetingSession = CfMeetAuth.meetingSession(env.userSession);
      final meeting = await c.meetings.call('v2CreateInstantMeeting',
          body: {'title': 'SDK parity resume (Dart)'}, auth: meetingSession);
      final id = ((meeting.json['conference'] as Map?)?['id'] ??
          meeting.json['id']) as String;
      final sockets = <RealtimeSocket>[];
      final urls = <Uri>[];
      final io = ioRealtimeConnector();
      Future<RealtimeSocket> recording(RealtimeHandshake h) async {
        urls.add(h.url);
        final s = await io(h);
        sockets.add(s);
        return s;
      }

      final conn = RealtimeClient(c,
              connector: recording,
              options: const ReconnectOptions(
                  baseDelay: Duration(milliseconds: 50),
                  maxDelay: Duration(milliseconds: 200),
                  maxConsecutiveFailures: 5))
          .meetings(id, auth: meetingSession);
      final frames = <Map<dynamic, dynamic>>[];
      final sub = conn.messages.listen((f) {
        if (f is Map) frames.add(f);
      });
      Future<(Map<dynamic, dynamic>, int)> waitFor(
          bool Function(Map<dynamic, dynamic>) pred, int from, String what,
          {Duration timeout = const Duration(seconds: 30)}) async {
        final deadline = DateTime.now().add(timeout);
        while (true) {
          for (var i = from; i < frames.length; i++) {
            if (pred(frames[i])) return (frames[i], i);
          }
          if (DateTime.now().isAfter(deadline) ||
              conn.state == RealtimeState.closed) {
            fail('no $what frame: $frames');
          }
          await Future<void>.delayed(const Duration(milliseconds: 50));
        }
      }

      bool isEvent(Map<dynamic, dynamic> f, String type) =>
          f['type'] == 'event' && (f['event'] as Map?)?['eventType'] == type;

      await waitFor((f) => f['type'] == 'subscribed', 0, 'subscribed');
      await c.meetings.call('v2LockConference',
          pathParams: {'id': id}, auth: meetingSession);
      final (locked, at) = await waitFor(
          (f) => isEvent(f, 'conference.locked'), 0, 'conference.locked');
      final n = (locked['event'] as Map)['sequence'] as int;
      await sockets.last.close(3000, 'parity: simulated drop');
      // The helper reconnects by itself and asks for a fresh ticket.
      await gated.secondTicketRequested.future
          .timeout(const Duration(seconds: 15));
      await c.meetings.call('v2UnlockConference',
          pathParams: {'id': id}, auth: meetingSession);
      gated.gate.complete();
      final (resubscribed, at2) = await waitFor(
          (f) => f['type'] == 'subscribed', at + 1, 'second subscribed');
      expect(urls, hasLength(2), reason: 'one reconnect');
      expect(urls[1].queryParameters['cursor'], '$n',
          reason: 'resumes after the last delivered event');
      expect(resubscribed['cursor'], n);
      final (unlocked, _) = await waitFor(
          (f) => isEvent(f, 'conference.unlocked'),
          at2 + 1,
          'replayed conference.unlocked');
      expect((unlocked['event'] as Map)['sequence'], n + 1);
      expect(gated.mints, 2, reason: 'a fresh ticket per connection');
      expect(conn.connections, 2);
      final closed = await conn.close();
      expect(closed.byClient, isTrue);
      await sub.cancel();
    },
  };

  test(
      'parity: implements exactly the scenarios of '
      'sdks/testdata/parity-scenarios.json', () {
    expect(ids.toSet().length, ids.length, reason: 'manifest ids are unique');
    expect(scenarios.keys.toSet(), ids.toSet());
  });

  if (envFile == null || envFile.isEmpty) {
    test('real-stack parity', () {
      // ignore: avoid_print
      print('SKIPPED: real-stack parity (CFMEET_STACK_ENV is not set)');
    });
    return;
  }

  group('real stack parity (CFMEET_STACK_ENV)', () {
    for (final s
        in (manifest['scenarios'] as List).cast<Map<String, dynamic>>()) {
      final id = s['id'] as String;
      test('$id: ${s['description']}', () async {
        final run = scenarios[id];
        if (run == null) fail('scenario $id is not implemented');
        await run();
      }, timeout: const Timeout(Duration(minutes: 4)));
    }
  });
}
