import type { ConferenceStatus, MeetingSpaceResponse, Role, SetRecurrenceRequest } from "@/types/v2";

export function schedulingError(error: unknown): string {
  const response = (error as { response?: { data?: { error?: { message?: string } } } })?.response;
  return response?.data?.error?.message || (error instanceof Error ? error.message : "The request failed. Please retry.");
}

/** datetime-local is deliberately retained as wall time for recurrence. */
export function recurrenceInput(rrule: string, startLocal: string, durationMinutes: number): SetRecurrenceRequest {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(startLocal)) throw new Error("Choose the first local date and time.");
  if (!Number.isInteger(durationMinutes) || durationMinutes < 1 || durationMinutes > 1440) throw new Error("Duration must be 1–1440 minutes.");
  const rule = rrule.trim().replace(/^RRULE:/i, "").toUpperCase();
  if (!rule.startsWith("FREQ=")) throw new Error("Enter an RFC 5545 rule beginning with FREQ=.");
  return { rrule: rule, startLocal: startLocal.length === 16 ? `${startLocal}:00` : startLocal, durationMinutes };
}

export function scheduledWindow(start: string, end: string) {
  const from = new Date(start), to = new Date(end);
  if (!start || !end || !Number.isFinite(+from) || !Number.isFinite(+to) || +from <= Date.now() || +to <= +from || +to - +from > 86400000) throw new Error("Choose a future start and an end within 24 hours of it.");
  return { scheduledStartAt: from.toISOString(), scheduledEndAt: to.toISOString() };
}

export function occurrenceTimestamp(value: string): string {
  if (!/T.*(?:Z|[+-]\d{2}:\d{2})$/i.test(value) || !Number.isFinite(Date.parse(value)) || Date.parse(value) <= Date.now()) throw new Error("Enter the original future occurrence as an ISO timestamp with Z or a timezone offset.");
  return new Date(value).toISOString();
}

export const unstarted = (status: ConferenceStatus) => ["CREATED", "PREPARING", "WAITING"].includes(status);
export const joinable = (status: ConferenceStatus) => ["CREATED", "PREPARING", "WAITING", "LIVE"].includes(status);
export function conferencePermission(space: MeetingSpaceResponse, action: "start" | "end" | "lock" | "configure_live") {
  if (space.status !== "ACTIVE" || space.view !== "FULL") return false;
  if (space.viewerRole === "OWNER" || space.viewerRole === "HOST") return true;
  if (space.viewerRole !== "CO_HOST") return false;
  if (action === "lock" || action === "configure_live") return true;
  return space.rolePolicy?.overrides?.CO_HOST?.[`conference.${action}`] ?? action === "start";
}
export function grantableRoles(role?: Role): Role[] {
  const roles: Role[] = ["CO_HOST", "MODERATOR", "PRESENTER", "PARTICIPANT", "VIEWER"];
  return role === "OWNER" ? ["HOST", ...roles] : role === "HOST" ? roles : [];
}
