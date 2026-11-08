/**
 * V2 Conferences Service
 * Handles conference lifecycle management
 */

import apiV2 from "./api";
import type {
  ConferenceResponse,
  ConferenceListResponse,
  ConferenceActionResponse,
  CreateConferenceRequest,
  RescheduleConferenceRequest,
  ListConferencesQuery,
  JoinConferenceRequestV2,
  JoinConferenceResponseV2,
  LeaveConferenceRequest,
  LeaveConferenceResponse,
  PreJoinResponse,
} from "@/types/v2";
import type { AgendaQuery, AgendaResponse } from "@/types/generated/meeting-service";

export const conferencesService = {
  // -------------------------------------------------------------------------
  // Conference CRUD
  // -------------------------------------------------------------------------

  /**
   * List conferences for a meeting space
   */
  list: async (spaceId: string, query?: ListConferencesQuery) => {
    const response = await apiV2.get<ConferenceListResponse>(
      `/meeting-spaces/${encodeURIComponent(spaceId)}/conferences`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Get a single conference by ID
   */
  get: async (id: string) => {
    const response = await apiV2.get<ConferenceResponse>(`/conferences/${encodeURIComponent(id)}`);
    return response.data;
  },

  /**
   * Create a new conference in a meeting space. `idempotencyKey` (one UUID per user action)
   * makes a retried submit return the same occurrence (SEC-014).
   */
  create: async (spaceId: string, data?: CreateConferenceRequest, idempotencyKey?: string) => {
    const response = await apiV2.post<ConferenceResponse>(
      `/meeting-spaces/${encodeURIComponent(spaceId)}/conferences`,
      data ?? {},
      idempotencyKey ? { headers: { "Idempotency-Key": idempotencyKey } } : undefined,
    );
    return response.data;
  },

  /**
   * The caller's agenda across meeting spaces: `GET /me/conferences` (one page; follow
   * `nextCursor` with the same filters). `organizationId` scopes it to one organization.
   */
  agenda: async (query: AgendaQuery) => {
    // Empty filters are omitted; the cursor is passed back unchanged (opaque).
    const params = Object.fromEntries(Object.entries(query).filter(([, value]) => value !== undefined && value !== null && value !== ""));
    const response = await apiV2.get<AgendaResponse>("/me/conferences", { params });
    return response.data;
  },

  /** One occurrence as an iCalendar file (`text/calendar`, no-store). */
  calendarFile: async (id: string) => {
    const response = await apiV2.get<Blob>(`/conferences/${encodeURIComponent(id)}/calendar.ics`, { responseType: "blob", headers: { Accept: "text/calendar" } });
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Conference Lifecycle
  // -------------------------------------------------------------------------

  /**
   * Prepare conference (CREATED → PREPARING → WAITING)
   */
  prepare: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/prepare`,
      {},
    );
    return response.data;
  },

  /**
   * Start conference (→ LIVE)
   */
  start: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/start`,
      {},
    );
    return response.data;
  },

  /**
   * Lock conference (prevent new joins)
   */
  lock: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/lock`,
      {},
    );
    return response.data;
  },

  /**
   * Unlock conference (allow new joins)
   */
  unlock: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/unlock`,
      {},
    );
    return response.data;
  },

  /**
   * End conference (→ ENDING → ENDED)
   */
  end: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/end`,
      {},
    );
    return response.data;
  },

  /**
   * Cancel an unstarted conference
   */
  cancel: async (id: string) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/cancel`,
      {},
    );
    return response.data;
  },

  /**
   * Reschedule a conference
   */
  reschedule: async (id: string, data: RescheduleConferenceRequest) => {
    const response = await apiV2.post<ConferenceActionResponse>(
      `/conferences/${encodeURIComponent(id)}/reschedule`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Join / Leave
  // -------------------------------------------------------------------------

  /**
   * Get pre-join information (requirements, consent, etc.)
   */
  preJoin: async (id: string) => {
    const response = await apiV2.get<PreJoinResponse>(`/conferences/${encodeURIComponent(id)}/pre-join`);
    return response.data;
  },

  /**
   * Join a conference
   */
  join: async (id: string, data?: JoinConferenceRequestV2) => {
    const response = await apiV2.post<JoinConferenceResponseV2>(
      `/conferences/${encodeURIComponent(id)}/join`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Leave a conference
   */
  leave: async (id: string, data?: LeaveConferenceRequest) => {
    const response = await apiV2.post<LeaveConferenceResponse>(
      `/conferences/${encodeURIComponent(id)}/leave`,
      data ?? {},
    );
    return response.data;
  },
};
