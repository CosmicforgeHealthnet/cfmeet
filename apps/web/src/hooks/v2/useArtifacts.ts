/**
 * V2 Artifacts Hooks
 * React Query hooks for recordings, transcription, consent, and intelligence
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { artifactsService } from "@/services/v2";
import type {
  StartRecordingRequest,
  ArtifactListQuery,
  GenerateReportRequest,
  RecordConsentRequest,
  ConsentKind,
  StartTranscriptionRequest,
  TranscriptQuery,
  TranslateRequest,
  IntelligenceRequest,
} from "@/types/v2";

// Query Keys
export const artifactKeys = {
  all: ["v2", "artifacts"] as const,
  recordings: (conferenceId: string) =>
    [...artifactKeys.all, "recordings", conferenceId] as const,
  recording: (conferenceId: string, recordingId: string) =>
    [...artifactKeys.recordings(conferenceId), recordingId] as const,
  artifacts: (conferenceId: string, query?: ArtifactListQuery) =>
    [...artifactKeys.all, "list", conferenceId, query] as const,
  artifact: (conferenceId: string, artifactId: string) =>
    [...artifactKeys.all, "detail", conferenceId, artifactId] as const,
  attendance: (conferenceId: string) =>
    [...artifactKeys.all, "attendance", conferenceId] as const,
  consents: (conferenceId: string) =>
    [...artifactKeys.all, "consents", conferenceId] as const,
  transcripts: (conferenceId: string) =>
    [...artifactKeys.all, "transcripts", conferenceId] as const,
  transcript: (conferenceId: string, artifactId: string, query?: TranscriptQuery) =>
    [...artifactKeys.transcripts(conferenceId), artifactId, query] as const,
  quality: (conferenceId: string) =>
    [...artifactKeys.all, "quality", conferenceId] as const,
};

// -------------------------------------------------------------------------
// Recording Queries
// -------------------------------------------------------------------------

/**
 * List recordings for a conference
 */
export function useRecordings(conferenceId: string) {
  return useQuery({
    queryKey: artifactKeys.recordings(conferenceId),
    queryFn: () => artifactsService.listRecordings(conferenceId),
    enabled: !!conferenceId,
    refetchInterval: 5000,
  });
}

/**
 * Get a single recording
 */
export function useRecording(conferenceId: string, recordingId: string) {
  return useQuery({
    queryKey: artifactKeys.recording(conferenceId, recordingId),
    queryFn: () => artifactsService.getRecording(conferenceId, recordingId),
    enabled: !!conferenceId && !!recordingId,
  });
}

// -------------------------------------------------------------------------
// Recording Mutations
// -------------------------------------------------------------------------

/**
 * Start recording
 */
export function useStartRecording(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data?: StartRecordingRequest) =>
      artifactsService.startRecording(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.recordings(conferenceId),
      });
    },
  });
}

/**
 * Stop recording
 */
export function useStopRecording(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (recordingId: string) =>
      artifactsService.stopRecording(conferenceId, recordingId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.recordings(conferenceId),
      });
    },
  });
}

// -------------------------------------------------------------------------
// Artifact Queries
// -------------------------------------------------------------------------

/**
 * List artifacts for a conference
 */
export function useArtifacts(conferenceId: string, query?: ArtifactListQuery) {
  return useQuery({
    queryKey: artifactKeys.artifacts(conferenceId, query),
    queryFn: () => artifactsService.listArtifacts(conferenceId, query),
    enabled: !!conferenceId,
  });
}

/**
 * Get a single artifact
 */
export function useArtifact(conferenceId: string, artifactId: string) {
  return useQuery({
    queryKey: artifactKeys.artifact(conferenceId, artifactId),
    queryFn: () => artifactsService.getArtifact(conferenceId, artifactId),
    enabled: !!conferenceId && !!artifactId,
  });
}

/**
 * Get attendance report
 */
