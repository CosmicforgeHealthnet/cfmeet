import type { AccessPolicy, MeetingSpaceResponse, MeetingType, UpdateMeetingSpaceRequest, WaitingRoomPolicy } from "@/types/v2";

export function spacePermissions(space: MeetingSpaceResponse) {
  const full = space.view === "FULL";
  const owner = full && space.viewerRole === "OWNER";
  return { owner, configure: full && (owner || space.viewerRole === "HOST"), active: space.status === "ACTIVE" };
}

export interface SpaceFormValues {
  title: string;
  description: string;
  timezone: string;
  meetingType: MeetingType;
  accessPolicy: AccessPolicy;
  waitingRoomPolicy: WaitingRoomPolicy;
  passcode: string;
  clearPasscode: boolean;
}

/** Space configuration only: conference dates, owner and caller roles never go in PATCH. */
export function spaceUpdate(space: MeetingSpaceResponse, values: SpaceFormValues): UpdateMeetingSpaceRequest {
  if (!spacePermissions(space).configure || space.status !== "ACTIVE" || space.version === undefined) throw new Error("You cannot edit this meeting space.");
  const title = values.title.trim();
  if (!title || [...title].length > 255) throw new Error("Title must contain 1–255 characters.");
  const passcode = values.passcode.trim();
  if (passcode && ([...passcode].length < 4 || [...passcode].length > 64)) throw new Error("Passcode must contain 4–64 characters.");
  if (values.accessPolicy === "PUBLIC_PASSCODE" && !passcode && (!space.requiresPasscode || values.clearPasscode)) throw new Error("A passcode is required for passcode-protected spaces.");
  if (passcode && values.clearPasscode) throw new Error("Choose a new passcode or remove the existing one.");
  try { new Intl.DateTimeFormat("en", { timeZone: values.timezone }).format(); } catch { throw new Error("Enter a valid IANA timezone, for example Africa/Lagos."); }
  return {
    version: space.version, title, description: values.description.trim(), timezone: values.timezone,
    meetingType: values.meetingType, accessPolicy: values.accessPolicy, waitingRoomPolicy: values.waitingRoomPolicy,
    ...(passcode ? { passcode } : {}), ...(values.clearPasscode ? { clearPasscode: true } : {}),
  };
}

export function meetingIdentifier(value: string): string {
  let identifier = value.trim();
  if (/^https?:\/\//i.test(identifier)) {
    const url = new URL(identifier);
    identifier = url.pathname.split("/").filter(Boolean).at(-1) ?? "";
  }
  if (!identifier || identifier.length > 128 || !/^[A-Za-z0-9-]+$/.test(identifier)) throw new Error("Enter a valid meeting link, code or ID.");
  return identifier;
}
