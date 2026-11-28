/**
 * Communities (ADR 0073): owned by the Messaging service and reached through the Meeting
 * Service's Messaging relay (`/api/v2/messaging/relay/v1/messaging/communities/...`), acting as
 * the signed-in user's own principal. The server enforces every role and privacy rule.
 */
import { relay } from "./messaging.service";
import { createCommunitiesApi } from "@/lib/communities-api";

export const communitiesService = createCommunitiesApi(relay);
export type { CreateCommunity, UpdateCommunity } from "@/lib/communities-api";
