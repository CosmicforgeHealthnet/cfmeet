"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { ScrollArea, ScrollBar } from "@/components/ui/scroll-area";
import { Video, Calendar, Copy, Check, Plus, Loader2 } from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { useSpaces, useCreateInstantMeeting, useJoinConference } from "@/hooks/v2";
import type { MeetingSpaceResponse, ConferenceStatus } from "@/types/v2";
import { formatDateForDisplay, formatTimeForDisplay } from "@/utils/timezone";
import { getErrorMessage } from "@/utils/error";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";
import MeetingSuccessView from "./MeetingSuccessView";
import { storageStore } from "@/store";

export default function DashboardContent() {
  const router = useRouter();
  const { data: spacesData, isLoading } = useSpaces();
  const createInstantMeeting = useCreateInstantMeeting();
  const joinConference = useJoinConference();
  const [error, setError] = useState("");
  const [createdMeeting, setCreatedMeeting] = useState<{
    meetingCode: string;
    conferenceId: string;
  } | null>(null);

  const spaces = spacesData?.items ?? [];

  const handleInstantMeeting = async () => {
    if (createInstantMeeting.isPending) return;

    setError("");
    try {
      // Create instant meeting (space + live conference)
      const instantResponse = await createInstantMeeting.mutateAsync({});
      const { meetingSpace, conference } = instantResponse;

      // Join the conference to get RTC credentials
      const joinResponse = await joinConference.mutateAsync({
        id: conference.id,
        data: {},
      });

      // Store V2 join data for the room page
      storageStore.setV2JoinData({
        meetingSpaceId: meetingSpace.id,
        conferenceId: conference.id,
        participantId: joinResponse.participant.id,
        participantSessionId: joinResponse.participantSession?.id,
        role: joinResponse.participant.role,
        rtcUrl: joinResponse.rtc?.url,
        rtcCredential: joinResponse.rtc?.credential,
        permissions: joinResponse.permissions,
        mediaContext: joinResponse.mediaContext,
      });

      // Store participant token for subsequent API calls
      if (joinResponse.participantToken) {
        storageStore.setParticipantToken(joinResponse.participantToken);
      }

      // Show success view with meeting details
      setCreatedMeeting({
        meetingCode: meetingSpace.meetingCode,
        conferenceId: conference.id,
      });
    } catch (err) {
      console.error("Failed to create instant meeting:", err);
      setError(getErrorMessage(err, "Failed to create meeting. Please try again."));
    }
  };

  const handleJoinNow = () => {
    if (createdMeeting) {
      router.push(`/${createdMeeting.meetingCode}`);
    }
  };

  const isCreatingInstant = createInstantMeeting.isPending || joinConference.isPending;

  // Show success view after instant meeting creation
  if (createdMeeting) {
    const joinUrl = `${typeof window !== "undefined" ? window.location.origin : ""}/${createdMeeting.meetingCode}`;
    return (
      <MeetingSuccessView
        meetingIdentifier={createdMeeting.meetingCode}
        joinUrl={joinUrl}
        title="Meeting Created"
        description="Your instant meeting is ready. Share the link or join now."
        showJoinNow={true}
        onJoinNow={handleJoinNow}
        showScheduleAnother={false}
        onBack={() => setCreatedMeeting(null)}
      />
    );
  }

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter overflow-hidden">
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
            {isCreatingInstant ? "Starting..." : "New Meeting"}
          </span>
        </div>

        <div
          className="flex flex-col items-center gap-2 sm:gap-3 cursor-pointer"
          onClick={() => router.push("/dashboard/schedule")}
        >
          <div className="w-16 h-16 sm:w-20 sm:h-20 md:w-24 md:h-24 rounded-2xl bg-sidebar flex items-center justify-center shadow">
            <Calendar className="w-6 h-6 sm:w-7 sm:h-7 md:w-8 md:h-8 text-primary" />
          </div>
          <span className="font-semibold text-sm sm:text-base md:text-lg">
            Schedule
          </span>
        </div>
      </div>

      {/* Join input */}
      <div className="flex justify-center mt-6 sm:mt-8 md:mt-10 px-2">
        <JoinMeetingInput
          className="max-w-[280px] sm:max-w-[340px] md:max-w-[384px] gap-2 sm:gap-3"
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
        ) : spaces.length === 0 ? (
          <p className="text-sm text-muted-foreground">No meeting spaces yet.</p>
        ) : (
          <div className="flex items-start gap-3 sm:gap-4">
            {/* STATIC ACCENT BAR */}
            <div
              className="w-[36px] sm:w-[45px] h-[100px] sm:h-[118px] bg-[var(--primary)4D] rounded-sm flex items-center justify-center shrink-0 cursor-pointer"
              onClick={() => router.push("/dashboard/schedule")}
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

function getConferenceStatusInfo(status: ConferenceStatus): { label: string; className: string } {
  switch (status) {
    case "LIVE":
      return { label: "Live", className: "bg-green-500 text-white" };
    case "WAITING":
    case "PREPARING":
      return { label: "Starting", className: "bg-yellow-500 text-white" };
    case "CREATED":
      return { label: "Scheduled", className: "bg-blue-500 text-white" };
    case "ENDING":
      return { label: "Ending", className: "bg-orange-500 text-white" };
    case "ENDED":
      return { label: "Ended", className: "bg-gray-500 text-white" };
    case "CANCELLED":
      return { label: "Cancelled", className: "bg-red-500 text-white" };
    case "FAILED":
      return { label: "Failed", className: "bg-red-600 text-white" };
    default:
      return { label: status, className: "bg-gray-400 text-white" };
  }
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
    router.push(`/${space.meetingCode}`);
  };

  const conferenceStatus = space.latestConference?.status;
  const statusInfo = conferenceStatus ? getConferenceStatusInfo(conferenceStatus) : null;

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
          {statusInfo && (
            <span className={`px-1.5 py-0.5 rounded text-[9px] sm:text-[10px] font-medium shrink-0 ${statusInfo.className}`}>
              {statusInfo.label}
            </span>
          )}
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
          {space.meetingType.replace("_", " ")}
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
