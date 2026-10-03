"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Video } from "lucide-react";

export default function JoinPage() {
  const [meetingCode, setMeetingCode] = useState("");
  const [error, setError] = useState("");
  const router = useRouter();

  const handleJoin = (e: React.FormEvent) => {
    e.preventDefault();
    setError("");

    const code = meetingCode.trim().toUpperCase();

    if (!code) {
      setError("Please enter a meeting code");
      return;
    }

    if (code.length < 6) {
      setError("Meeting code must be at least 6 characters");
      return;
    }

    // Navigate to the room with the meeting identifier
    router.push(`/${code}`);
  };

  return (
    <div className="flex items-center justify-center min-h-screen bg-gray-50">
      <div className="w-full max-w-md px-6">
        <div className="bg-white rounded-2xl shadow-lg p-8">
          {/* Header */}
          <div className="flex flex-col items-center gap-4 mb-8">
            <div className="w-16 h-16 rounded-full bg-[#029CD4]/10 flex items-center justify-center">
              <Video className="w-8 h-8 text-[#029CD4]" />
            </div>
            <div className="text-center">
              <h1 className="text-2xl font-bold text-[#343434]">
                Join a Meeting
              </h1>
              <p className="text-sm text-[#00000080] mt-1">
                Enter the meeting code to join
              </p>
            </div>
          </div>

          {/* Form */}
          <form onSubmit={handleJoin} className="space-y-4">
            <div>
              <Input
                type="text"
                placeholder="Enter meeting code (e.g., ABCD1234)"
                value={meetingCode}
                onChange={(e) => {
                  setMeetingCode(e.target.value.toUpperCase());
                  setError("");
                }}
                className="text-center text-lg tracking-wider uppercase h-12"
                maxLength={12}
              />
              {error && (
                <p className="text-sm text-red-500 mt-2 text-center">{error}</p>
              )}
            </div>

            <Button type="submit" className="w-full h-12 text-base">
              Join Meeting
            </Button>
          </form>

          {/* Divider */}
          <div className="flex items-center gap-4 my-6">
            <div className="flex-1 h-px bg-gray-200" />
            <span className="text-sm text-gray-400">or</span>
            <div className="flex-1 h-px bg-gray-200" />
          </div>

          {/* Create meeting link */}
          <div className="text-center">
            <p className="text-sm text-[#00000080]">
              Want to create a meeting?{" "}
              <Link
                href="/dashboard/schedule"
                className="text-[#029CD4] hover:underline font-medium"
              >
                Schedule one
              </Link>
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
