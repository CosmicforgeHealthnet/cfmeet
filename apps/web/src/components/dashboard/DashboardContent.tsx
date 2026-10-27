"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { ScrollArea, ScrollBar } from "@/components/ui/scroll-area";
import { Video, Calendar, Copy, Check, Plus, Loader2 } from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { useSpaces, useCreateInstantMeeting } from "@/hooks/v2/useSpaces";
import type { MeetingSpaceResponse } from "@/types/v2";
import { formatDateForDisplay } from "@/utils/timezone";
import { getErrorMessage } from "@/utils/error";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";
import MeetingSuccessView from "./MeetingSuccessView";

export default function DashboardContent() {
  const router = useRouter();
  const { data: spacesData, isLoading, error: spacesError } = useSpaces();
  const createInstantMeeting = useCreateInstantMeeting();
  const [error, setError] = useState("");
  const [createdMeeting, setCreatedMeeting] = useState<{
    meetingCode: string;
    conferenceId: string;
  } | null>(null);

  const spaces = (spacesData?.items ?? []).filter((space) => space.status === "ACTIVE");

  const handleInstantMeeting = async () => {
    if (createInstantMeeting.isPending) return;

    setError("");
    try {
      // Create instant meeting (space + live conference)
      const instantResponse = await createInstantMeeting.mutateAsync({});
      const { meetingSpace, conference } = instantResponse;

      // Show success view with meeting details
      setCreatedMeeting({
        meetingCode: meetingSpace.meetingCode,
        conferenceId: conference.id,
      });
    } catch (err) {
      setError(getErrorMessage(err, "Failed to create meeting. Please try again."));
    }
  };

  const handleJoinNow = () => {
    if (createdMeeting) {
      router.push(`/${createdMeeting.meetingCode}?conference=${createdMeeting.conferenceId}`);
    }
  };

  const isCreatingInstant = createInstantMeeting.isPending;

  // Show success view after instant meeting creation
  if (createdMeeting) {
    const joinUrl = `${typeof window !== "undefined" ? window.location.origin : ""}/${createdMeeting.meetingCode}`;
    return (
      <MeetingSuccessView
        meetingIdentifier={createdMeeting.meetingCode}
        joinUrl={joinUrl}
        title="Conference started"
        description="Your live conference is ready. Its meeting space holds the reusable settings."
        showJoinNow={true}
        onJoinNow={handleJoinNow}
        showScheduleAnother={false}
        onBack={() => setCreatedMeeting(null)}
      />
    );
  }

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter overflow-hidden">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {/* Top actions */}
      <div className="flex justify-center gap-8 sm:gap-12 md:gap-16 mt-8 sm:mt-14 md:mt-20">
        <div
          className={`flex flex-col items-center gap-2 sm:gap-3 ${isCreatingInstant ? "cursor-wait" : "cursor-pointer"}`}
          onClick={handleInstantMeeting}
        >
          <div className="w-16 h-16 sm:w-20 sm:h-20 md:w-24 md:h-24 rounded-2xl bg-primary flex items-center justify-center shadow-lg">
            {isCreatingInstant ? (
              <Loader2 className="w-6 h-6 sm:w-7 sm:h-7 md:w-8 md:h-8 text-white animate-spin" />
            ) : (
              <Video className="w-6 h-6 sm:w-7 sm:h-7 md:w-8 md:h-8 text-white" />
            )}
          </div>
          <span className="font-semibold text-sm sm:text-base md:text-lg">
            {isCreatingInstant ? "Starting..." : "Instant conference"}
          </span>
        </div>

        <div
          className="flex flex-col items-center gap-2 sm:gap-3 cursor-pointer"
          onClick={() => router.push("/dashboard/conferences")}
        >
          <div className="w-16 h-16 sm:w-20 sm:h-20 md:w-24 md:h-24 rounded-2xl bg-sidebar flex items-center justify-center shadow">
            <Calendar className="w-6 h-6 sm:w-7 sm:h-7 md:w-8 md:h-8 text-primary" />
          </div>
          <span className="font-semibold text-sm sm:text-base md:text-lg">
            Conferences
          </span>
        </div>
      </div>

      {/* Join input */}
      <div className="flex justify-center mt-6 sm:mt-8 md:mt-10 px-2">
        <JoinMeetingInput
          className="mx-auto max-w-[280px] sm:max-w-[340px] md:max-w-[384px] gap-2 sm:gap-3"
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
            <div
              className="w-[36px] sm:w-[45px] h-[100px] sm:h-[118px] bg-[var(--primary)4D] rounded-sm flex items-center justify-center shrink-0 cursor-pointer"
              onClick={() => router.push("/dashboard/meetings/new")}
            >
              <div className="w-6 h-6 flex items-center justify-center">
                <Plus className="w-4 h-4 text-primary" />
              </div>
            </div>

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
