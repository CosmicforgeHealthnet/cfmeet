"use client";

import Link from "next/link";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";
import { Video } from "lucide-react";

export default function JoinPage() {
  return (
    <div className="flex items-center justify-center min-h-screen bg-muted">
      <div className="w-full max-w-md px-6">
        <div className="bg-background rounded-2xl shadow-lg p-8">
          {/* Header */}
          <div className="flex flex-col items-center gap-4 mb-8">
            <div className="w-16 h-16 rounded-full bg-primary/10 flex items-center justify-center">
              <Video className="w-8 h-8 text-primary" />
            </div>
            <div className="text-center">
              <h1 className="text-2xl font-bold text-foreground">
                Join a Meeting
              </h1>
              <p className="text-sm text-muted-foreground mt-1">
                Enter the meeting code to join
              </p>
            </div>
          </div>

          {/* Form */}
          <JoinMeetingInput />
          <p className="mt-4 text-sm"><Link href="/invite" className="text-primary hover:underline">Have an invitation link or token?</Link></p>
          <p className="mt-3 text-xs text-muted-foreground">Meeting links and codes identify a space. Membership, roles and admission are checked separately.</p>

          {/* Divider */}
          <div className="flex items-center gap-4 my-6">
            <div className="flex-1 h-px bg-muted" />
            <span className="text-sm text-muted-foreground">or</span>
            <div className="flex-1 h-px bg-muted" />
          </div>

          {/* Create meeting link */}
          <div className="text-center">
            <p className="text-sm text-muted-foreground">
              Want to create a meeting?{" "}
              <Link
                href="/dashboard/meetings/new"
                className="text-primary hover:underline font-medium"
              >
                Create one
              </Link>
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
