/**
 * Webhook event types a console subscription may select: the `WebhookEventType` enum of
 * packages/contracts/platform-developer/webhooks/v1.schema.json (pinned by
 * tests/developer-portal.test.mjs). Pure module.
 */
export const WEBHOOK_EVENT_TYPES: string[] = [
  "call.requested",
  "call.incoming",
  "call.ringing",
  "call.ring_cancelled",
  "call.accepted",
  "call.declined",
  "call.cancelled",
  "call.answered",
  "call.connected",
  "call.route_changed",
  "call.held",
  "call.resumed",
  "call.transfer_started",
  "call.transferred",
  "call.transfer_failed",
  "call.ended",
  "call.failed",
  "call.no_answer",
  "call.busy",
  "call.answered_elsewhere",
  "call.invitation_expired",
  "voicemail.received",
  "voicemail.ready",
  "voicemail.deleted",
  "recording.ready",
  "recording.deleted",
  "relay.created",
  "relay.updated",
  "relay.revoked",
  "relay.expired",
  "relay.participant_changed",
  "usage.recorded",
  "limit.warning",
  "limit.exceeded",
  "sms.received",
  "sms.requested",
  "sms.submitted",
  "sms.delivered",
  "sms.failed",
  "sms.unknown",
  "sms.blocked",
  "conversation.created",
  "conversation.updated",
  "conversation.member_added",
  "conversation.member_removed",
  "message.created",
  "message.deleted",
  "message.moderated",
  "message.expired",
  "meeting.created",
  "meeting.started",
  "meeting.ended",
  "participant.joined",
  "participant.left",
  "artifact.ready",
  "artifact.deleted",
  "number.reservation_changed",
  "number.assignment_changed",
  "number.binding_changed",
  "number.port_changed",
  "number.release_changed",
  "number.lifecycle_changed",
  "number.suspended",
  "number.restored",
  "credential.created",
  "credential.rotated",
  "credential.revoked",
  "credential.expiring_soon",
  "credential.expired",
  "production_access.requested",
  "production_access.approved",
  "production_access.rejected",
];

/** Event types grouped by family prefix (`call`, `sms`, ...), for the picker. */
export function groupEventTypes(types: string[] = WEBHOOK_EVENT_TYPES): Record<string, string[]> {
  const groups: Record<string, string[]> = {};
  for (const type of types) {
    const family = type.split(".")[0];
    (groups[family] ??= []).push(type);
  }
  return groups;
}
