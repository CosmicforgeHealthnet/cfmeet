/**
 * V2 Artifacts Service
 * Handles recordings, transcription, consent, and intelligence
 */

import apiV2 from "./api";
import type {
  // Recordings
  StartRecordingRequest,
  RecordingResponse,
  RecordingListResponse,
  ArtifactView,
  ArtifactListQuery,
  ArtifactListResponse,
  ArtifactAccess,
  AttendanceReport,
  GenerateReportRequest,
  // Consent
  ConsentRequirementsResponse,
  RecordConsentRequest,
  WithdrawalResponse,
  ConsentKind,
  // Transcription
  StartTranscriptionRequest,
  TranscriptionJobView,
  TranscriptionJobListResponse,
  TranscriptQuery,
  TranscriptView,
  TranslateRequest,
  TranslationResponse,
  // Intelligence
  IntelligenceRequest,
} from "@/types/v2";

export const artifactsService = {
  // -------------------------------------------------------------------------
  // Recordings
  // -------------------------------------------------------------------------

  /**
   * Start recording
   */
  startRecording: async (conferenceId: string, data?: StartRecordingRequest) => {
    const response = await apiV2.post<RecordingResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/recordings`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * List recordings for a conference
   */
  listRecordings: async (conferenceId: string) => {
    const response = await apiV2.get<RecordingListResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/recordings`,
    );
    return response.data;
  },

  /**
   * Get a single recording
   */
  getRecording: async (conferenceId: string, recordingId: string) => {
    const response = await apiV2.get<ArtifactView>(
      `/conferences/${encodeURIComponent(conferenceId)}/recordings/${encodeURIComponent(recordingId)}`,
    );
    return response.data;
  },

  /**
   * Stop recording
   */
  stopRecording: async (conferenceId: string, recordingId: string) => {
    const response = await apiV2.post<RecordingResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/recordings/${encodeURIComponent(recordingId)}/stop`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Artifacts
  // -------------------------------------------------------------------------

  /**
   * List artifacts for a conference
   */
  listArtifacts: async (conferenceId: string, query?: ArtifactListQuery) => {
    const response = await apiV2.get<ArtifactListResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/artifacts`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Get a single artifact
   */
  getArtifact: async (conferenceId: string, artifactId: string) => {
    const response = await apiV2.get<ArtifactView>(
      `/conferences/${encodeURIComponent(conferenceId)}/artifacts/${encodeURIComponent(artifactId)}`,
    );
    return response.data;
  },

  /**
   * Get signed access URL for an artifact
   */
  getArtifactAccess: async (conferenceId: string, artifactId: string) => {
    const response = await apiV2.post<ArtifactAccess>(
      `/conferences/${encodeURIComponent(conferenceId)}/artifacts/${encodeURIComponent(artifactId)}/access`,
    );
    return response.data;
  },

  /**
   * Delete an artifact
   */
  deleteArtifact: async (conferenceId: string, artifactId: string) => {
    const response = await apiV2.delete<ArtifactView>(
      `/conferences/${encodeURIComponent(conferenceId)}/artifacts/${encodeURIComponent(artifactId)}`,
    );
    return response.data;
  },

  /**
   * Get attendance report
   */
  getAttendance: async (conferenceId: string) => {
    const response = await apiV2.get<AttendanceReport>(
      `/conferences/${encodeURIComponent(conferenceId)}/attendance`,
    );
    return response.data;
  },

  /**
   * Generate a report (attendance, poll, chat export)
   */
  generateReport: async (conferenceId: string, data: GenerateReportRequest) => {
    const response = await apiV2.post<ArtifactView>(
      `/conferences/${encodeURIComponent(conferenceId)}/artifacts/reports`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Consent
  // -------------------------------------------------------------------------

  /**
   * Get consent requirements
   */
  getConsentRequirements: async (conferenceId: string) => {
    const response = await apiV2.get<ConsentRequirementsResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/consents/requirements`,
    );
    return response.data;
  },

  /**
   * Record consent decisions
   */
  recordConsents: async (conferenceId: string, data: RecordConsentRequest) => {
    const response = await apiV2.post<ConsentRequirementsResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/consents`,
      data,
    );
    return response.data;
  },

  /**
   * Withdraw a consent
   */
  withdrawConsent: async (conferenceId: string, consentType: ConsentKind) => {
    const response = await apiV2.post<WithdrawalResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/consents/${encodeURIComponent(consentType)}/withdraw`,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Transcription
  // -------------------------------------------------------------------------

  /**
   * Start transcription
   */
  startTranscription: async (
    conferenceId: string,
    data?: StartTranscriptionRequest,
  ) => {
    const response = await apiV2.post<TranscriptionJobView>(
      `/conferences/${encodeURIComponent(conferenceId)}/transcription`,
      data ?? {},
    );
    return response.data;
  },

  /**
   * Stop transcription
   */
  stopTranscription: async (conferenceId: string, jobId: string) => {
    const response = await apiV2.post<TranscriptionJobView>(
      `/conferences/${encodeURIComponent(conferenceId)}/transcription/${encodeURIComponent(jobId)}/stop`,
    );
    return response.data;
  },

  /**
   * List transcripts
   */
  listTranscripts: async (conferenceId: string) => {
    const response = await apiV2.get<TranscriptionJobListResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/transcripts`,
    );
    return response.data;
  },

  /**
   * Get transcript content
   */
  getTranscript: async (
    conferenceId: string,
    artifactId: string,
    query?: TranscriptQuery,
  ) => {
    const response = await apiV2.get<TranscriptView>(
      `/conferences/${encodeURIComponent(conferenceId)}/transcripts/${encodeURIComponent(artifactId)}`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Translate a transcript
   */
  translateTranscript: async (
    conferenceId: string,
    artifactId: string,
    data: TranslateRequest,
  ) => {
    const response = await apiV2.post<TranslationResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/transcripts/${encodeURIComponent(artifactId)}/translations`,
      data,
    );
    return response.data;
  },

  // -------------------------------------------------------------------------
  // Intelligence
  // -------------------------------------------------------------------------

  /**
   * Request an intelligence output (summary, action items, etc.)
   */
  requestIntelligence: async (conferenceId: string, data: IntelligenceRequest) => {
    const response = await apiV2.post<ArtifactView>(
      `/conferences/${encodeURIComponent(conferenceId)}/intelligence/requests`,
      data,
    );
    return response.data;
  },
};
