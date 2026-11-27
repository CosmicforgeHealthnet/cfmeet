"use client";

import Link from "next/link";
import { Link2, Ticket } from "lucide-react";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";

export default function JoinPage() {
  return (
    <div>
      <h1 className="text-3xl font-bold tracking-tight text-foreground">Join a meeting</h1>
      <p className="mt-2 text-base text-muted-foreground">
        Enter the meeting code or paste the link you were sent. No account needed.
      </p>

      <label htmlFor="join-code" className="mt-8 flex items-center gap-2 text-sm font-medium text-foreground">
        <Link2 className="size-4 text-primary" aria-hidden />
        Meeting code or link
      </label>
      <JoinMeetingInput
        inputId="join-code"
        placeholder="e.g. kxr-mpqa-tdv"
        className="mt-2"
        inputClassName="h-12 rounded-xl px-4 py-0 text-base sm:h-12 sm:px-4 sm:py-0"
        buttonClassName="h-12 rounded-xl px-6 text-base shadow-none disabled:bg-muted disabled:text-muted-foreground disabled:opacity-100"
      />
      <p className="mt-3 text-sm text-muted-foreground">
        The host lets you in from the waiting room when the meeting needs it.
      </p>

      <Link
        href="/invite"
        className="mt-6 flex items-center gap-3 rounded-xl border border-border px-4 py-3 text-sm text-foreground transition-colors hover:bg-muted"
      >
        <Ticket className="size-4 shrink-0 text-primary" aria-hidden />
        <span className="flex-1">Have an invitation token instead?</span>
        <span className="font-semibold text-primary">Use it</span>
      </Link>

      <p className="mt-8 border-t border-border pt-6 text-center text-sm text-muted-foreground">
        Want to host?{" "}
        <Link href="/login" className="font-semibold text-primary hover:underline">Sign in</Link> or{" "}
        <Link href="/signup" className="font-semibold text-primary hover:underline">create an account</Link>
      </p>
    </div>
  );
}
