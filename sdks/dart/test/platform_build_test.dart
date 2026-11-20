// CF Meet Platform "Build" (ADR 0062 / 0063): the userToken auth (cache, refresh before
// expiry, one retry after a refusal), the server-side users helpers and the device
// clients (chat, calls, meetings, push, installation id) against the test fakes.
import 'dart:async';
import 'dart:convert';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:cfmeet_platform_sdk/testing.dart';
import 'package:test/test.dart';

const String base = 'https://api.cfmeet.test';
final String credential = 'cfmk_sbx_${'a' * 24}_${'b' * 64}';
const String principal = '66666666-7777-4888-9999-aaaaaaaaaaaa';
const String endpoint = 'f6f6f6f6-0707-4818-9929-3a3a3a3a3a3a';
const String sessionDevice = '0d0d0d0d-1111-4222-8333-444444444444';
const String conversation = '11111111-2222-4333-8444-555555555555';
const String conference = '22222222-3333-4444-8555-666666666666';
const String installation = 'inst-test-0001';
final DateTime t0 = DateTime.utc(2026, 11, 18, 10);

String _b64(Object json) =>
    base64Url.encode(utf8.encode(jsonEncode(json))).replaceAll('=', '');

/// A structurally valid (unsigned) user token expiring at [exp].
String userToken(int n, DateTime exp) {
  final header = _b64(
      {'alg': 'EdDSA', 'typ': 'cfmeet-user+jwt', 'kid': '0123456789abcdef'});
  final payload = _b64({
    'sub': principal,
    'jti': 'jti-$n',
    'exp': exp.millisecondsSinceEpoch ~/ 1000,
    'grt': {
      'msg': ['read', 'send'],
      'call': ['receive'],
    },
  });
  return 'cfut_sbx_$header.$payload.c2lnbmF0dXJl$n';
}

class Rig {
  Rig({String? installationId = installation, KeyValueStorage? storage}) {
    device = CfMeetDeviceClient(
      baseUrl: base,
      transport: fake,
      retryOptions: clock.retryOptions(),
      tokenProvider: () {
        final t =
            userToken(tokens.length + 1, now.add(const Duration(hours: 1)));
        tokens.add(t);
        return t;
      },
      installationId: installationId,
      storage: storage,
      connector: sockets.connector,
      reconnectOptions: clock.reconnectOptions(),
      autoRenew: false,
      now: () => now,
    );
  }

  final FakeCfMeetTransport fake = FakeCfMeetTransport();
  final FakeClock clock = FakeClock();
  final FakeRealtimeConnector sockets = FakeRealtimeConnector();
  final List<String> tokens = [];
  DateTime now = t0;
  late final CfMeetDeviceClient device;
}

Map<String, Object?> messagingSession(int n, DateTime now,
        [Duration ttl = const Duration(minutes: 10)]) =>
    {
      'principalId': principal,
      'sessionDeviceId': sessionDevice,
      'sessionDeviceExpiresAt':
          now.add(const Duration(days: 1)).toIso8601String(),
      'clientToken': 'client-token-$n',
      'clientTokenExpiresAt': now.add(ttl).toIso8601String(),
    };

Map<String, Object?> callingSession(int n, DateTime now,
        [Duration ttl = const Duration(minutes: 10)]) =>
    {
      'principalId': principal,
      'endpointId': endpoint,
      'signalingCredential': 'cfsig_test$n',
      'signalingCredentialExpiresAt': now.add(ttl).toIso8601String(),
    };

final String ticket = 'a' * 64;
FakeResponse ticketReply() => FakeResponse.json({
      'ticket': ticket,
      'expiresAt': t0.add(const Duration(seconds: 30)).toIso8601String(),
      'path': '/v1/realtime',
      'subprotocol': 'cfmeet.messaging.v1',
      'ticketSubprotocolPrefix': 'cfmeet.ticket.',
    }, status: 201);

const FakeResponse pushOk = FakeResponse.json(
    {'installationId': installation, 'platform': 'FCM', 'status': 'ACTIVE'});

