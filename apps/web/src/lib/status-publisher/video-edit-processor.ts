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
import type { Overlay } from "@/lib/media-editor/overlays";
import type { MusicSettings } from "@/lib/media-editor/audio";
import type { ProcessorResult, StatusProcessor } from "./types";

export const VIDEO_EDIT_PROCESSOR = "video-edit";

export type VideoEditParams = {
  edit: VideoEdit;
  durationMs: number;
  /** Text, stickers, emojis and drawing burned into every frame (positions in the output frame). */
  overlays?: Overlay[];
  /** Music mixed under the original sound; the file itself is `ctx.blobs.music`. Plays from the first part's start. */
  music?: MusicSettings | null;
};

/** The name of the music file in the job's blobs. */
export const MUSIC_BLOB = "music";

export const renderVideoEditForStatus: StatusProcessor = async (input, params, ctx): Promise<ProcessorResult> => {
  const { edit, durationMs, overlays, music } = params as VideoEditParams;
  const video = await import("@/lib/media-editor/video");
  const musicFile = ctx.blobs?.[MUSIC_BLOB];
  try {
    let drawOverlay: ReturnType<typeof import("@/lib/media-editor/video-overlays").overlayFrameDrawer> = null;
    if (overlays?.length) {
      const [{ overlayFrameDrawer }, { loadEditorFonts }] = await Promise.all([import("@/lib/media-editor/video-overlays"), import("@/lib/media-editor/fonts")]);
      await loadEditorFonts();
      drawOverlay = overlayFrameDrawer(overlays);
    }
    const blobs = await video.renderVideoEdit(input, edit, {
      drawOverlay: drawOverlay ? (c, w, h) => drawOverlay!(c, w, h) : null,
      music: music && musicFile ? { file: musicFile, offsetMs: 0, ...music } : null,
      signal: ctx.signal,
      onProgress: ({ overall }) => ctx.onProgress(overall),
    });
    return { segments: blobs.map((blob, i) => ({ blob, durationMs: edit.segments[i].endMs - edit.segments[i].startMs })) };
  } catch (e) {
    if (!video.isVideoProcessingUnsupported(e)) throw e;
    // The original can't carry the overlays or the music: say so rather than post it without them.
    if (overlays?.length || (music && musicFile)) throw new Error("This browser can't add text, drawings or music to videos. Remove them and try again.");
    if (input.size > LIMITS.VIDEO) throw new Error("Videos can be at most 64 MB.");
    return { source: input, durationMs, ranges: edit.segments.map((s) => ({ startMs: s.startMs, endMs: s.endMs })) };
  }
};
