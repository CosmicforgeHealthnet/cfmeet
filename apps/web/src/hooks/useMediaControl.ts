import { useMutation } from "@tanstack/react-query";
import { meetingService } from "@/services/meeting.service";

export function useMediaControl() {
  const startScreenShare = useMutation({
    mutationFn: (meetingId: string) =>
      meetingService.startScreenShare(meetingId),
    onError: (error) => {
      console.error("Failed to start screen share:", error);
    },
  });

  const stopScreenShare = useMutation({
    mutationFn: (meetingId: string) =>
      meetingService.stopScreenShare(meetingId),
    onError: (error) => {
      console.error("Failed to stop screen share:", error);
    },
  });

  const updateAudio = useMutation({
    mutationFn: ({
      participantId,
      isMuted,
    }: {
      participantId: string;
      isMuted: boolean;
    }) => meetingService.updateAudioState(participantId, isMuted),
    onError: (error) => {
      // Fail silently - LiveKit handles the actual audio state
      console.error("Failed to sync audio state:", error);
    },
  });

  const updateVideo = useMutation({
    mutationFn: ({
      participantId,
      isVideoOn,
    }: {
      participantId: string;
      isVideoOn: boolean;
    }) => meetingService.updateVideoState(participantId, isVideoOn),
    onError: (error) => {
      // Fail silently - LiveKit handles the actual video state
      console.error("Failed to sync video state:", error);
    },
  });

  return {
    startScreenShare,
    stopScreenShare,
    updateAudio,
    updateVideo,
  };
}
