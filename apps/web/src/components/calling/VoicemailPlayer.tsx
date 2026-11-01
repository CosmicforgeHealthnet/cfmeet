"use client";

/**
 * VoicemailPlayer - Audio playback for voicemail messages.
 */

import { useState, useRef, useEffect } from "react";
import { Play, Pause, X, Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { useVoicemailMessage, useVoicemailMediaAccess } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";

interface VoicemailPlayerProps {
  messageId: string;
  onClose?: () => void;
  className?: string;
}

export default function VoicemailPlayer({
  messageId,
  onClose,
  className,
}: VoicemailPlayerProps) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [mediaUrl, setMediaUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const message = useVoicemailMessage(messageId);
  const getMediaAccess = useVoicemailMediaAccess();

  // Fetch media URL on mount
  useEffect(() => {
    const fetchMedia = async () => {
      try {
        const result = await getMediaAccess.mutateAsync(messageId);
        setMediaUrl(result.url);
      } catch (err) {
        setError(callingErrorMessage(err));
      }
    };
    fetchMedia();
  }, [messageId]);

  // Update time and duration
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;

    const handleTimeUpdate = () => setCurrentTime(audio.currentTime);
    const handleDurationChange = () => setDuration(audio.duration);
    const handleEnded = () => setIsPlaying(false);

    audio.addEventListener("timeupdate", handleTimeUpdate);
    audio.addEventListener("durationchange", handleDurationChange);
    audio.addEventListener("ended", handleEnded);

    return () => {
      audio.removeEventListener("timeupdate", handleTimeUpdate);
      audio.removeEventListener("durationchange", handleDurationChange);
      audio.removeEventListener("ended", handleEnded);
    };
  }, [mediaUrl]);

  const togglePlay = () => {
    const audio = audioRef.current;
    if (!audio) return;

    if (isPlaying) {
      audio.pause();
    } else {
      void audio.play();
    }
    setIsPlaying(!isPlaying);
  };

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    const audio = audioRef.current;
    if (!audio) return;
    const time = Number(e.target.value);
    audio.currentTime = time;
    setCurrentTime(time);
  };

  const formatTime = (seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}:${secs.toString().padStart(2, "0")}`;
  };

  const messageData = message.data;

  return (
    <div className={`rounded-xl border border-border bg-card p-4 ${className ?? ""}`}>
      {/* Header */}
      <div className="mb-3 flex items-center justify-between">
        <div>
          <p className="font-medium">
            {messageData?.callerDisplayName || messageData?.callerNumber || "Unknown Caller"}
          </p>
          <p className="text-sm text-muted-foreground">
            {messageData && new Date(messageData.createdAt).toLocaleString()}
          </p>
        </div>
        {onClose && (
          <Button size="icon" variant="ghost" onClick={onClose} aria-label="Close">
            <X className="size-4" />
          </Button>
        )}
      </div>

      {error && (
        <p className="mb-3 rounded-lg border border-destructive/30 bg-destructive/5 p-2 text-sm text-destructive">
          {error}
        </p>
      )}

      {/* Audio Player */}
      {getMediaAccess.isPending ? (
        <div className="flex items-center justify-center py-4">
          <Spinner />
          <span className="ml-2 text-sm text-muted-foreground">Loading audio...</span>
        </div>
      ) : mediaUrl ? (
        <div className="space-y-3">
          <audio ref={audioRef} src={mediaUrl} preload="metadata" />

          {/* Controls */}
          <div className="flex items-center gap-3">
            <Button
              size="icon"
              variant="outline"
              onClick={togglePlay}
              aria-label={isPlaying ? "Pause" : "Play"}
            >
              {isPlaying ? <Pause className="size-4" /> : <Play className="size-4" />}
            </Button>

            {/* Seek bar */}
            <div className="flex flex-1 items-center gap-2">
              <span className="text-xs text-muted-foreground">{formatTime(currentTime)}</span>
              <input
                type="range"
                min={0}
                max={duration || 0}
                value={currentTime}
                onChange={handleSeek}
                className="h-2 flex-1 cursor-pointer appearance-none rounded-full bg-muted"
              />
              <span className="text-xs text-muted-foreground">{formatTime(duration)}</span>
            </div>

            {/* Download */}
            <Button
              size="icon"
              variant="ghost"
              asChild
              aria-label="Download"
            >
              <a href={mediaUrl} download={`voicemail-${messageId}.mp3`}>
                <Download className="size-4" />
              </a>
            </Button>
          </div>
        </div>
      ) : null}

      {/* Transcription */}
      {messageData?.transcription && (
        <div className="mt-4 border-t border-border pt-4">
          <p className="mb-2 text-xs font-medium text-muted-foreground">
            Transcription
            {messageData.transcriptionConfidence && (
              <span className="ml-2 text-muted-foreground/70">
                ({Math.round(messageData.transcriptionConfidence * 100)}% confidence)
              </span>
            )}
          </p>
          <p className="text-sm">{messageData.transcription}</p>
        </div>
      )}
    </div>
  );
}
