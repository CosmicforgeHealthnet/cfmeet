"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { CheckCircle, ArrowLeft } from "lucide-react";

interface MeetingSuccessViewProps {
  meetingIdentifier: string;
  joinUrl: string;
  title?: string;
  description?: string;
  onScheduleAnother?: () => void;
  showScheduleAnother?: boolean;
  onJoinNow?: () => void;
  showJoinNow?: boolean;
  onBack?: () => void;
}

export default function MeetingSuccessView({
  meetingIdentifier,
  joinUrl,
  title = "Meeting Created",
  description = "Your meeting has been created successfully.",
  onScheduleAnother,
  showScheduleAnother = true,
  onJoinNow,
  showJoinNow = false,
  onBack,
}: MeetingSuccessViewProps) {
  const router = useRouter();
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    navigator.clipboard.writeText(joinUrl);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
      {/* Back Button */}
      {onBack && (
        <button
          onClick={onBack}
          className="flex items-center gap-2 text-[#343434] hover:text-[#029CD4] transition-colors mb-4"
        >
          <ArrowLeft className="w-5 h-5" />
          <span className="text-sm font-medium">Back</span>
        </button>
      )}

      <div className="max-w-2xl mx-auto mt-4 sm:mt-10 md:mt-20 text-center">
        {/* Success Icon */}
        <CheckCircle className="w-12 h-12 sm:w-16 sm:h-16 md:w-20 md:h-20 text-green-500 mx-auto mb-3 sm:mb-4" />

        {/* Title */}
        <h2 className="text-lg sm:text-xl md:text-2xl lg:text-3xl font-bold text-[#343434] mb-1.5 sm:mb-2">
          {title}
        </h2>

        {/* Description */}
        <p className="text-xs sm:text-sm md:text-base text-[#00000080] mb-4 sm:mb-6 px-4">
          {description}
        </p>

        {/* Meeting Details Card */}
        <div className="bg-[#FAFAFB] rounded-xl p-4 sm:p-5 md:p-6 space-y-3 sm:space-y-4 text-left mb-6 sm:mb-8">
          <div>
            <span className="text-xs sm:text-sm text-[#00000060] uppercase tracking-wide font-medium">
              Meeting Code
            </span>
            <p className="font-mono text-base sm:text-lg md:text-xl font-bold text-[#029CD4] mt-1">
              {meetingIdentifier}
            </p>
          </div>
          <div className="border-t border-[#E5E5E5] pt-3 sm:pt-4">
            <span className="text-xs sm:text-sm text-[#00000060] uppercase tracking-wide font-medium">
              Join Link
            </span>
            <p className="text-xs sm:text-sm md:text-base text-[#343434] break-all mt-1 leading-relaxed">
              {joinUrl}
            </p>
          </div>
        </div>

        {/* Action Buttons */}
        <div className="flex flex-col sm:flex-row gap-2 sm:gap-3 justify-center">
          <Button
            variant="outline"
            onClick={handleCopy}
            className="w-full sm:w-auto order-1"
          >
            {copied ? "Copied!" : "Copy Link"}
          </Button>
          {showJoinNow && onJoinNow && (
            <Button onClick={onJoinNow} className="w-full sm:w-auto order-2">
              Join Now
            </Button>
          )}
          <Button
            onClick={() => router.push("/dashboard/meetings")}
            className="w-full sm:w-auto order-3"
          >
            View Meetings
          </Button>
          {showScheduleAnother && onScheduleAnother && (
            <Button
              variant="ghost"
              onClick={onScheduleAnother}
              className="w-full sm:w-auto order-4"
            >
              Schedule Another
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
