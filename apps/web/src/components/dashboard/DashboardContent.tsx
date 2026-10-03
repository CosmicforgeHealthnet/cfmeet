"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { ScrollArea, ScrollBar } from "@/components/ui/scroll-area";
import { Video, Calendar, Copy, Check, Plus, Loader2 } from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { useMeetings } from "@/hooks";
import type { Meeting } from "@/types/meeting";
import { formatDateForDisplay, formatTimeForDisplay } from "@/utils/timezone";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";
import MeetingSuccessView from "./MeetingSuccessView";
import { meetingService } from "@/services/meeting.service";
import { storageStore } from "@/store";

export default function DashboardContent() {
  const router = useRouter();
  const { data, isLoading } = useMeetings({ page: 1, limit: 20 });
  const [isCreatingInstant, setIsCreatingInstant] = useState(false);
  const [error, setError] = useState("");
  const [createdMeeting, setCreatedMeeting] = useState<{
    meeting_identifier: string;
    join_url: string;
  } | null>(null);

  const meetings = data?.data ?? [];

  const handleInstantMeeting = async () => {
    if (isCreatingInstant) return;

    setIsCreatingInstant(true);
    setError("");
    try {
      const response = await meetingService.createInstantMeeting();

      if (response.success) {
        // Store join data for the room page
        storageStore.setInstantJoinData({
          meeting_id: response.data.id,
          participant_id: response.data.participant_id,
          role: "host",
          join_token: response.data.join_token,
          livekit_url: response.data.livekit_url,
          room_name: response.data.room_name,
        });

        // Show success view with meeting details
        setCreatedMeeting({
          meeting_identifier: response.data.meeting_identifier,
          join_url: response.data.join_url,
        });
      }
    } catch (err) {
      console.error("Failed to create instant meeting:", err);
      setError("Failed to create meeting. Please try again.");
    } finally {
      setIsCreatingInstant(false);
    }
  };

  const handleJoinNow = () => {
    if (createdMeeting) {
      router.push(`/${createdMeeting.meeting_identifier}`);
    }
  };

  // Show success view after instant meeting creation
  if (createdMeeting) {
    return (
      <MeetingSuccessView
        meetingIdentifier={createdMeeting.meeting_identifier}
        joinUrl={createdMeeting.join_url}
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
          <div className="w-16 h-16 sm:w-20 sm:h-20 md:w-24 md:h-24 rounded-2xl bg-[#029CD4] flex items-center justify-center shadow-lg">
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
          <div className="w-16 h-16 sm:w-20 sm:h-20 md:w-24 md:h-24 rounded-2xl bg-[#FAFAFB] flex items-center justify-center shadow">
            <Calendar className="w-6 h-6 sm:w-7 sm:h-7 md:w-8 md:h-8 text-[#029CD4]" />
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
          inputClassName="h-10 sm:h-11 border-[#029CD4] focus:ring-[#029CD44D]"
          buttonClassName="h-10 sm:h-11 px-5 sm:px-8"
        />
      </div>

      {/* Meeting history section */}
      <div className="mt-10 sm:mt-14 md:mt-20 px-0 sm:px-4 md:px-20">
        <h3 className="font-semibold mb-4 sm:mb-6 text-base sm:text-lg">
          Meeting History
        </h3>

        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <Spinner />
          </div>
        ) : meetings.length === 0 ? (
          <p className="text-sm text-[#00000080]">No meetings yet.</p>
        ) : (
          <div className="flex items-start gap-3 sm:gap-4">
            {/* STATIC ACCENT BAR */}
            <div
              className="w-[36px] sm:w-[45px] h-[100px] sm:h-[118px] bg-[#029CD44D] rounded-sm flex items-center justify-center shrink-0 cursor-pointer"
              onClick={() => router.push("/dashboard/schedule")}
            >
              <div className="w-6 h-6 flex items-center justify-center">
                <Plus className="w-4 h-4 text-[#029CD4]" />
              </div>
            </div>

            {/* SCROLLABLE CARDS */}
            <ScrollArea className="w-full whitespace-nowrap">
              <div className="flex gap-3 sm:gap-4 pb-4">
                {meetings.map((meeting: Meeting) => (
                  <MeetingCard key={meeting.id} meeting={meeting} />
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

function MeetingCard({ meeting }: { meeting: Meeting }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    navigator.clipboard.writeText(meeting.join_url);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="w-[200px] sm:w-[230px] md:w-[258px] h-[115px] sm:h-[125px] md:h-[135px] bg-white rounded-[10px] p-3 sm:p-4 shadow-sm border border-gray-50 shrink-0 flex flex-col justify-between">
      <div className="flex justify-between items-start">
        <h4 className="text-[#029CD4] font-semibold text-[13px] sm:text-[15px] md:text-[16px] leading-none truncate pr-2">
          {meeting.title}
        </h4>
        <button
          onClick={handleCopy}
          className="p-1 hover:bg-gray-100 rounded transition-colors shrink-0"
          title="Copy meeting link"
        >
          {copied ? (
            <Check className="w-4 h-4 text-green-500" />
          ) : (
            <Copy className="w-4 h-4 text-[#00000080]" />
          )}
        </button>
      </div>

      <div className="space-y-1">
        <p className="text-[#00000080] font-normal text-[11px] sm:text-[12px] leading-none">
          {formatDateForDisplay(meeting.start_time)}
        </p>
        <p className="text-[#00000080] font-normal text-[11px] sm:text-[12px] leading-none">
          {formatTimeForDisplay(meeting.start_time)}
        </p>
      </div>

      <div className="flex justify-between items-end">
        <div className="flex flex-col gap-1.5">
          <span className="text-[#00000080] font-normal text-[11px] sm:text-[12px] leading-none">
            {meeting.meeting_identifier}
          </span>
          <span
            className={`text-[10px] px-1.5 py-0.5 rounded-full capitalize font-medium w-fit ${
              meeting.status === "live"
                ? "bg-green-100 text-green-700"
                : meeting.status === "scheduled"
                  ? "bg-blue-100 text-blue-700"
                  : "bg-gray-100 text-gray-600"
            }`}
          >
            {meeting.status}
          </span>
        </div>
      </div>
    </div>
  );
}
