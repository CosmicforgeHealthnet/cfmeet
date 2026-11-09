import 'dart:convert';

import 'package:cfmeet_platform_sdk/models/meetings.dart' as meetings;
import 'package:test/test.dart';

/// An agenda page as the server sends it, with a field this SDK version does not know.
const _agendaPage = '''
{
  "items": [
    {
      "occurrenceKey": "2026-11-09T09:00:00Z",
      "meetingSpaceId": "8a3c0f0e-1d2b-4c3a-9e8f-0a1b2c3d4e5f",
      "conferenceId": null,
      "materialized": false,
      "view": "SUMMARY",
      "status": "SCHEDULED",
      "kind": "SCHEDULED",
      "scheduledStartAt": "2026-11-09T09:00:00Z",
      "isException": false,
      "relations": ["OWNER", "INVITEE"],
      "meetingSpace": {
        "meetingCode": "abc-defg-hij",
        "meetingType": "SCHEDULED",
        "status": "ACTIVE",
        "timezone": "Africa/Lagos",
        "accessPolicy": "INVITE_ONLY",
        "requiresPasscode": true
      },
      "addedInAFutureVersion": {"anything": 1}
    }
  ],
  "nextCursor": "c2"
}
''';

void main() {
  group('generated Dart models', () {
    test('decode typed values, nested models and lists', () {
      final page = meetings.AgendaResponse.fromJson(
        jsonDecode(_agendaPage) as Map<String, Object?>,
      );
      expect(page.nextCursor, 'c2');
      expect(page.items, hasLength(1));
      final item = page.items.single;
      expect(item.occurrenceKey, '2026-11-09T09:00:00Z');
      expect(item.conferenceId, isNull);
      expect(item.materialized, isFalse);
      expect(item.relations, <String>['OWNER', 'INVITEE']);
      expect(item.meetingSpace.requiresPasscode, isTrue);
      expect(item.meetingSpace.timezone, 'Africa/Lagos');
      expect(item.meetingSpace.organizationId, isNull);
    });

    test('unknown fields survive a round trip', () {
      final decoded = jsonDecode(_agendaPage) as Map<String, Object?>;
      final page = meetings.AgendaResponse.fromJson(decoded);
      final again =
          jsonDecode(jsonEncode(page.toJson())) as Map<String, Object?>;
      final item =
          (again['items']! as List<Object?>).single! as Map<String, Object?>;
      expect(item['addedInAFutureVersion'], <String, Object?>{'anything': 1});
    });

    test('create builds the wire form and omits absent optional values', () {
      final space = meetings.AgendaSpace.create(
        meetingCode: 'abc-defg-hij',
        meetingType: 'SCHEDULED',
        status: 'ACTIVE',
        timezone: 'UTC',
        accessPolicy: 'OPEN',
        requiresPasscode: false,
      );
      expect(space.toJson().containsKey('organizationId'), isFalse);
      final page = meetings.AgendaResponse.create(
        items: <meetings.AgendaItem>[
          meetings.AgendaItem.create(
            occurrenceKey: 'k',
            meetingSpaceId: 's',
            materialized: true,
            view: 'FULL',
            status: 'LIVE',
            kind: 'INSTANT',
            scheduledStartAt: '2026-11-09T09:00:00Z',
            isException: false,
            relations: <meetings.AgendaRelation>['PARTICIPANT'],
            meetingSpace: space,
          ),
        ],
      );
      final wire =
          jsonDecode(jsonEncode(page.toJson())) as Map<String, Object?>;
      final item =
          (wire['items']! as List<Object?>).single! as Map<String, Object?>;
      expect(item['meetingSpace'], isA<Map<String, Object?>>());
      expect(
          (item['meetingSpace']! as Map<String, Object?>)['timezone'], 'UTC');
      expect(item['relations'], <Object?>['PARTICIPANT']);
      expect(wire.containsKey('nextCursor'), isFalse);
    });

    test('a missing required field fails loudly on access', () {
      final broken = meetings.AgendaSpace.fromJson(<String, Object?>{});
      expect(() => broken.meetingCode, throwsA(isA<TypeError>()));
      expect(broken.organizationId, isNull);
    });
  });
}
