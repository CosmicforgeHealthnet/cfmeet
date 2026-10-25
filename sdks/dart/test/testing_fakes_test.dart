// The deterministic test fakes of package:cfmeet_platform_sdk/testing.dart (Part VI
// §52.3), used the way an application test would: a real CfMeetClient / RealtimeClient
// wired to the fakes, no network, no real time passing.
import 'dart:async';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:cfmeet_platform_sdk/testing.dart';
import 'package:test/test.dart';

final String credential = 'cfmk_sbx_${'a' * 24}_${'b' * 64}';

void main() {
  late FakeCfMeetTransport fake;
  late FakeClock clock;
  late CfMeetClient client;
  late Stopwatch wall;

  setUp(() {
    fake = FakeCfMeetTransport();
    clock = FakeClock();
    client = CfMeetClient(
      baseUrl: 'https://api.cfmeet.test',
      auth: CfMeetAuth.platformCredential(credential),
      transport: fake,
      retryOptions: clock.retryOptions(),
    );
    wall = Stopwatch()..start();
  });

  // Every test runs without sleeping: the SDK's waits go to the fake clock.
  tearDown(() => expect(wall.elapsed, lessThan(const Duration(seconds: 5))));

  group('FakeCfMeetTransport', () {
    test('serves a typed call and records the request', () async {
      fake.onOperation(
          'sms',
          'getMessage',
          const FakeResponse.json({'id': 'm1', 'status': 'delivered'},
              correlationId: 'cor_1',
              quota: QuotaInfo(
                  limitMinute: 600, remainingMinute: 599, resetSeconds: 42)));

      final r =
          await client.sms.call('getMessage', pathParams: {'messageId': 'm 1'});

      expect(r.status, 200);
      expect(r.json, {'id': 'm1', 'status': 'delivered'});
      expect(r.requestId, 'req_fake_000001');
      expect(r.correlationId, 'cor_1');
      expect(r.quota!.limitMinute, 600);
      expect(r.quota!.remainingMinute, 599);
      expect(r.quota!.resetSeconds, 42);
      expect(r.attempts, 1);

      final req = fake.requests.single;
      expect(req.index, 1);
      expect(req.requestId, 'req_fake_000001');
      expect(req.method, 'GET');
      expect(req.path, '/v1/sms/messages/m%201');
      expect(req.pathParams, {'messageId': 'm 1'});
      expect(req.operationName, 'sms.getMessage');
      expect(req.headers['authorization'], 'Bearer $credential');
      expect(req.idempotencyKey, isNull);
      expect(req.body, isNull);
      expect(fake.requestsFor('sms', 'getMessage'), [req]);
      expect(req.toString(), isNot(contains(credential)));
    });

    test('records query, headers and the decoded JSON body', () async {
      fake.onOperation('messaging', 'listMessages',
          const FakeResponse.json({'messages': <Object>[]}));
      fake.onOperation('calling', 'holdCall',
          const FakeResponse.json({'id': 'c1', 'state': 'HELD'}));

      await client.messaging.call('listMessages',
          pathParams: {'conversationId': 'conv_1'},
          query: {'limit': 10, 'afterSequence': 4},
          headers: {'cf-acting-principal': 'usr_1'});
      await client.calling.call('holdCall',
          pathParams: {'call_id': 'c1'},
          body: {'version': 3},
          idempotencyKey: 'hold-c1-v3');

      final list = fake.requests[0];
      expect(list.query, {'limit': '10', 'afterSequence': '4'});
      expect(list.headers['cf-acting-principal'], 'usr_1');
      final hold = fake.requests[1];
      expect(hold.method, 'POST');
      expect(hold.json, {'version': 3});
      expect(hold.idempotencyKey, 'hold-c1-v3');
      expect(hold.requestId, 'req_fake_000002');
    });

    test('operation names are validated against the catalogue', () {
      expect(
          () => fake.onOperation('sms', 'sendMesage'),
          throwsA(isA<CfMeetUsageError>()
              .having((e) => e.message, 'message', contains('sendMessage'))));
      expect(() => fake.onOperation('telecom', 'call'),
          throwsA(isA<CfMeetUsageError>()));
      expect(() => fake.onOperation('messaging', 'connectRealtime'),
          throwsArgumentError);
      expect(() => FakeResponse.page('sms', 'getMessage', const []),
          throwsArgumentError);
      expect(() => fake.onRequest('FETCH', '/x'), throwsArgumentError);
    });

    test('an unregistered call gets the default 404 NOT_FOUND envelope',
        () async {
      await expectLater(
          client.sms.call('getMessage', pathParams: {'messageId': 'm1'}),
          throwsA(isA<CfMeetNotFoundError>()
              .having((e) => e.status, 'status', 404)
              .having((e) => e.code, 'code', 'NOT_FOUND')
              .having((e) => e.requestId, 'requestId', 'req_fake_000001')));
      expect(fake.unmatched.single.operationName, 'sms.getMessage');
    });

    test('method + path routes (templates too) take precedence', () async {
      fake.onOperation(
          'sms', 'getMessage', const FakeResponse.json({'from': 'operation'}));
      fake.onRequest('GET', '/v1/sms/messages/{messageId}',
          const FakeResponse.json({'from': 'template'}));
      fake.onRequest('get', '/v1/sms/messages/special',
          const FakeResponse.json({'from': 'literal'}));

      Future<Object?> get(String id) async =>
          (await client.sms.call('getMessage', pathParams: {'messageId': id}))
              .json['from'];
      expect(await get('special'), 'literal');
      expect(await get('other'), 'template');

      final discovery = fake.onRequest(
          'GET', '/discovery', const FakeResponse.json({'apis': <Object>[]}));
      await client.discovery();
      expect(discovery.callCount, 1);
      expect(fake.requests.last.headers.containsKey('authorization'), isFalse);
    });

    test('scripted sequences: replies in order, the last one repeats',
        () async {
      var n = 0;
      final route = fake
          .onOperation('sms', 'getMessage', const FakeResponse.json({'n': 1}))
          .thenReplyWith((req) => FakeResponse.json(
              {'n': 2, 'id': req.pathParams['messageId'], 'seen': ++n}));
      Future<Map<String, dynamic>> get() async =>
          (await client.sms.call('getMessage', pathParams: {'messageId': 'x'}))
              .json;
      expect((await get())['n'], 1);
      expect(route.isExhausted, isFalse);
      expect(await get(), {'n': 2, 'id': 'x', 'seen': 1});
      expect(await get(), {'n': 2, 'id': 'x', 'seen': 2});
      expect(route.isExhausted, isTrue);
      expect(route.callCount, 3);
    });

    test('custom request ids, path prefixes and reset', () async {
      final t = FakeCfMeetTransport(
          pathPrefix: '/gw', requestIds: (n) => 'req_${'$n'.padLeft(32, '0')}');
      final c = CfMeetClient(
          baseUrl: 'https://api.cfmeet.test/gw',
          auth: CfMeetAuth.platformCredential(credential),
          transport: t);
      t.onOperation('sms', 'getMessage', const FakeResponse.json({}));
      final r = await c.sms.call('getMessage', pathParams: {'messageId': 'm1'});
      expect(r.requestId, 'req_${'1'.padLeft(32, '0')}');
      expect(t.requests.single.path, '/v1/sms/messages/m1');
      t.reset();
      expect(t.requests, isEmpty);
      await expectLater(
          c.sms.call('getMessage', pathParams: {'messageId': 'm1'}),
          throwsA(isA<CfMeetNotFoundError>()));
      c.close();
      expect(t.isClosed, isTrue);
    });
  });

  group('retries by retry class', () {
    test('SAFE: 503, network error and timeout are retried', () async {
      fake
          .onOperation('sms', 'getMessage', const FakeResponse.error(503))
          .thenReply(const FakeResponse.networkError())
          .thenReply(const FakeResponse.timeout())
          .thenReply(const FakeResponse.json({'id': 'm1'}));
      final r =
          await client.sms.call('getMessage', pathParams: {'messageId': 'm1'});
      expect(r.attempts, 4);
      expect(r.requestId, 'req_fake_000004');
      expect(fake.requests, hasLength(4));
      expect(clock.sleeps, hasLength(3));
    });

    test('SAFE: Retry-After is honoured on the fake clock, not slept',
        () async {
      fake
          .onOperation(
              'sms',
              'getMessage',
              const FakeResponse.error(429,
                  retryAfter: Duration(seconds: 20),
                  quota: QuotaInfo(remainingMinute: 0)))
          .thenReply(const FakeResponse.json({'id': 'm1'}));
      final r =
          await client.sms.call('getMessage', pathParams: {'messageId': 'm1'});
      expect(r.attempts, 2);
      expect(clock.sleeps, [const Duration(seconds: 20)]);
      expect(clock.elapsed, const Duration(seconds: 20));
    });

    test('SAFE: retries are bounded by maxRetries', () async {
      fake.onOperation('sms', 'getMessage', const FakeResponse.error(503));
      await expectLater(
          client.sms.call('getMessage', pathParams: {'messageId': 'm1'}),
          throwsA(isA<CfMeetUpstreamUnavailableError>()
              .having((e) => e.requestId, 'requestId', 'req_fake_000004')));
      expect(fake.requests, hasLength(4)); // 1 + maxRetries (3)
    });

    test('IDEMPOTENT: the same Idempotency-Key on every attempt', () async {
      fake
          .onOperation('calling', 'holdCall', const FakeResponse.error(502))
          .thenReply(const FakeResponse.networkError())
          .thenReply(
              const FakeResponse.json({'id': 'c1'}, idempotentReplayed: true));
      final r = await client.calling.call('holdCall',
          pathParams: {'call_id': 'c1'}, body: {'version': 1});
      final keys = fake.requests.map((q) => q.idempotencyKey).toList();
      expect(keys, hasLength(3));
      expect(keys.toSet(), hasLength(1));
      expect(keys.first, isNotNull);
      expect(r.idempotencyKey, keys.first);
      expect(r.idempotentReplayed, isTrue);
      expect(r.attempts, 3);
    });

    test('NO_RETRY: 503 is not retried (one request); 429 is', () async {
      fake.onOperation('sms', 'createOptOut', const FakeResponse.error(503));
      await expectLater(
          client.sms.call('createOptOut', body: {'phoneNumber': '+15550100'}),
          throwsA(isA<CfMeetUpstreamUnavailableError>()));
      expect(fake.requests, hasLength(1));

      fake
          .onOperation('sms', 'createOptOut', const FakeResponse.error(429))
          .thenReply(const FakeResponse.json({'id': 'o1'}, status: 201));
      final r = await client.sms
          .call('createOptOut', body: {'phoneNumber': '+15550100'});
      expect(r.status, 201);
      expect(fake.requests, hasLength(3));
    });

    group('NEVER (paid telecom): exactly one request, whatever happens', () {
      Future<Object> send() async {
        try {
          await client.sms.call('sendMessage',
              body: {'to': '+15550100', 'text': 'hi'},
              idempotencyKey: 'order-42');
        } on Object catch (e) {
          return e;
        }
        fail('expected an error');
      }

      test('503 is thrown as is', () async {
        fake.onOperation('sms', 'sendMessage', const FakeResponse.error(503));
        expect(await send(), isA<CfMeetUpstreamUnavailableError>());
        expect(fake.requests, hasLength(1));
      });

      for (final (label, reply) in [
        ('502', const FakeResponse.error(502)),
        ('500', const FakeResponse.error(500)),
        ('a timeout', const FakeResponse.timeout()),
        ('a connection reset', const FakeResponse.networkError()),
      ]) {
        test('$label is an UnknownOutcomeError carrying the key', () async {
          fake.onOperation('sms', 'sendMessage', reply);
          final e = await send();
          expect(
              e,
              isA<UnknownOutcomeError>()
                  .having((e) => e.idempotencyKey, 'key', 'order-42')
                  .having((e) => e.operation, 'operation', 'sms.sendMessage'));
          expect(fake.requests.single.idempotencyKey, 'order-42');
          expect(clock.sleeps, isEmpty);
        });
      }

      test('a failure before sending is a plain network error', () async {
        fake.onOperation('sms', 'sendMessage',
            const FakeResponse.networkError(requestSent: false));
        expect(
            await send(),
            isA<CfMeetNetworkError>()
                .having((e) => e.requestSent, 'requestSent', isFalse));
        expect(fake.requests, hasLength(1));
      });

      test('a deliberate replay of the key is answered as replayed', () async {
        fake
            .onOperation('sms', 'sendMessage', const FakeResponse.error(504))
            .thenReply(const FakeResponse.json({'id': 'sms_1'},
                status: 202, idempotentReplayed: true));
        final e = await send() as UnknownOutcomeError;
        final r = await client.sms.call('sendMessage',
            body: {'to': '+15550100', 'text': 'hi'},
            idempotencyKey: e.idempotencyKey);
        expect(r.idempotentReplayed, isTrue);
        expect(fake.requests.map((q) => q.idempotencyKey),
            ['order-42', 'order-42']);
      });
    });

    test('cancellation interrupts a retry wait on the fake clock', () async {
      final gate = Completer<void>();
      final cancel = CancelToken();
      final c = CfMeetClient(
        baseUrl: 'https://api.cfmeet.test',
        auth: CfMeetAuth.platformCredential(credential),
        transport: fake,
        retryOptions: RetryOptions(sleeper: (_) => gate.future),
      );
      fake.onOperation('sms', 'getMessage', const FakeResponse.error(503));
      final call = c.sms.call('getMessage',
          pathParams: {'messageId': 'm1'}, cancelToken: cancel);
      await Future<void>.delayed(Duration.zero);
      cancel.cancel();
      await expectLater(call, throwsA(isA<CfMeetCancelledError>()));
      expect(fake.requests, hasLength(1));
    });
  });

  group('pagination', () {
    test('follows nextCursor across fake pages', () async {
      final route = fake.onPages('sms', 'listMessages', [
        [
          {'id': 'm1'},
          {'id': 'm2'},
        ],
        [
          {'id': 'm3'},
        ],
        [
          {'id': 'm4'},
        ],
      ]);
      final ids = await client.sms
          .paginate('listMessages', query: {'limit': 2})
          .map((m) => m['id'])
          .toList();
      expect(ids, ['m1', 'm2', 'm3', 'm4']);
      expect(route.requests.map((r) => r.query['cursor']),
          [null, 'fake_cursor_1', 'fake_cursor_2']);
      expect(route.requests.every((r) => r.query['limit'] == '2'), isTrue);
    });

    test('page helpers follow the pagination spec', () async {
      expect(
          fakePageBody('calling', 'listQueues', const [1], nextCursor: 'c2'), {
        'queues': const [1],
        'nextCursor': 'c2'
      });
      fake
          .onOperation(
              'calling',
              'listQueues',
              FakeResponse.page(
                  'calling',
                  'listQueues',
                  const [
                    {'id': 'q1'}
                  ],
                  nextCursor: 'n1'))
          .thenReply(FakeResponse.page('calling', 'listQueues', const [
            {'id': 'q2'}
          ]));
      final ids = await client.calling
          .paginate('listQueues')
          .map((q) => q['id'])
          .toList();
      expect(ids, ['q1', 'q2']);
      expect(fake.requests.last.query['cursor'], 'n1');
    });

    test('an unknown cursor is a validation error', () async {
      fake.onPages('sms', 'listMessages', [
        [
          {'id': 'm1'},
        ],
      ]);
      await expectLater(
          client.sms.call('listMessages', query: {'cursor': 'bogus'}),
          throwsA(isA<CfMeetValidationError>()
              .having((e) => e.code, 'code', 'VALIDATION_ERROR')));
    });
  });

  group('error mapping', () {
    final cases = <(int, String?, TypeMatcher<CfMeetApiError>, String)>[
      (400, null, isA<CfMeetValidationError>(), 'VALIDATION_ERROR'),
      (401, null, isA<CfMeetAuthenticationError>(), 'AUTHENTICATION_FAILED'),
      (403, null, isA<CfMeetPermissionError>(), 'INSUFFICIENT_SCOPE'),
      (404, null, isA<CfMeetNotFoundError>(), 'NOT_FOUND'),
      (
        409,
        'STALE_CALL_VERSION',
        isA<CfMeetConflictError>(),
        'STALE_CALL_VERSION'
      ),
      (
        429,
        'QUOTA_EXCEEDED',
        isA<CfMeetQuotaExceededError>(),
        'QUOTA_EXCEEDED'
      ),
      (431, null, isA<CfMeetValidationError>(), 'HEADERS_TOO_LARGE'),
    ];
    for (final (status, code, matcher, expected) in cases) {
      test('$status → $expected', () async {
        fake.onOperation(
            'calling',
            'holdCall',
            FakeResponse.error(status,
                code: code,
                message: 'refused',
                retryAfter: status == 429 ? const Duration(days: 1) : null,
                quota: status == 429
                    ? const QuotaInfo(
                        window: 'requests-per-day', level: 'environment')
                    : null));
        Object? error;
        try {
          await client.calling.call('holdCall',
              pathParams: {'call_id': 'c1'}, body: {'version': 1});
        } on CfMeetApiError catch (e) {
          error = e;
        }
        expect(error, matcher);
        final e = error! as CfMeetApiError;
        expect(e.status, status);
        expect(e.code, expected);
        expect(e.message, 'refused');
        expect(e.requestId, fake.requests.last.requestId);
        expect((e.body! as Map)['error'], {
          'code': expected,
          'message': 'refused',
          'requestId': fake.requests.last.requestId,
        });
        if (status == 429) {
          expect(e.retryAfter, const Duration(days: 1));
          expect(e.quota!.window, 'requests-per-day');
        }
        expect(fake.requests, hasLength(1)); // nothing here is retried
      });
    }

    test('fakeErrorEnvelope is the contract envelope', () {
      expect(fakeErrorEnvelope('RATE_LIMITED', requestId: 'req_1'), {
        'error': {
          'code': 'RATE_LIMITED',
          'message': 'fake RATE_LIMITED',
          'requestId': 'req_1',
        },
      });
      expect(fakeErrorCodeFor(504), 'UPSTREAM_TIMEOUT');
      expect(fakeQuotaHeaders(const QuotaInfo(limitDay: 5)),
          {'x-cf-quota-limit-day': '5'});
    });
  });

  group('FakeRealtimeConnector', () {
    late FakeRealtimeConnector sockets;
    late RealtimeClient realtime;

    setUp(() {
      sockets = FakeRealtimeConnector();
      realtime = RealtimeClient(client,
          connector: sockets.connector, options: clock.reconnectOptions());
    });

    test(
        'Messaging: frames both ways, reconnect with a fresh ticket, stop on 4401',
        () async {
      var minted = 0;
      fake.onOperation('messaging', 'issueRealtimeTicket').thenReplyWith((_) =>
          FakeResponse.json({
            'ticket': 'tkt_${++minted}',
            'expiresAt': '2026-01-01T00:00:00Z'
          }, status: 201));
      final received = <Object?>[];
      final conn = realtime.messaging(
          auth: CfMeetAuth.clientToken('ct_1'), onMessage: received.add);

      final s1 = await sockets.connection(1);
      expect(conn.state, RealtimeState.open);
      expect(s1.url.toString(), 'wss://api.cfmeet.test/v1/realtime');
      expect(s1.protocols, ['cfmeet.messaging.v1', 'cfmeet.ticket.tkt_1']);
      expect(s1.ticketSubprotocol, 'tkt_1');
      expect(s1.queryTicket, isNull); // never in the URL
      expect(s1.protocol, 'cfmeet.messaging.v1');
      expect(fake.requests.single.headers['authorization'],
          'CF-Client-Token ct_1');

      s1.pushJson({'type': 'message.created', 'id': 'msg_1'});
      conn.send({'type': 'ack', 'id': 'msg_1'});
      await Future<void>.delayed(Duration.zero);
      expect(received, [
        {'type': 'message.created', 'id': 'msg_1'},
      ]);
      expect(s1.sentJson, [
        {'type': 'ack', 'id': 'msg_1'},
      ]);

      s1.closeFromServer(1011, 'restarting');
      final s2 = await sockets.connection(2);
      expect(s2.ticketSubprotocol, 'tkt_2');
      expect(
          fake.requestsFor('messaging', 'issueRealtimeTicket'), hasLength(2));
      expect(conn.connections, 2);
      expect(clock.sleeps, hasLength(1));

      s2.closeFromServer(4401, 'authorization ended');
      final closed = await conn.done;
      expect(closed.code, 4401);
      expect(closed.byClient, isFalse);
      expect(sockets.sockets, hasLength(2));
      expect(minted, 2);
    });

    test('Meetings: query ticket, resume after the last sequence', () async {
      var minted = 0;
      fake.onOperation('meetings', 'v2RealtimeTicket').thenReplyWith((req) =>
          FakeResponse.json({'ticket': 'mt_${++minted}', 'cursor': 5}));
      final conn = realtime.meetings('conf_1',
          auth: CfMeetAuth.meetingSession('sess_1'));

      final s1 = await sockets.connection(1);
      expect(s1.url.path, '/api/v2/conferences/conf_1/realtime');
      expect(s1.url.queryParameters, {'ticket': 'mt_1', 'cursor': '5'});
      expect(s1.ticket, 'mt_1');
      expect(s1.protocols, isEmpty);

      s1.pushJson({
        'type': 'event',
        'event': {'sequence': 7},
      });
      s1.failWithError(); // dropped without a close frame
      final s2 = await sockets.connection(2);
      expect(s2.url.queryParameters, {'ticket': 'mt_2', 'cursor': '7'});
      expect(
          fake
              .requestsFor('meetings', 'v2RealtimeTicket')
              .map((r) => r.pathParams),
          [
            {'id': 'conf_1'},
            {'id': 'conf_1'},
          ]);

      final closed = await conn.close();
      expect(closed.byClient, isTrue);
      expect(s2.closedByClient, isTrue);
      expect(s2.closeCode, 1000);
    });

    test('Calling signaling: a fresh credential per connection', () async {
      var n = 0;
      final conn = realtime.callingSignaling(
          auth: CfMeetAuth.provider(
              AuthMode.signalingCredential, () => 'cfsig_${++n}'));
      final s1 = await sockets.connection(1);
      expect(s1.authorization, 'Bearer cfsig_1');
      expect(s1.protocols, ['cfmeet.signaling.v1']);
      s1.closeFromServer(1006);
      final s2 = await sockets.connection(2);
      expect(s2.authorization, 'Bearer cfsig_2');
      await conn.close();
      expect(fake.requests, isEmpty); // no ticket for signaling
    });

    test('scripted handshake failures: transient retried, 401 stops', () async {
      fake.onOperation('messaging', 'issueRealtimeTicket').thenReplyWith(
          (r) => FakeResponse.json({'ticket': 'tkt_${r.index}'}, status: 201));
      sockets
        ..failNext()
        ..refuseNext(status: 503);
      final conn = realtime.messaging(auth: CfMeetAuth.clientToken('ct_1'));
      final s1 = await sockets.connection(1);
      expect(sockets.handshakes, hasLength(3));
      expect(s1.ticketSubprotocol, 'tkt_3'); // a fresh ticket per attempt
      expect(clock.sleeps, hasLength(2));

      sockets.refuseNext(status: 401, code: 'AUTHENTICATION_FAILED');
      s1.closeFromServer(1012);
      final closed = await conn.done;
      expect(
          closed.error,
          isA<RealtimeHandshakeException>()
              .having((e) => e.status, 'status', 401));
      expect(sockets.handshakes, hasLength(4));
      expect(sockets.sockets, hasLength(1));
    });

    test('a refused ticket stops the connection before any handshake',
        () async {
      fake.onOperation(
          'messaging', 'issueRealtimeTicket', const FakeResponse.error(403));
      final conn = realtime.messaging(auth: CfMeetAuth.clientToken('ct_1'));
      final closed = await conn.done;
      expect(closed.error, isA<CfMeetPermissionError>());
      expect(sockets.handshakes, isEmpty);
    });
  });
}