export function useAttendance(conferenceId: string) {
  return useQuery({
    queryKey: artifactKeys.attendance(conferenceId),
    queryFn: () => artifactsService.getAttendance(conferenceId),
    enabled: !!conferenceId,
  });
}

// -------------------------------------------------------------------------
// Artifact Mutations
// -------------------------------------------------------------------------

/**
 * Get signed access URL for an artifact
 */
export function useArtifactAccess(conferenceId: string) {
  return useMutation({
    mutationFn: (artifactId: string) =>
      artifactsService.getArtifactAccess(conferenceId, artifactId),
  });
}

/**
 * Delete an artifact
 */
export function useDeleteArtifact(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (artifactId: string) =>
      artifactsService.deleteArtifact(conferenceId, artifactId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.artifacts(conferenceId),
      });
    },
  });
}

/**
 * Generate a report
 */
export function useGenerateReport(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: GenerateReportRequest) =>
      artifactsService.generateReport(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.artifacts(conferenceId),
      });
    },
  });
}

// -------------------------------------------------------------------------
// Consent Queries
// -------------------------------------------------------------------------

/**
 * Get consent requirements
 */
export function useConsentRequirements(conferenceId: string) {
  return useQuery({
    queryKey: artifactKeys.consents(conferenceId),
    queryFn: () => artifactsService.getConsentRequirements(conferenceId),
    enabled: !!conferenceId,
  });
}

// -------------------------------------------------------------------------
// Consent Mutations
// -------------------------------------------------------------------------

/**
 * Record consent decisions
 */
export function useRecordConsents(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: RecordConsentRequest) =>
      artifactsService.recordConsents(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.consents(conferenceId),
      });
    },
  });
}

/**
 * Withdraw a consent
 */
export function useWithdrawConsent(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (consentType: ConsentKind) =>
      artifactsService.withdrawConsent(conferenceId, consentType),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.consents(conferenceId),
      });
    },
  });
}

// -------------------------------------------------------------------------
// Transcription Queries
// -------------------------------------------------------------------------

/**
 * List transcripts
 */
export function useTranscripts(conferenceId: string) {
  return useQuery({
    queryKey: artifactKeys.transcripts(conferenceId),
    queryFn: () => artifactsService.listTranscripts(conferenceId),
    enabled: !!conferenceId,
  });
}

/**
 * Get transcript content
 */
export function useTranscript(
  conferenceId: string,
  artifactId: string,
  query?: TranscriptQuery,
) {
  return useQuery({
    queryKey: artifactKeys.transcript(conferenceId, artifactId, query),
    queryFn: () => artifactsService.getTranscript(conferenceId, artifactId, query),
    enabled: !!conferenceId && !!artifactId,
  });
}

// -------------------------------------------------------------------------
// Transcription Mutations
// -------------------------------------------------------------------------

/**
 * Start transcription
 */
export function useStartTranscription(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data?: StartTranscriptionRequest) =>
      artifactsService.startTranscription(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.transcripts(conferenceId),
      });
    },
  });
}

/**
 * Stop transcription
 */
export function useStopTranscription(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (jobId: string) =>
      artifactsService.stopTranscription(conferenceId, jobId),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.transcripts(conferenceId),
      });
    },
  });
}

/**
 * Translate a transcript
 */
export function useTranslateTranscript(conferenceId: string) {
  return useMutation({
    mutationFn: ({
      artifactId,
      data,
    }: {
      artifactId: string;
      data: TranslateRequest;
    }) => artifactsService.translateTranscript(conferenceId, artifactId, data),
  });
}

// -------------------------------------------------------------------------
// Intelligence Mutations
// -------------------------------------------------------------------------

/**
 * Request an intelligence output
 */
export function useRequestIntelligence(conferenceId: string) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (data: IntelligenceRequest) =>
      artifactsService.requestIntelligence(conferenceId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: artifactKeys.artifacts(conferenceId),
      });
    },
  });
}
