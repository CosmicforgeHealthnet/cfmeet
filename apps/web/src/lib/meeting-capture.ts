/**
 * Meeting recording and AI features are only offered once the server can actually do them:
 * recording needs the server's recording storage, and transcripts, live captions, translation
 * and AI notes need a transcription / AI provider. Until then their buttons stay hidden.
 *
 * Turn them on by putting these in your .env.local (or the hosting settings):
 *   NEXT_PUBLIC_MEETING_RECORDING=on   recording, recordings list, saved report copies
 *   NEXT_PUBLIC_MEETING_AI=on          transcripts, live captions, translation, AI notes
 */
export const MEETING_RECORDING_READY = process.env.NEXT_PUBLIC_MEETING_RECORDING === "on";
export const MEETING_AI_READY = process.env.NEXT_PUBLIC_MEETING_AI === "on";
