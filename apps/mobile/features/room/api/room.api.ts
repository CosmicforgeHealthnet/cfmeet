import { meetingsApi } from "@/features/meetings/api/meetings.api";

export const roomApi = {
  getParticipants: meetingsApi.getParticipants,
  getWaitingParticipants: meetingsApi.getWaitingParticipants,
  admitParticipant: meetingsApi.admitParticipant,
  denyParticipant: meetingsApi.denyParticipant,
  getChatMessages: meetingsApi.getChatMessages,
  sendChatMessage: meetingsApi.sendChatMessage,
  updateAudioState: meetingsApi.updateAudioState,
  updateVideoState: meetingsApi.updateVideoState,
  startScreenShare: meetingsApi.startScreenShare,
  stopScreenShare: meetingsApi.stopScreenShare,
  endMeeting: meetingsApi.endMeeting,
};