void main() {
  group('user token', () {
    test('claims are decoded without verification', () {
      final t = userToken(1, t0.add(const Duration(hours: 1)));
      expect(isUserToken(t), isTrue);
      expect(decodeUserTokenClaims(t)!.subject, principal);
      expect(decodeUserTokenClaims(t)!.expiresAt,
          t0.add(const Duration(hours: 1)));
      expect(decodeUserTokenClaims('cfut_sbx_not-a-jws'), isNull);
      expect(decodeUserTokenClaims(credential), isNull);
    });

    test(
        'the cache asks once, again ~60 s before exp, and shares concurrent calls',
        () async {
      var now = t0;
      var calls = 0;
      final cache = UserTokenCache(() async {
        calls++;
        return userToken(calls, now.add(const Duration(minutes: 10)));
      }, now: () => now);
      final both = await Future.wait([cache.get(), cache.get()]);
      expect(calls, 1);
      expect(both[0], both[1]);
      now = now.add(const Duration(seconds: 539));
      expect(await cache.get(), both[0]);
      now = now.add(const Duration(seconds: 2));
      final c = await cache.get();
      expect(calls, 2);
      cache.invalidate(both[0]); // older: kept
      expect(await cache.get(), c);
      cache.invalidate();
      await cache.get();
      expect(calls, 3);
    });

    test('requests carry Bearer cfut_...; reused until it nears expiry',
        () async {
      final r = Rig();
      r.fake.onRequest('PUT',
          '/v1/messaging/user-push-registrations/{installationId}', pushOk);
      await r.device.push.register(platform: PushPlatform.fcm, handle: 'h1');
      await r.device.push.register(platform: PushPlatform.fcm, handle: 'h2');
      expect(r.tokens, hasLength(1));
      expect(
          r.fake.requests[0].headers['authorization'], 'Bearer ${r.tokens[0]}');
      r.now = r.now.add(const Duration(minutes: 59, seconds: 30));
      await r.device.push.register(platform: PushPlatform.fcm, handle: 'h3');
      expect(r.tokens, hasLength(2));
      expect(
          r.fake.requests[2].headers['authorization'], 'Bearer ${r.tokens[1]}');
    });

    for (final code in [
      'USER_TOKEN_EXPIRED',
      'USER_TOKEN_REVOKED',
      'AUTHENTICATION_FAILED'
    ]) {
      test('401 $code: a fresh token, retried once', () async {
        final r = Rig();
        r.fake
            .onRequest(
                'PUT',
                '/v1/messaging/user-push-registrations/{installationId}',
                FakeResponse.error(401, code: code))
            .thenReply(pushOk);
        await r.device.push.register(platform: PushPlatform.apns, handle: 'h');
        expect(r.tokens, hasLength(2));
        expect(r.fake.requests[1].headers['authorization'],
            'Bearer ${r.tokens[1]}');
      });
    }

    test('a second 401 is thrown; other codes are not retried', () async {
      final r = Rig();
      r.fake.onRequest(
          'PUT',
          '/v1/messaging/user-push-registrations/{installationId}',
          const FakeResponse.error(401, code: 'USER_TOKEN_REVOKED'));
      await expectLater(
          r.device.push.register(platform: PushPlatform.fcm, handle: 'h'),
          throwsA(isA<CfMeetAuthenticationError>()));
      expect(r.fake.requests, hasLength(2));

      final s = Rig();
      s.fake.onRequest(
          'PUT',
          '/v1/messaging/user-push-registrations/{installationId}',
          const FakeResponse.error(401, code: 'SOMETHING_ELSE'));
      await expectLater(
          s.device.push.register(platform: PushPlatform.fcm, handle: 'h'),
          throwsA(isA<CfMeetAuthenticationError>()));
      expect(s.fake.requests, hasLength(1));
    });

    test('a provider returning a platform credential is refused before sending',
        () async {
      final fake = FakeCfMeetTransport();
      final device = CfMeetDeviceClient(
          baseUrl: base,
          transport: fake,
          tokenProvider: () => credential,
          installationId: installation);
      await expectLater(
          device.push.register(platform: PushPlatform.fcm, handle: 'h'),
          throwsA(isA<CfMeetUsageError>()));
      expect(fake.requests, isEmpty);
    });
  });

  group('users helpers (server-side)', () {
    late FakeCfMeetTransport fake;
    late CfMeetClient client;
    final user = {
      'id': principal,
      'externalId': 'rider-42',
      'displayName': 'Rider 42',
      'imageUrl': null,
      'status': 'ACTIVE',
      'createdAt': '2026-11-18T10:00:00Z',
      'updatedAt': '2026-11-18T10:00:00Z',
    };
    setUp(() {
      fake = FakeCfMeetTransport();
      client = CfMeetClient(
          baseUrl: base,
          auth: CfMeetAuth.platformCredential(credential),
          transport: fake,
          retryOptions: FakeClock().retryOptions());
    });

    test('create / upsert', () async {
      fake
          .onRequest('POST', '/v1/users', FakeResponse.json(user, status: 201))
          .thenReply(FakeResponse.json(user));
      final u = await client.users.create(
          externalId: 'rider-42',
          displayName: 'Rider 42',
          idempotencyKey: 'k1');
      expect(u.id, principal);
      final req = fake.requests[0];
      expect(req.headers['authorization'], 'Bearer $credential');
      expect(req.headers['idempotency-key'], 'k1');
      expect(req.body, {'externalId': 'rider-42', 'displayName': 'Rider 42'});
      final again = await client.users.upsert(externalId: 'rider-42');
      expect(again.created, isFalse);
    });

    test('list / all / update / delete / revokeSessions', () async {
      fake
          .onRequest(
              'GET',
              '/v1/users',
              FakeResponse.json({
                'items': [user],
                'nextCursor': 'c2'
              }))
          .thenReply(FakeResponse.json({
            'items': [
              {...user, 'id': endpoint}
            ],
            'nextCursor': null
          }));
      fake.onRequest('PATCH', '/v1/users/{userId}',
          FakeResponse.json({...user, 'status': 'SUSPENDED'}));
      fake.onRequest(
          'DELETE', '/v1/users/{userId}', const FakeResponse(status: 204));
      fake.onRequest('POST', '/v1/users/{userId}/revoke-sessions',
          const FakeResponse.json({'tokensNotBefore': '2026-11-18T10:00:00Z'}));
      final ids = await client.users.all(limit: 1).map((u) => u.id).toList();
      expect(ids, [principal, endpoint]);
      expect(
          fake.requests[1].url.queryParameters, {'limit': '1', 'cursor': 'c2'});
      final updated = await client.users.update(principal, status: 'SUSPENDED');
      expect(updated.status, 'SUSPENDED');
      expect(fake.requests[2].body, {'status': 'SUSPENDED'});
      await client.users.delete(principal);
      expect(fake.requests[3].method, 'DELETE');
      expect(
          await client.users.revokeSessions(principal), '2026-11-18T10:00:00Z');
    });

    test('createToken / upsertWithToken; a redacted replay is an error',
        () async {
      final token = userToken(1, t0.add(const Duration(hours: 1)));
      fake.onRequest(
          'POST',
          '/v1/users/{userId}/tokens',
          FakeResponse.json({
            'token': token,
            'tokenId': sessionDevice,
            'expiresAt': '2026-11-18T11:00:00Z'
          }, status: 201));
      fake
          .onRequest(
              'POST',
              '/v1/users/tokens',
              FakeResponse.json({
                'user': user,
                'token': token,
                'tokenId': sessionDevice,
                'expiresAt': '2026-11-18T11:00:00Z'
              }, status: 201))
          .thenReply(FakeResponse.json(
              {'user': user, 'tokenRedacted': true, 'tokenId': sessionDevice},
              status: 201));
      final minted = await client.users.createToken(principal,
          grants:
              const UserTokenGrants(msg: ['read', 'send'], call: ['receive']),
          ttlSeconds: 900);
      expect(minted.token, token);
      expect(fake.requests[0].body, {
        'grants': {
          'msg': ['read', 'send'],
          'call': ['receive']
        },
        'ttlSeconds': 900
      });
      final up = await client.users.upsertWithToken(
          externalId: 'rider-42',
          grants: const UserTokenGrants(
              meetings: [MeetingGrant(conference, 'PARTICIPANT')]));
      expect(up.user!.id, principal);
      expect((fake.requests[1].body as Map)['grants'], {
        'mtg': [
          {'conferenceId': conference, 'role': 'PARTICIPANT'}
        ]
      });
      await expectLater(
          client.users.upsertWithToken(
              externalId: 'rider-42',
              grants: const UserTokenGrants(msg: ['read'])),
          throwsA(isA<CfMeetUsageError>()));
    });

    test('a userToken client cannot call the users routes', () async {
      final r = Rig();
      await expectLater(r.device.client.users.create(externalId: 'x'),
          throwsA(isA<CfMeetUsageError>()));
      expect(r.fake.requests, isEmpty);
    });
  });

  group('chat (device)', () {
    test('connect: user-sessions → ticket → WebSocket; events and REST',
        () async {
      final r = Rig();
      r.fake.onRequest('POST', '/v1/messaging/user-sessions',
          FakeResponse.json(messagingSession(1, t0), status: 201));
      r.fake.onRequest('POST', '/v1/realtime/tickets', ticketReply());
      r.fake.onRequest('POST', '/v1/conversations/{conversationId}/messages',
          const FakeResponse.json({'sequence': 7}, status: 201));
      r.fake.onRequest('PUT', '/v1/conversations/{conversationId}/read-cursor',
          const FakeResponse.json({'sequence': 7}));

      final chat = await r.device.chat.connect();
      final exchange = r.fake.requests[0];
      expect(exchange.path, '/v1/messaging/user-sessions');
      expect(exchange.headers['authorization'], 'Bearer ${r.tokens[0]}');
      expect(exchange.body, {'installationId': installation});
      expect(chat.principalId, principal);

      final received = <Object?>[];
      final sub = chat.messages.listen(received.add);
      final ws = await r.sockets.connection(1);
      expect(ws.protocols, ['cfmeet.messaging.v1', 'cfmeet.ticket.$ticket']);
      final tickets =
          r.fake.requests.where((q) => q.path == '/v1/realtime/tickets');
      expect(tickets.single.headers['authorization'],
          'CF-Client-Token client-token-1');
      ws.pushJson({'type': 'message.created'});
      await Future<void>.delayed(Duration.zero);
      expect(received, [
        {'type': 'message.created'}
      ]);

      await chat.sendMessage(conversation, 'on my way');
      final send = r.fake.requests.last;
      expect(send.headers['authorization'], 'CF-Client-Token client-token-1');
      expect(send.headers['cf-acting-principal'], principal);
      expect((send.body as Map)['text'], 'on my way');
      expect((send.body as Map)['clientMessageId'], isA<String>());
      await chat.markRead(conversation, 7);
      expect(r.fake.requests.last.body, {'sequence': 7});

      await sub.cancel();
      await chat.close();
      expect((await chat.done).byClient, isTrue);
      expect(ws.closedByClient, isTrue);
    });

    test('the client token is renewed (with sessionDeviceId) before expiry',
        () async {
      final r = Rig();
      r.fake
          .onRequest(
              'POST',
              '/v1/messaging/user-sessions',
              FakeResponse.json(
                  messagingSession(1, t0, const Duration(minutes: 5)),
                  status: 201))
          .thenReply(FakeResponse.json(messagingSession(
              2,
              t0.add(const Duration(seconds: 250)),
              const Duration(minutes: 5))));
      r.fake.onRequest(
          'GET',
          '/v1/conversations',
          const FakeResponse.json(
              {'conversations': <Object?>[], 'nextCursor': null}));
      final chat = await r.device.chat.connect(realtime: false);
      await chat.listConversations();
      expect(r.fake.requests.last.headers['authorization'],
          'CF-Client-Token client-token-1');
      r.now = r.now.add(const Duration(seconds: 250));
      await chat.listConversations();
      final renewal = r.fake.requests
          .where((q) => q.path == '/v1/messaging/user-sessions')
          .elementAt(1);
      expect(renewal.body,
          {'installationId': installation, 'sessionDeviceId': sessionDevice});
      expect(r.fake.requests.last.headers['authorization'],
          'CF-Client-Token client-token-2');
      await chat.close();
    });

    test(
        '404 on renewal starts a new device; a refused client token is renewed',
        () async {
      final r = Rig();
      r.fake
          .onRequest('POST', '/v1/messaging/user-sessions',
              FakeResponse.json(messagingSession(1, t0), status: 201))
          .thenReply(const FakeResponse.error(404, code: 'NOT_FOUND'))
          .thenReply(FakeResponse.json(messagingSession(2, t0), status: 201));
      r.fake
          .onRequest('GET', '/v1/conversations/{conversationId}/unread',
              const FakeResponse.error(401, code: 'AUTHENTICATION_FAILED'))
          .thenReply(const FakeResponse.json({'unread': 0}));
      final chat = await r.device.chat.connect(realtime: false);
      final res = await chat.unreadCount(conversation);
      expect(res.json, {'unread': 0});
      final exchanges = r.fake.requests
          .where((q) => q.path == '/v1/messaging/user-sessions')
          .toList();
      expect(exchanges, hasLength(3));
      expect(exchanges[2].body, {'installationId': installation});
      expect(r.fake.requests.last.headers['authorization'],
          'CF-Client-Token client-token-2');
      await chat.close();
    });
  });

  group('calls (device)', () {
    const invitation = '5e6f7081-92a3-44b5-86d7-e8f9a0b1c2d3';
    const call = '7f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7';
    const session = '9a9a9a9a-8b8b-4c7c-9d6d-5e5e5e5e5e5e';
    Map<String, Object?> incoming(int version) => {
          'schema': 'cfmeet.calling.realtime-signal',
          'type': 'call.incoming',
          'callId': call,
          'callKind': 'VOICE',
          'callState': 'DIALING',
          'callVersion': version,
          'sequence': 14,
          'invitation': {
            'invitationId': invitation,
            'expiresAt': '2026-11-18T10:00:45Z',
            'callerDisplayName': 'Ada'
          },
        };

    test('connect → signaling; invitation → accept → media credential',
        () async {
      final r = Rig();
      r.fake.onRequest('POST', '/v1/calling/user-sessions',
          FakeResponse.json(callingSession(1, t0), status: 201));
      r.fake.onRequest(
          'POST',
          '/v1/calling/invitations/{invitation_id}/accept',
          const FakeResponse.json(
              {'sessionId': session, 'outcome': 'ACCEPTED'}));
      r.fake.onRequest(
          'POST',
          '/v1/calling/sessions/{session_id}/media-credentials',
          const FakeResponse.json({
            'sessionId': session,
            'url': 'wss://media.test',
            'token': 'mt'
          }));

      final calls = await r.device.calls.connect(kind: DeviceKind.mobile);
      expect(r.fake.requests[0].body,
          {'installationId': installation, 'kind': 'MOBILE'});
      expect(calls.endpointId, endpoint);
      final invitations = <IncomingInvitation>[];
      final sub = calls.invitations.listen(invitations.add);
      final ws = await r.sockets.connection(1);
      expect(ws.authorization, 'Bearer cfsig_test1');
      ws.pushJson(incoming(6));
      ws.pushJson({...incoming(8), 'type': 'call.ringing', 'invitation': null});
      await Future<void>.delayed(Duration.zero);
      expect(invitations.single.invitationId, invitation);

      final accepted = await calls.accept(invitation);
      expect(r.fake.requests.last.body, {'expectedVersion': 8});
      expect(
          r.fake.requests.last.headers['authorization'], 'Bearer cfsig_test1');
      final media =
          await calls.mediaCredential(accepted.json['sessionId'] as String);
      expect(media.json['url'], 'wss://media.test');
      await expectLater(calls.decline('00000000-0000-4000-8000-000000000000'),
          throwsA(isA<CfMeetUsageError>()));
      await sub.cancel();
      await calls.close();
    });

    test('close 4001: exchanged again and reconnected with the new credential',
        () async {
      final r = Rig();
      r.fake
          .onRequest('POST', '/v1/calling/user-sessions',
              FakeResponse.json(callingSession(1, t0), status: 201))
          .thenReply(FakeResponse.json(callingSession(2, t0)));
      final calls = await r.device.calls.connect();
      final ws1 = await r.sockets.connection(1);
      ws1.closeFromServer(4001, 'credential expired');
      final ws2 = await r.sockets.connection(2);
      expect(ws2.authorization, 'Bearer cfsig_test2');
      await calls.close();
    });
  });

  group('meetings (device)', () {
    test(
        'join with the user token returns the RTC credential and participant token',
        () async {
      final r = Rig();
      final rtc = {
        'provider': 'livekit',
        'url': 'wss://rtc.test',
        'credential': 'lk-jwt',
        'expiresAt': '2026-11-18T10:10:00Z'
      };
      r.fake.onRequest(
          'POST',
          '/api/v2/conferences/{id}/join',
          FakeResponse.json({
            'participant': {'id': principal},
            'participantSession': {'id': endpoint},
            'participantToken': 'pt-1',
            'rtc': rtc,
            'rtcStatus': 'ISSUED',
          }));
      r.fake.onRequest(
          'POST',
          '/api/v2/conferences/{id}/media-credential',
          FakeResponse.json({
            'rtc': {...rtc, 'credential': 'lk-jwt-2'}
          }));
      final joined = await r.device.meetings
          .join(conference, installationId: installation);
      expect(
          r.fake.requests[0].headers['authorization'], 'Bearer ${r.tokens[0]}');
      expect(r.fake.requests[0].body, <String, Object?>{});
      expect(joined.participantToken, 'pt-1');
      expect(joined.rtc!.url, 'wss://rtc.test');
      expect(joined.rtc!.credential, 'lk-jwt');
      final renewed = await joined.renewMediaCredential();
      expect(renewed.credential, 'lk-jwt-2');
      expect(r.fake.requests[1].headers['x-participant-token'], 'pt-1');
      expect(r.fake.requests[1].body, {'participantSessionId': endpoint});
    });
  });

  group('push (device)', () {
    test('register and unregister', () async {
      final r = Rig();
      r.fake.onRequest('PUT',
          '/v1/messaging/user-push-registrations/{installationId}', pushOk);
      r.fake.onRequest(
          'DELETE',
          '/v1/messaging/user-push-registrations/{installationId}',
          const FakeResponse(status: 204));
      await r.device.push
          .register(platform: PushPlatform.apnsVoip, handle: 'voip');
      expect(r.fake.requests[0].path,
          '/v1/messaging/user-push-registrations/$installation');
      expect(
          r.fake.requests[0].body, {'platform': 'APNS_VOIP', 'handle': 'voip'});
      await r.device.push.unregister(platform: PushPlatform.apnsVoip);
      expect(r.fake.requests[1].url.queryParameters, {'platform': 'APNS_VOIP'});
      await expectLater(
          r.device.push.register(platform: PushPlatform.fcm, handle: ''),
          throwsA(isA<CfMeetUsageError>()));
    });
  });

  group('installation id', () {
    test('generated once, persisted, reused by the next client', () async {
      final storage = MemoryKeyValueStorage();
      final a = Rig(installationId: null, storage: storage);
      final ids = await Future.wait(
          [a.device.installationId(), a.device.installationId()]);
      expect(ids[0], ids[1]);
      expect(isValidInstallationId(ids[0]), isTrue);
      expect(storage.values[defaultInstallationIdKey], ids[0]);
      final b = Rig(installationId: null, storage: storage);
      expect(await b.device.installationId(), ids[0]);
    });

    test('an invalid stored value is replaced; reset() rotates', () async {
      final storage = MemoryKeyValueStorage({'k': 'bad id!'});
      final store = InstallationIdStore(storage: storage, key: 'k');
      final id = await store.get();
      expect(isValidInstallationId(id), isTrue);
      expect(storage.values['k'], id);
      final next = await store.reset();
      expect(next, isNot(id));
      expect(await store.get(), next);
      expect(generateInstallationId(), matches(installationIdPattern));
    });
  });
}
