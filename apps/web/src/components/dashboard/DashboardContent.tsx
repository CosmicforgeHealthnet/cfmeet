"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ScrollArea, ScrollBar } from "@/components/ui/scroll-area";
import { Video, Copy, Check, Plus, Loader2 } from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { useCreateInstantMeeting, useSpaces } from "@/hooks/v2/useSpaces";
import type { MeetingSpaceResponse } from "@/types/v2";
import { formatDateForDisplay } from "@/utils/timezone";
import { getErrorMessage } from "@/utils/error";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";

export default function DashboardContent() {
  const router = useRouter();
  const createInstantMeeting = useCreateInstantMeeting();
  const [instantMeetingError, setInstantMeetingError] = useState("");
  const { data: spacesData, isLoading, error: spacesError } = useSpaces();
  const spaces = (spacesData?.items ?? []).filter((space) => space.status === "ACTIVE");

  const handleInstantMeeting = async () => {
    if (createInstantMeeting.isPending) return;

    setInstantMeetingError("");
    try {
      const { meetingSpace } = await createInstantMeeting.mutateAsync({});
      router.push(`/${encodeURIComponent(meetingSpace.meetingCode)}`);
    } catch (error) {
      setInstantMeetingError(
        getErrorMessage(error, "Could not start an instant meeting."),
      );
    }
  };

  return (
    <div className="mx-auto max-w-6xl px-4 py-6 sm:px-6 sm:py-8 md:px-10 md:py-10 font-inter overflow-hidden">
      <div className="mt-8 grid gap-4 sm:mt-14 sm:grid-cols-2 md:mt-20 md:gap-6">
        <Link
          href="/dashboard/meetings/new"
          className="group flex min-h-[220px] w-full min-w-0 flex-col items-center justify-center gap-4 rounded-2xl border border-border bg-card p-5 text-center transition-all duration-200 hover:border-primary/60 hover:bg-muted/30"
        >
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary/10 shadow-inner sm:h-20 sm:w-20">
            <Video className="h-7 w-7 text-primary sm:h-8 sm:w-8" />
          </div>
          <div className="space-y-2">
            <span className="block text-center text-lg font-semibold tracking-tight text-foreground sm:text-xl">Create meeting space</span>
            <span className="block max-w-xs text-center text-xs leading-relaxed text-muted-foreground sm:text-sm">
              Set up reusable meeting details, access policies, members, and recurrence.
            </span>
          </div>
        </Link>

        <button
          type="button"
          onClick={() => void handleInstantMeeting()}
          disabled={createInstantMeeting.isPending}
          aria-busy={createInstantMeeting.isPending}
          className="group flex min-h-[220px] w-full min-w-0 flex-col items-center justify-center gap-4 rounded-2xl border border-border bg-card p-5 text-center transition-all duration-200 hover:border-primary/60 hover:bg-muted/30 disabled:cursor-wait disabled:opacity-70"
        >
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-sidebar shadow-sm sm:h-20 sm:w-20">
            {createInstantMeeting.isPending ? (
              <Loader2 className="h-7 w-7 animate-spin text-primary sm:h-8 sm:w-8" />
            ) : (
              <Plus className="h-7 w-7 text-primary sm:h-8 sm:w-8" />
            )}
          </div>
          <div className="space-y-2">
            <span className="block text-center text-lg font-semibold tracking-tight text-foreground sm:text-xl">
              {createInstantMeeting.isPending ? "Starting meeting…" : "Instant Meeting"}
            </span>
            <span className="block max-w-xs text-center text-xs leading-relaxed text-muted-foreground sm:text-sm">
              Start a live meeting now and invite others with its meeting link.
            </span>
          </div>
        </button>
      </div>

      {instantMeetingError && (
        <p role="alert" className="mt-4 text-center text-sm text-destructive">
          {instantMeetingError}
        </p>
      )}

      {/* Join input */}
      <div className="mt-6 flex justify-center px-2 sm:mt-8 md:mt-10">
        <JoinMeetingInput
          className="mx-auto w-full max-w-[320px] gap-2 sm:max-w-[380px] sm:gap-3"
          inputClassName="h-10 sm:h-11 border-[var(--primary)] focus:ring-[var(--primary)4D]"
          buttonClassName="h-10 sm:h-11 px-5 sm:px-8"
        />
      </div>

      {/* Meeting Spaces section */}
      <div className="mt-10 sm:mt-14 md:mt-20 px-0 sm:px-4 md:px-20">
        <h3 className="font-semibold mb-4 sm:mb-6 text-base sm:text-lg">
          Your Meeting Spaces
        </h3>

        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <Spinner />
          </div>
        ) : spacesError ? <p role="alert" className="text-sm text-destructive">{getErrorMessage(spacesError, "Could not load meeting spaces.")}</p> : spaces.length === 0 ? (
          <p className="text-sm text-muted-foreground">No meeting spaces yet.</p>
        ) : (
          <div className="flex items-start gap-3 sm:gap-4">
            {/* STATIC ACCENT BAR */}
            <Link
              href="/dashboard/meetings/new"
              aria-label="Create meeting space"
              className="w-[36px] sm:w-[45px] h-[100px] sm:h-[118px] bg-[var(--primary)4D] rounded-sm flex items-center justify-center shrink-0"
            >
              <div className="w-6 h-6 flex items-center justify-center">
                <Plus className="w-4 h-4 text-primary" />
              </div>
            </Link>

            {/* SCROLLABLE CARDS */}
            <ScrollArea className="w-full whitespace-nowrap">
              <div className="flex gap-3 sm:gap-4 pb-4">
                {spaces.map((space) => (
                  <SpaceCard key={space.id} space={space} />
                ))}
              </div>
              <ScrollBar orientation="horizontal" />
            </ScrollArea>
          </div>
        )}
      </div>
    </div>
  );
}


