import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';
import 'package:test/test.dart';

import 'support/mock_server.dart';

final String credential = 'cfmk_sbx_${'a' * 24}_${'b' * 64}';
final RegExp uuidV4 = RegExp(
    r'^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$');

const RetryOptions fast = RetryOptions(
  maxRetries: 3,
  baseDelay: Duration(milliseconds: 1),
  maxDelay: Duration(milliseconds: 5),
);

void main() {
  late MockServer server;
  late List<RetryEvent> retries;

  CfMeetClient client({
    CfMeetAuth? auth,
    Duration timeout = const Duration(seconds: 5),
    RetryOptions retryOptions = fast,
  }) =>
      CfMeetClient(
        baseUrl: server.url,
        auth: auth ?? CfMeetAuth.platformCredential(credential),
        timeout: timeout,
        retryOptions: retryOptions,
        onRetry: retries.add,
      );

  setUp(() async {
    server = await MockServer.start();
    retries = [];
  });
  tearDown(() => server.close());

  group('authentication', () {
    test(
        'a platform credential travels as a Bearer token; the response is decoded',
        () async {
      server.handler = (req, rec, n) => server.json(req, 200, {
            'id': 'm1'
          }, headers: {
            'x-request-id': 'req-1',
            'x-correlation-id': 'cor-1',
            'x-cf-quota-limit-minute': '600',
            'x-cf-quota-remaining-minute': '599',
            'x-cf-quota-reset': '42',
            'x-cf-quota-limit-day': '100000',
          });
      final r = await client()
          .sms
          .call('getMessage', pathParams: {'messageId': 'm1'});
      final rec = server.requests.single;
      expect(rec.method, 'GET');
      expect(rec.path, '/v1/sms/messages/m1');
      expect(rec.headers['authorization'], 'Bearer $credential');
      expect(rec.headers['accept'], 'application/json');
      expect(
          rec.headers['user-agent'], startsWith('cfmeet-platform-sdk-dart/'));
      expect(rec.headers.containsKey('idempotency-key'), isFalse);
      expect(r.status, 200);
      expect(r.json['id'], 'm1');
      expect(r.requestId, 'req-1');
      expect(r.correlationId, 'cor-1');
      expect(r.idempotentReplayed, isFalse);
      expect(r.attempts, 1);
      expect(r.quota!.limitMinute, 600);
      expect(r.quota!.remainingMinute, 599);
      expect(r.quota!.resetSeconds, 42);
      expect(r.quota!.limitDay, 100000);
    });

    test('participant token, client token and session placements', () async {
      final c = client(auth: CfMeetAuth.participantToken('pt-1'));
      await c.meetings.call('v2RealtimeTicket', pathParams: {'id': 'conf-1'});
      expect(server.requests.last.headers['x-participant-token'], 'pt-1');
      expect(
          server.requests.last.headers.containsKey('authorization'), isFalse);

      await client(auth: CfMeetAuth.clientToken('ct-1'))
          .messaging
          .call('issueRealtimeTicket');
      expect(server.requests.last.headers['authorization'],
          'CF-Client-Token ct-1');

      await client(auth: CfMeetAuth.meetingSession('sess-1'))
          .meetings
          .call('v2RealtimeTicket', pathParams: {'id': 'conf-1'});
      expect(server.requests.last.headers['authorization'], 'Bearer sess-1');
    });

    test('an async token provider is asked again on every attempt', () async {
      var n = 0;
      server.handler = (req, rec, i) => i == 1
          ? server.error(req, 503, 'SERVICE_UNAVAILABLE')
          : server.json(req, 200, {});
      final c = client(
          auth: CfMeetAuth.provider(
              AuthMode.clientToken, () async => 'tok-${++n}'));
      await c.messaging.call('listConversations');
      expect(server.requests.map((r) => r.headers['authorization']),
          ['CF-Client-Token tok-1', 'CF-Client-Token tok-2']);
    });

    test('a mode the operation does not accept is refused before sending',
        () async {
      final c = client(auth: CfMeetAuth.participantToken('pt-1'));
      await expectLater(
          c.sms.call('getMessage', pathParams: {'messageId': 'x'}),
          throwsA(isA<CfMeetUsageError>()));
      expect(server.requests, isEmpty);
    });

    test('anonymous operations never receive the credential', () async {
      await client()
          .meetings
          .call('v2ResolveInvitation', pathParams: {'token': 't1'});
      expect(
          server.requests.single.headers.containsKey('authorization'), isFalse);
      await client().discovery();
      expect(server.requests.last.path, '/discovery');
      expect(
          server.requests.last.headers.containsKey('authorization'), isFalse);
    });

    test('credentials are validated and redacted', () {
      expect(() => CfMeetAuth.platformCredential('sk_live_123'),
          throwsA(isA<CfMeetUsageError>()));
      expect(() => CfMeetAuth.platformCredential('cfmk_prod_abc'),
          throwsA(isA<CfMeetUsageError>()));
      expect(CfMeetAuth.platformCredential(credential).toString(),
          isNot(contains('bbbb')));
      expect(() => CfMeetAuth.meetingSession(''),
          throwsA(isA<CfMeetUsageError>()));
    });

    test('a session mode refuses a platform credential token', () async {
      final c = client(
          auth: CfMeetAuth.provider(AuthMode.meetingSession, () => credential));
      await expectLater(
          c.meetings.call('v2RealtimeTicket', pathParams: {'id': 'c'}),
          throwsA(isA<CfMeetUsageError>()));
      expect(server.requests, isEmpty);
    });

    test('http base URLs are refused unless loopback or explicitly allowed',
        () {
      expect(() => CfMeetClient(baseUrl: 'http://api.example.test'),
          throwsA(isA<CfMeetUsageError>()));
      expect(
          CfMeetClient(
                  baseUrl: 'http://api.example.test', allowInsecureHttp: true)
              .baseUri
              .host,
          'api.example.test');
      expect(() => CfMeetClient(baseUrl: 'https://u:p@api.example.test'),
          throwsA(isA<CfMeetUsageError>()));
      expect(() => CfMeetClient(baseUrl: 'ftp://api.example.test'),
          throwsA(isA<CfMeetUsageError>()));
    });
  });

  group('request building', () {
    test('path parameters are percent-encoded and unsafe values refused',
        () async {
      final c = client();
      await c.sms.call('getMessage', pathParams: {'messageId': 'a b+é?#%'});
      expect(server.requests.single.path,
          '/v1/sms/messages/a%20b%2B%C3%A9%3F%23%25');
      for (final bad in ['a/b', '..', '.', 'x..y', r'a\b', '', 'a\nb']) {
        await expectLater(
            c.sms.call('getMessage', pathParams: {'messageId': bad}),
            throwsA(isA<CfMeetUsageError>()),
            reason: bad);
      }
      await expectLater(
          c.sms.call('getMessage'), throwsA(isA<CfMeetUsageError>()));
      await expectLater(
          c.sms
              .call('getMessage', pathParams: {'messageId': 'a', 'other': 'b'}),
          throwsA(isA<CfMeetUsageError>()));
      expect(server.requests, hasLength(1));
    });

    test('a base URL path prefix is kept', () async {
      final c = CfMeetClient(
          baseUrl: '${server.url}/edge/',
          auth: CfMeetAuth.platformCredential(credential));
      await c.sms.call('getMessage', pathParams: {'messageId': 'm'});
      expect(server.requests.single.path, '/edge/v1/sms/messages/m');
    });

    test(
        'only documented query parameters; required ones enforced; lists repeat',
        () async {
      final c = client();
      await c.sms.call('listMessages',
          query: {'limit': 10, 'direction': 'INBOUND', 'cursor': null});
      expect(server.requests.single.query, {
        'limit': ['10'],
        'direction': ['INBOUND']
      });
      await expectLater(c.sms.call('listMessages', query: {'evil': '1'}),
          throwsA(isA<CfMeetUsageError>()));
      await expectLater(
          c.calling.call('deleteWebhook', pathParams: {'webhook_id': 'w'}),
          throwsA(isA<CfMeetUsageError>())); // expectedVersion is required
      await c.calling.call('deleteWebhook',
          pathParams: {'webhook_id': 'w'}, query: {'expectedVersion': 3});
      expect(server.requests.last.query['expectedVersion'], ['3']);
    });

    test('only documented and forwarded headers may be set', () async {
      final c = client();
      await expectLater(
          c.sms.call('getMessage',
              pathParams: {'messageId': 'm'}, headers: {'Authorization': 'x'}),
          throwsA(isA<CfMeetUsageError>()));
      await expectLater(
          c.sms.call('getMessage',
              pathParams: {'messageId': 'm'}, headers: {'x-cf-tenant': 't'}),
          throwsA(isA<CfMeetUsageError>()));
      await expectLater(
          c.sms.call('getMessage',
              pathParams: {'messageId': 'm'},
              headers: {'if-none-match': 'bad\nvalue'}),
          throwsA(isA<CfMeetUsageError>()));
      // A required documented header.
      await expectLater(
          c.messaging
              .call('acceptInvitation', pathParams: {'conversationId': 'c'}),
          throwsA(isA<CfMeetUsageError>()));
      await c.messaging.call('acceptInvitation',
          pathParams: {'conversationId': 'c'},
          headers: {'CF-Acting-Principal': 'p-1'});
      expect(server.requests.single.headers['cf-acting-principal'], 'p-1');
      await c.sms.call('getMessage',
          pathParams: {'messageId': 'm'}, headers: {'If-None-Match': '"v1"'});
      expect(server.requests.last.headers['if-none-match'], '"v1"');
    });

    test('bodies: JSON, raw bytes, required and forbidden', () async {
      final c = client();
      await c.sms.call('createOptOut', body: {'phoneNumber': '+15550100'});
      expect(server.requests.last.headers['content-type'],
          startsWith('application/json'));
      expect(
          jsonDecode(server.requests.last.body), {'phoneNumber': '+15550100'});
      await expectLater(
          c.sms.call('createOptOut'), throwsA(isA<CfMeetUsageError>()));
      await expectLater(
          c.sms.call('getMessage', pathParams: {'messageId': 'm'}, body: {}),
          throwsA(isA<CfMeetUsageError>()));
      await c.sms.call('createOptOut',
          body: RawBody(utf8.encode('raw'), contentType: 'text/plain'));
      expect(server.requests.last.body, 'raw');
      expect(server.requests.last.headers['content-type'],
          startsWith('text/plain'));
    });

    test(
        'Idempotency-Key: generated as UUID v4, caller value kept, refused where not taken',
        () async {
      final c = client();
      final r = await c.sms.call('sendMessage', body: {'to': '+15550100'});
      expect(r.idempotencyKey, matches(uuidV4));
      expect(server.requests.last.headers['idempotency-key'], r.idempotencyKey);
      await c.sms.call('sendMessage', body: {}, idempotencyKey: 'order-42');
      expect(server.requests.last.headers['idempotency-key'], 'order-42');
      await expectLater(
          c.sms.call('sendMessage', body: {}, idempotencyKey: 'bad key'),
          throwsA(isA<CfMeetUsageError>()));
      await expectLater(
          c.sms.call('getMessage',
              pathParams: {'messageId': 'm'}, idempotencyKey: 'k'),
          throwsA(isA<CfMeetUsageError>()));
      final keys = {for (var i = 0; i < 200; i++) newIdempotencyKey()};
      expect(keys, hasLength(200));
      expect(keys.every(uuidV4.hasMatch), isTrue);
    });

    test('operations are name-checked, with suggestions', () {
      final c = client();
      expect(c.operation('sms', 'sendMessage').spec.paidTelecom, isTrue);
      expect(
          () => c.operation('sms', 'sendMesage'),
          throwsA(isA<CfMeetUsageError>()
              .having((e) => e.message, 'message', contains('sendMessage'))));
      expect(() => c.domain('nope'), throwsA(isA<CfMeetUsageError>()));
      expect(c.sms.operationNames, contains('sendMessage'));
      for (final op in operations) {
        expect(findOperation(op.domain, op.name), same(op));
      }
    });

    test('WebSocket operations are refused by request()', () async {
      await expectLater(client().messaging.call('connectRealtime'),
          throwsA(isA<CfMeetUsageError>()));
    });

    test('the idempotent-replayed header is surfaced', () async {
      server.handler = (req, rec, n) => server.json(req, 200, {'id': 's1'},
          headers: {'idempotent-replayed': 'true'});
      final r = await client()
          .sms
          .call('sendMessage', body: {}, idempotencyKey: 'k-1');
      expect(r.idempotentReplayed, isTrue);
      expect(r.idempotencyKey, 'k-1');
    });
  });

  group('error mapping', () {
    final cases = <(int, String, TypeMatcher<CfMeetApiError>)>[
      (401, 'AUTHENTICATION_FAILED', isA<CfMeetAuthenticationError>()),
      (403, 'INSUFFICIENT_SCOPE', isA<CfMeetPermissionError>()),
      (404, 'NOT_FOUND', isA<CfMeetNotFoundError>()),
      (400, 'VALIDATION_ERROR', isA<CfMeetValidationError>()),
      (422, 'VALIDATION_ERROR', isA<CfMeetValidationError>()),
      (409, 'VERSION_CONFLICT', isA<CfMeetConflictError>()),
      (429, 'RATE_LIMITED', isA<CfMeetRateLimitedError>()),
      (429, 'QUOTA_EXCEEDED', isA<CfMeetQuotaExceededError>()),
      (502, 'UPSTREAM_UNAVAILABLE', isA<CfMeetUpstreamUnavailableError>()),
      (503, 'SERVICE_UNAVAILABLE', isA<CfMeetUpstreamUnavailableError>()),
      (504, 'UPSTREAM_TIMEOUT', isA<CfMeetUpstreamUnavailableError>()),
      (500, 'INTERNAL_ERROR', isA<CfMeetApiError>()),
    ];
    for (final (status, code, matcher) in cases) {
      test('$status $code', () async {
        server.handler =
            (req, rec, n) => server.error(req, status, code, headers: {
                  'retry-after': '120',
                  'x-cf-quota-window': 'minute',
                  'x-cf-quota-level': 'environment',
                });
        final err = await client(
                retryOptions: const RetryOptions(maxRetries: 0))
            .sms
            .call('getMessage', pathParams: {'messageId': 'm'}).then<Object>(
                (_) => fail('expected an error'),
                onError: (Object e) => e);
        expect(err, matcher);
        final e = err as CfMeetApiError;
        expect(e.status, status);
        expect(e.code, code);
        expect(e.message, 'msg $code');
        expect(e.requestId, 'req-$status');
        expect(e.retryAfter, const Duration(seconds: 120));
        expect(e.quota!.window, 'minute');
        expect(e.quota!.level, 'environment');
        if (code == 'QUOTA_EXCEEDED') expect(e, isA<CfMeetRateLimitedError>());
      });
    }

    test('a non-envelope error body still maps by status', () async {
      server.handler = (req, rec, n) async {
        req.response.statusCode = 404;
        req.response.write('nope');
        await req.response.close();
      };
      await expectLater(
          client().sms.call('getMessage', pathParams: {'messageId': 'm'}),
          throwsA(isA<CfMeetNotFoundError>()
              .having((e) => e.code, 'code', 'HTTP_404')));
    });
  });

  group('retries', () {
    test(
        'safe: 408 / 429 / 502 / 503 / 504 are retried, then the answer is returned',
        () async {
      final statuses = [408, 429, 502];
      server.handler = (req, rec, n) => n <= statuses.length
          ? server.error(req, statuses[n - 1], 'X')
          : server.json(req, 200, {'ok': 1});
      final r =
          await client().sms.call('getMessage', pathParams: {'messageId': 'm'});
      expect(r.attempts, 4);
      expect(server.requests, hasLength(4));
      expect(
          retries.map((e) => e.reason), ['HTTP 408', 'HTTP 429', 'HTTP 502']);
    });

    test('safe: retries are bounded by maxRetries', () async {
      server.handler =
          (req, rec, n) => server.error(req, 503, 'SERVICE_UNAVAILABLE');
      await expectLater(
          client().sms.call('getMessage', pathParams: {'messageId': 'm'}),
          throwsA(isA<CfMeetUpstreamUnavailableError>()));
      expect(server.requests, hasLength(4)); // 1 + maxRetries (3)
    });

    test('safe: 500 and 4xx are not retried', () async {
      for (final s in [500, 400, 401, 404]) {
        server.requests.clear();
        server.handler = (req, rec, n) => server.error(req, s, 'X');
        await expectLater(
            client().sms.call('getMessage', pathParams: {'messageId': 'm'}),
            throwsA(isA<CfMeetApiError>()));
        expect(server.requests, hasLength(1), reason: '$s');
      }
    });

    test('safe: a dropped connection is retried', () async {
      server.handler = (req, rec, n) =>
          n == 1 ? server.drop(req) : server.json(req, 200, {'ok': true});
      final r =
          await client().sms.call('getMessage', pathParams: {'messageId': 'm'});
      expect(r.attempts, 2);
      expect(retries.single.reason, 'network');
    });

    test('idempotent: retried with the SAME Idempotency-Key', () async {
      server.handler = (req, rec, n) => n < 3
          ? server.error(req, 503, 'SERVICE_UNAVAILABLE')
          : server.json(req, 200, {});
      final r = await client()
          .calling
          .call('acceptCall', pathParams: {'call_id': 'c1'}, body: {});
      expect(server.requests, hasLength(3));
      final keys =
          server.requests.map((q) => q.headers['idempotency-key']).toSet();
      expect(keys, hasLength(1));
      expect(keys.single, matches(uuidV4));
      expect(r.idempotencyKey, keys.single);
      expect(retries.every((e) => e.idempotencyKey == keys.single), isTrue);
    });

    test('idempotent: PUT without a key is retried on 502', () async {
      server.handler = (req, rec, n) =>
          n == 1 ? server.error(req, 502, 'X') : server.json(req, 200, {});
      await client().calling.call('replaceBusinessHours',
          pathParams: {'business_hours_id': 'b'}, body: {});
      expect(server.requests, hasLength(2));
    });

    test('noRetry: only 429 is retried', () async {
      server.handler =
          (req, rec, n) => server.error(req, 503, 'SERVICE_UNAVAILABLE');
      await expectLater(client().sms.call('createOptOut', body: {}),
          throwsA(isA<CfMeetUpstreamUnavailableError>()));
      expect(server.requests, hasLength(1));

      server.requests.clear();
      server.handler = (req, rec, n) => n == 1
          ? server.error(req, 429, 'RATE_LIMITED')
          : server.json(req, 200, {});
      await client().sms.call('createOptOut', body: {});
      expect(server.requests, hasLength(2));

      server.requests.clear();
      server.handler = (req, rec, n) => server.drop(req);
      await expectLater(client().sms.call('createOptOut', body: {}),
          throwsA(isA<CfMeetNetworkError>()));
      expect(server.requests, hasLength(1));
    });

    for (final status in [500, 502, 504]) {
      test('never: $status makes exactly one request and is an unknown outcome',
          () async {
        server.handler = (req, rec, n) => server.error(req, status, 'X');
        final err = await client()
            .sms
            .call('sendMessage',
                body: {'to': '+15550100'}, idempotencyKey: 'send-1')
            .then<Object>((_) => fail('expected an error'),
                onError: (Object e) => e);
        expect(server.requests, hasLength(1));
        expect(err, isA<UnknownOutcomeError>());
        final u = err as UnknownOutcomeError;
        expect(u.idempotencyKey, 'send-1');
        expect(u.operation, 'sms.sendMessage');
        expect(u.cause, isA<CfMeetApiError>());
        expect(u.requestId, 'req-$status');
        expect(retries, isEmpty);
      });
    }

    test('never: a timeout makes exactly one request and is an unknown outcome',
        () async {
      server.handler = (req, rec, n) async {
        await Future<void>.delayed(const Duration(seconds: 2));
        await server.json(req, 200, {});
      };
      final err = await client(timeout: const Duration(milliseconds: 200))
          .sms
          .call('sendMessage', body: {}).then<Object>(
              (_) => fail('expected an error'),
              onError: (Object e) => e);
      expect(server.requests, hasLength(1));
      expect(err, isA<UnknownOutcomeError>());
      final u = err as UnknownOutcomeError;
      expect(u.cause, isA<CfMeetTimeoutError>());
      expect(u.idempotencyKey, matches(uuidV4));
      expect(
          u.idempotencyKey, server.requests.single.headers['idempotency-key']);
    });

    test('never: a connection lost after sending is an unknown outcome',
        () async {
      server.handler = (req, rec, n) => server.drop(req);
      await expectLater(
          client().sms.call('sendMessage', body: {}),
          throwsA(isA<UnknownOutcomeError>()
              .having((e) => e.cause, 'cause', isA<CfMeetNetworkError>())));
      expect(server.requests, hasLength(1));
    });

    test('never: 503 and 429 are refusals, not retried', () async {
      server.handler =
          (req, rec, n) => server.error(req, 503, 'SERVICE_UNAVAILABLE');
      await expectLater(client().sms.call('sendMessage', body: {}),
          throwsA(isA<CfMeetUpstreamUnavailableError>()));
      server.handler = (req, rec, n) => server.error(req, 429, 'RATE_LIMITED');
      await expectLater(client().sms.call('sendMessage', body: {}),
          throwsA(isA<CfMeetRateLimitedError>()));
      expect(server.requests, hasLength(2));
    });

    test('never: a refused connection (nothing sent) is a network error',
        () async {
      final port = server.url.split(':').last;
      await server.close();
      final c = CfMeetClient(
          baseUrl: 'http://127.0.0.1:$port',
          auth: CfMeetAuth.platformCredential(credential));
      await expectLater(
          c.sms.call('sendMessage', body: {}),
          throwsA(isA<CfMeetNetworkError>()
              .having((e) => e.requestSent, 'requestSent', isFalse)));
      server = await MockServer.start(); // for tearDown
    });

    test('Retry-After (seconds) is honored', () async {
      server.handler = (req, rec, n) => n == 1
          ? server.error(req, 503, 'SERVICE_UNAVAILABLE',
              headers: {'retry-after': '1'})
          : server.json(req, 200, {});
      final sw = Stopwatch()..start();
      await client().sms.call('getMessage', pathParams: {'messageId': 'm'});
      expect(retries.single.delay, const Duration(seconds: 1));
      expect(sw.elapsedMilliseconds, greaterThanOrEqualTo(950));
    });

    test('a Retry-After beyond the cap is not waited for', () async {
      server.handler = (req, rec, n) => server
          .error(req, 429, 'RATE_LIMITED', headers: {'retry-after': '3600'});
      await expectLater(
          client().sms.call('getMessage', pathParams: {'messageId': 'm'}),
          throwsA(isA<CfMeetRateLimitedError>().having(
              (e) => e.retryAfter, 'retryAfter', const Duration(hours: 1))));
      expect(server.requests, hasLength(1));
    });

    test('Retry-After parsing: seconds and HTTP dates', () {
      final now = DateTime.utc(2026, 10, 23, 12, 0, 0);
      expect(parseRetryAfter('7', now: now), const Duration(seconds: 7));
      expect(parseRetryAfter(' 0 ', now: now), Duration.zero);
      expect(parseRetryAfter('Fri, 23 Oct 2026 12:00:30 GMT', now: now),
          const Duration(seconds: 30));
      expect(parseRetryAfter('Friday, 23-Oct-26 12:01:00 GMT', now: now),
          const Duration(minutes: 1));
      expect(parseRetryAfter('Fri Oct 23 12:00:05 2026', now: now),
          const Duration(seconds: 5));
      expect(parseRetryAfter('Fri, 23 Oct 2026 11:00:00 GMT', now: now),
          Duration.zero);
      expect(parseRetryAfter('soon', now: now), isNull);
      expect(parseRetryAfter('-5', now: now), isNull);
      expect(parseRetryAfter(null), isNull);
    });

    test('backoff grows exponentially with full jitter, capped', () {
      const o = RetryOptions(
          baseDelay: Duration(milliseconds: 100),
          maxDelay: Duration(seconds: 1));
      final rnd = _MaxRandom();
      expect(o.backoff(0, rnd), const Duration(milliseconds: 100));
      expect(o.backoff(2, rnd), const Duration(milliseconds: 400));
      expect(o.backoff(10, rnd), const Duration(seconds: 1));
    });

    test('retry classes of the catalogue', () {
      expect(shouldRetryStatus(RetryClass.safe, 503), isTrue);
      expect(shouldRetryStatus(RetryClass.noRetry, 503), isFalse);
      expect(shouldRetryStatus(RetryClass.noRetry, 429), isTrue);
      expect(shouldRetryStatus(RetryClass.never, 429), isFalse);
      for (final op in operations.where((o) => o.paidTelecom)) {
        expect(op.retry, RetryClass.never, reason: op.qualifiedName);
      }
      expect(
          operations.where((o) => o.retry == RetryClass.never), hasLength(10));
    });
  });

  group('timeouts and cancellation', () {
    test('a safe operation times out per attempt, retries, then throws',
        () async {
      server.handler = (req, rec, n) async {
        await Future<void>.delayed(const Duration(seconds: 2));
        await server.json(req, 200, {});
      };
      final err = await client(
              timeout: const Duration(milliseconds: 150),
              retryOptions: const RetryOptions(
                  maxRetries: 1, baseDelay: Duration(milliseconds: 1)))
          .sms
          .call('getMessage', pathParams: {'messageId': 'm'}).then<Object>(
              (_) => fail('expected an error'),
              onError: (Object e) => e);
      expect(err, isA<CfMeetTimeoutError>());
      expect(server.requests, hasLength(2));
    });

    test('a per-call timeout overrides the client default', () async {
      server.handler = (req, rec, n) async {
        await Future<void>.delayed(const Duration(milliseconds: 300));
        await server.json(req, 200, {});
      };
      final c = client(
          timeout: const Duration(milliseconds: 50),
          retryOptions: const RetryOptions(maxRetries: 0));
      final r = await c.sms.call('getMessage',
          pathParams: {'messageId': 'm'}, timeout: const Duration(seconds: 3));
      expect(r.status, 200);
    });

    test('cancelling an in-flight request', () async {
      server.handler = (req, rec, n) async {
        await Future<void>.delayed(const Duration(seconds: 2));
        await server.json(req, 200, {});
      };
      final token = CancelToken();
      final f = client().sms.call('getMessage',
          pathParams: {'messageId': 'm'}, cancelToken: token);
      await Future<void>.delayed(const Duration(milliseconds: 100));
      token.cancel();
      await expectLater(f, throwsA(isA<CfMeetCancelledError>()));
      expect(server.requests, hasLength(1));
    });

    test('cancelling while waiting to retry stops the retries', () async {
      server.handler = (req, rec, n) => server.error(
          req, 503, 'SERVICE_UNAVAILABLE',
          headers: {'retry-after': '30'});
      final token = CancelToken();
      final f = client().sms.call('getMessage',
          pathParams: {'messageId': 'm'}, cancelToken: token);
      await Future<void>.delayed(const Duration(milliseconds: 200));
      token.cancel();
      await expectLater(f, throwsA(isA<CfMeetCancelledError>()));
      expect(server.requests, hasLength(1));
    });

    test('an already-cancelled token sends nothing', () async {
      final token = CancelToken()..cancel();
      await expectLater(
          client().sms.call('getMessage',
              pathParams: {'messageId': 'm'}, cancelToken: token),
          throwsA(isA<CfMeetCancelledError>()));
      expect(server.requests, isEmpty);
    });
  });

  group('pagination', () {
    test('follows nextCursor across pages', () async {
      server.handler = (req, rec, n) {
        final cursor = rec.query['cursor']?.single;
        final page = switch (cursor) {
          null => {
              'departments': [
                {'id': 1},
                {'id': 2}
              ],
              'nextCursor': 'c2'
            },
          'c2' => {
              'departments': [
                {'id': 3}
              ],
              'nextCursor': 'c3'
            },
          _ => {
              'departments': [
                {'id': 4}
              ],
              'nextCursor': null
            },
        };
        return server.json(req, 200, page);
      };
      final items = await client()
          .calling
          .paginate('listDepartments', query: {'limit': 2}).toList();
      expect(items.map((i) => i['id']), [1, 2, 3, 4]);
      expect(server.requests.map((r) => r.query['cursor']?.single),
          [null, 'c2', 'c3']);
      expect(server.requests.every((r) => r.query['limit']!.single == '2'),
          isTrue);
    });

    test('maxPages bounds the walk; a repeated cursor stops it', () async {
      server.handler = (req, rec, n) => server.json(req, 200, {
            'departments': [
              {'id': n}
            ],
            'nextCursor': 'same'
          });
      final two = await client()
          .calling
          .paginate('listDepartments', maxPages: 1)
          .toList();
      expect(two, hasLength(1));
      await expectLater(client().calling.paginate('listDepartments').toList(),
          throwsA(isA<StateError>()));
    });

    test('a non-paginated operation is refused', () async {
      await expectLater(
          client()
              .sms
              .paginate('getMessage', pathParams: {'messageId': 'm'}).toList(),
          throwsA(isA<CfMeetUsageError>()));
    });

    test('every paginated operation names its cursor, next and items fields',
        () {
      final paginated = operations.where((o) => o.pagination != null).toList();
      expect(paginated, isNotEmpty);
      for (final op in paginated) {
        expect(op.method, 'GET');
        expect(
            op.query.map((q) => q.name), contains(op.pagination!.cursorParam));
      }
    });
  });

  test('IoTransport accepts an HttpClient override', () async {
    final http = HttpClient();
    final c = CfMeetClient(
        baseUrl: server.url,
        auth: CfMeetAuth.platformCredential(credential),
        transport: IoTransport(httpClient: http));
    await c.sms.call('getMessage', pathParams: {'messageId': 'm'});
    expect(server.requests, hasLength(1));
    http.close();
  });
}

class _MaxRandom implements Random {
  @override
  int nextInt(int max) => max - 1;
  @override
  bool nextBool() => true;
  @override
  double nextDouble() => 1;
}
