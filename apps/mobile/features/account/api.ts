/** Account profile and sign-in sessions (Meeting Service v2 `/me/*`). */
import { send } from "@/lib/api/client";
import { ROUTES } from "@/lib/api/routes";
import type {
  AccountProfile,
  AccountSessionListResponse,
  AccountSessionRevokeResponse,
  UpdateAccountProfileRequest,
} from "@/lib/api/generated/meeting-service";

export const accountApi = {
  profile: () => send<AccountProfile>(ROUTES.myProfile, []),
  updateProfile: (data: UpdateAccountProfileRequest) => send<AccountProfile>(ROUTES.updateMyProfile, [], { data }),
  sessions: () => send<AccountSessionListResponse>(ROUTES.mySessions, []),
  revokeSession: (sessionId: string) => send<AccountSessionRevokeResponse>(ROUTES.revokeMySession, [sessionId]),
  revokeAllSessions: () => send<AccountSessionRevokeResponse>(ROUTES.revokeAllMySessions, []),
};
