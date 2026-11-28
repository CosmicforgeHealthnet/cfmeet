/**
 * The "video-edit" status processor: renders a VideoEditor edit into ≤ 60 s MP4 parts in the
 * background, after the editor has closed. The job persists the source file + the edit (JSON),
 * so after a reload the render simply runs again.
 *
 * Registered once at app start (StatusQueueRoot). The heavy video module is loaded only when a
 * job actually needs it.
 *
 * If this browser turns out unable to encode (no WebCodecs encoder, undecodable track), the
 * processor falls back to uploading the original once and posting the edit's parts as time
 * ranges of it — what the composer did before the queue existed.
 */
import { LIMITS } from "@/services/v2/social.service";
import type { VideoEdit } from "@/lib/media-editor/video-edit";
import type { ProcessorResult, StatusProcessor } from "./types";

export const VIDEO_EDIT_PROCESSOR = "video-edit";

export type VideoEditParams = { edit: VideoEdit; durationMs: number };

export const renderVideoEditForStatus: StatusProcessor = async (input, params, ctx): Promise<ProcessorResult> => {
  const { edit, durationMs } = params as VideoEditParams;
  const video = await import("@/lib/media-editor/video");
  try {
    const blobs = await video.renderVideoEdit(input, edit, {
      signal: ctx.signal,
      onProgress: ({ overall }) => ctx.onProgress(overall),
    });
    return { segments: blobs.map((blob, i) => ({ blob, durationMs: edit.segments[i].endMs - edit.segments[i].startMs })) };
  } catch (e) {
    if (!video.isVideoProcessingUnsupported(e)) throw e;
    if (input.size > LIMITS.VIDEO) throw new Error("Videos can be at most 64 MB.");
    return { source: input, durationMs, ranges: edit.segments.map((s) => ({ startMs: s.startMs, endMs: s.endMs })) };
  }
};
