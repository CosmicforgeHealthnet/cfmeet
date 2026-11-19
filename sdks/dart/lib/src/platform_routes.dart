/// Hand-written operation specs of the CF Meet Platform "Build" routes (ADR 0062 / 0063,
/// `docs/platform/build-internal-contracts.md` §3, §4, §8).
///
/// The users helpers and the device clients send THESE specs through
/// `CfMeetClient.request`, so they work whether or not the generated catalogue already
/// holds the routes. Names match the contracts' operationIds.
library;

import 'operation.dart';

const List<String> _platform = ['PLATFORM_CREDENTIAL'];
const List<String> _user = ['USER_TOKEN'];

/// `/v1/users*` (Developer Console public API, platform credential; §3).
abstract final class UserRoutes {
  /// Idempotent on externalId (201 new, 200 existing): safe to retry.
  static const OperationSpec createUser = OperationSpec(
    domain: 'developer-platform',
    name: 'createUser',
    operationId: 'createUser',
    method: 'POST',
    path: '/v1/users',
    protocol: 'REST',
    auth: _platform,
    scope: 'principals.write',
    pathParams: [],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'OPTIONAL',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [200, 201],
  );

  static const OperationSpec listUsers = OperationSpec(
    domain: 'developer-platform',
    name: 'listUsers',
    operationId: 'listUsers',
    method: 'GET',
    path: '/v1/users',
    protocol: 'REST',
    auth: _platform,
    scope: 'principals.read',
    pathParams: [],
    query: [
      ParamSpec('externalId', required: false),
      ParamSpec('limit', required: false),
      ParamSpec('cursor', required: false),
    ],
    headers: [],
    body: 'NONE',
    idempotency: 'NONE',
    retry: RetryClass.safe,
    paidTelecom: false,
    pagination: PaginationSpec(
        cursorParam: 'cursor', nextField: 'nextCursor', itemsField: 'items'),
    success: [200],
  );

  static const OperationSpec getUser = OperationSpec(
    domain: 'developer-platform',
    name: 'getUser',
    operationId: 'getUser',
    method: 'GET',
    path: '/v1/users/{userId}',
    protocol: 'REST',
    auth: _platform,
    scope: 'principals.read',
    pathParams: ['userId'],
    query: [],
    headers: [],
    body: 'NONE',
    idempotency: 'NONE',
    retry: RetryClass.safe,
    paidTelecom: false,
    pagination: null,
    success: [200],
  );

  static const OperationSpec updateUser = OperationSpec(
    domain: 'developer-platform',
    name: 'updateUser',
    operationId: 'updateUser',
    method: 'PATCH',
    path: '/v1/users/{userId}',
    protocol: 'REST',
    auth: _platform,
    scope: 'principals.write',
    pathParams: ['userId'],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'NONE',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [200],
  );

  static const OperationSpec deleteUser = OperationSpec(
    domain: 'developer-platform',
    name: 'deleteUser',
    operationId: 'deleteUser',
    method: 'DELETE',
    path: '/v1/users/{userId}',
    protocol: 'REST',
    auth: _platform,
    scope: 'principals.write',
    pathParams: ['userId'],
    query: [],
    headers: [],
    body: 'NONE',
    idempotency: 'NONE',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [204],
  );

  static const OperationSpec revokeUserSessions = OperationSpec(
    domain: 'developer-platform',
    name: 'revokeUserSessions',
    operationId: 'revokeUserSessions',
    method: 'POST',
    path: '/v1/users/{userId}/revoke-sessions',
    protocol: 'REST',
    auth: _platform,
    scope: 'principals.write',
    pathParams: ['userId'],
    query: [],
    headers: [],
    body: 'NONE',
    idempotency: 'NONE',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [200],
  );

  /// Not retried: an Idempotency-Key replay returns the metadata WITHOUT the token.
  static const OperationSpec issueUserToken = OperationSpec(
    domain: 'developer-platform',
    name: 'issueUserToken',
    operationId: 'issueUserToken',
    method: 'POST',
    path: '/v1/users/{userId}/tokens',
    protocol: 'REST',
    auth: _platform,
    scope: 'user_tokens.issue',
    pathParams: ['userId'],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'OPTIONAL',
    retry: RetryClass.noRetry,
    paidTelecom: false,
    pagination: null,
    success: [201],
  );

  static const OperationSpec upsertUserToken = OperationSpec(
    domain: 'developer-platform',
    name: 'upsertUserToken',
    operationId: 'upsertUserToken',
    method: 'POST',
    path: '/v1/users/tokens',
    protocol: 'REST',
    auth: _platform,
    scope: 'user_tokens.issue',
    pathParams: [],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'OPTIONAL',
    retry: RetryClass.noRetry,
    paidTelecom: false,
    pagination: null,
    success: [201],
  );
}

/// Device exchange routes (user token; §4, §8).
abstract final class DeviceRoutes {
  static const OperationSpec messagingUserSession = OperationSpec(
    domain: 'messaging',
    name: 'exchangeUserSession',
    operationId: 'exchangeUserSession',
    method: 'POST',
    path: '/v1/messaging/user-sessions',
    protocol: 'REST',
    auth: _user,
    scope: null,
    pathParams: [],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'NONE',
    retry: RetryClass.noRetry,
    paidTelecom: false,
    pagination: null,
    success: [200, 201],
  );

  static const OperationSpec registerUserPush = OperationSpec(
    domain: 'messaging',
    name: 'registerUserPush',
    operationId: 'registerUserPush',
    method: 'PUT',
    path: '/v1/messaging/user-push-registrations/{installationId}',
    protocol: 'REST',
    auth: _user,
    scope: null,
    pathParams: ['installationId'],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'NONE',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [200],
  );

  static const OperationSpec unregisterUserPush = OperationSpec(
    domain: 'messaging',
    name: 'unregisterUserPush',
    operationId: 'unregisterUserPush',
    method: 'DELETE',
    path: '/v1/messaging/user-push-registrations/{installationId}',
    protocol: 'REST',
    auth: _user,
    scope: null,
    pathParams: ['installationId'],
    query: [ParamSpec('platform', required: false)],
    headers: [],
    body: 'NONE',
    idempotency: 'NONE',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [204],
  );

  /// Creates (201) or reuses (200) the installation's endpoint: safe to retry.
  static const OperationSpec callingUserSession = OperationSpec(
    domain: 'calling',
    name: 'exchangeUserSession',
    operationId: 'exchangeUserSession',
    method: 'POST',
    path: '/v1/calling/user-sessions',
    protocol: 'REST',
    auth: _user,
    scope: null,
    pathParams: [],
    query: [],
    headers: [],
    body: 'REQUIRED',
    idempotency: 'NONE',
    retry: RetryClass.idempotent,
    paidTelecom: false,
    pagination: null,
    success: [200, 201],
  );

  /// The existing join route, which also accepts a user token (§8).
  static const OperationSpec joinConference = OperationSpec(
    domain: 'meetings',
    name: 'v2JoinConference',
    operationId: 'v2_join_conference',
    method: 'POST',
    path: '/api/v2/conferences/{id}/join',
    protocol: 'REST',
    auth: _user,
    scope: null,
    pathParams: ['id'],
    query: [],
    headers: [],
    body: 'OPTIONAL',
    idempotency: 'NONE',
    retry: RetryClass.noRetry,
    paidTelecom: false,
    pagination: null,
    success: [200],
  );
}
