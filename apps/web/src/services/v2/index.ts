/**
 * V2 API Services
 * Central export for all V2 API services
 */

// API Clients
export { default as apiV2, publicApiV2 } from "./api";

// Services
export { spacesService } from "./spaces.service";
export { conferencesService } from "./conferences.service";
export { membersService } from "./members.service";
export { invitationsService } from "./invitations.service";
export { participantsService } from "./participants.service";
export { collaborationService } from "./collaboration.service";
export { realtimeService } from "./realtime.service";
export { breakoutsService } from "./breakouts.service";
export { engagementService } from "./engagement.service";
export { artifactsService } from "./artifacts.service";
export { qualityService } from "./quality.service";
export { profilesService } from "./profiles.service";
export { organizationsService } from "./organizations.service";

// Unified V2 Service Object
export const v2 = {
  spaces: () => import("./spaces.service").then((m) => m.spacesService),
  conferences: () => import("./conferences.service").then((m) => m.conferencesService),
  members: () => import("./members.service").then((m) => m.membersService),
  invitations: () => import("./invitations.service").then((m) => m.invitationsService),
  participants: () => import("./participants.service").then((m) => m.participantsService),
  collaboration: () => import("./collaboration.service").then((m) => m.collaborationService),
  realtime: () => import("./realtime.service").then((m) => m.realtimeService),
  breakouts: () => import("./breakouts.service").then((m) => m.breakoutsService),
  engagement: () => import("./engagement.service").then((m) => m.engagementService),
  artifacts: () => import("./artifacts.service").then((m) => m.artifactsService),
  quality: () => import("./quality.service").then((m) => m.qualityService),
  profiles: () => import("./profiles.service").then((m) => m.profilesService),
  organizations: () => import("./organizations.service").then((m) => m.organizationsService),
};
