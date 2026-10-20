/**
 * CF Meet V2 API Module (Mobile)
 * Complete V2 API types, services, hooks, and components
 */

// Types
export * from "./types";

// API Services
export {
  meetingSpacesApi,
  conferencesApi,
  participantsApi,
  waitingRoomApi,
  chatApi,
  reactionsApi,
  breakoutsApi,
  pollsApi,
  qnaApi,
  collaborationApi,
} from "./api";

// Components
export * from "./components";

// Hooks
export {
  // Meeting Spaces
  useSpaces,
  useSpace,
  useCreateSpace,
  useArchiveSpace,
  useResolve,
  useCreateInstantMeeting,
  // Conferences
  useConferences,
  useConference,
  useCreateConference,
  usePreJoin,
  useJoinConference,
  useLeaveConference,
  useStartConference,
  useEndConference,
  useLockConference,
  useUnlockConference,
  // Participants
  useParticipants,
  useChangeRole,
  useRemoveParticipant,
  useMuteParticipant,
  // Waiting Room
  useWaitingRoom,
  useAdmitParticipants,
  useDenyParticipants,
  // Chat
  useMessages,
  useSendMessage,
  useDeleteMessage,
  // Reactions & Hand Raise
  useSendReaction,
  useRaiseHand,
  useLowerHand,
  useLowerOthersHand,
  useRaisedHands,
  // Breakouts
  useBreakouts,
  useCreateBreakouts,
  useAssignBreakouts,
  useOpenBreakout,
  useCloseBreakout,
  useCloseAllBreakouts,
  useAnnounceBreakouts,
  useEnterBreakout,
  useLeaveBreakout,
  // Polls
  usePolls,
  usePoll,
  useCreatePoll,
  useOpenPoll,
  useClosePoll,
  useSubmitPollResponse,
  usePollResults,
  // Q&A
  useQuestions,
  useSubmitQuestion,
  useVoteQuestion,
  useUnvoteQuestion,
  useAnswerQuestion,
  useDismissQuestion,
  usePinQuestion,
  useUnpinQuestion,
  // Collaboration
  useCollaborationState,
  useSpotlight,
  useRemoveSpotlight,
  useLockAudio,
  useUnlockAudio,
  useLockVideo,
  useUnlockVideo,
} from "./hooks";