function SpaceCard({ space }: { space: MeetingSpaceResponse }) {
  const router = useRouter();
  const [copied, setCopied] = useState(false);

  const joinUrl = typeof window !== "undefined"
    ? `${window.location.origin}/${space.meetingCode}`
    : "";

  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    navigator.clipboard.writeText(joinUrl);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleClick = () => {
    router.push(`/dashboard/meetings/${space.id}`);
  };

  return (
    <div
      className="w-[200px] sm:w-[230px] md:w-[258px] h-[100px] sm:h-[110px] md:h-[118px] bg-card rounded-[10px] p-3 sm:p-4 shadow-sm border border-border shrink-0 flex flex-col justify-between cursor-pointer hover:border-primary/50 transition-colors"
      onClick={handleClick}
    >
      <div className="flex justify-between items-start">
        <div className="flex items-center gap-2 min-w-0 flex-1">
          <h4 className="text-primary font-semibold text-[13px] sm:text-[15px] md:text-[16px] leading-none truncate">
            {space.title || "Untitled Meeting"}
          </h4>
        </div>
        <button
          onClick={handleCopy}
          className="p-1 hover:bg-muted rounded transition-colors shrink-0 ml-1"
          title="Copy meeting link"
        >
          {copied ? (
            <Check className="w-4 h-4 text-green-500" />
          ) : (
            <Copy className="w-4 h-4 text-muted-foreground" />
          )}
        </button>
      </div>

      <div className="space-y-1">
        <p className="text-muted-foreground font-normal text-[11px] sm:text-[12px] leading-none">
          {space.meetingType?.replace("_", " ")}
        </p>
        {space.createdAt && (
          <p className="text-muted-foreground font-normal text-[11px] sm:text-[12px] leading-none">
            {formatDateForDisplay(space.createdAt)}
          </p>
        )}
      </div>

      <div className="flex justify-between items-end">
        <span className="text-muted-foreground font-normal text-[11px] sm:text-[12px] leading-none">
          {space.meetingCode}
        </span>
      </div>
    </div>
  );
}
