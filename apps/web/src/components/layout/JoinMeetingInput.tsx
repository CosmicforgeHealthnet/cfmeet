"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { spacesService } from "@/services/v2/spaces.service";
import { meetingIdentifier } from "@/lib/meeting-spaces";
import { getErrorMessage } from "@/utils/error";

interface JoinMeetingInputProps {
  className?: string;
  inputClassName?: string;
  buttonClassName?: string;
}

export default function JoinMeetingInput({
  className,
  inputClassName,
  buttonClassName,
}: JoinMeetingInputProps) {
  const router = useRouter();
  const [joinInput, setJoinInput] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);

  const handleJoin = async () => {
    if (!joinInput.trim()) return;
    if (pending) return;
    setPending(true); setError("");
    try {
      const result = await spacesService.resolve(meetingIdentifier(joinInput));
      if (result.meetingSpace.status !== "ACTIVE") throw new Error("This meeting space is archived.");
      router.push(`/${encodeURIComponent(result.meetingSpace.meetingCode)}`);
    } catch (cause) { setError(getErrorMessage(cause, "Could not resolve this meeting.")); }
    finally { setPending(false); }
  };

  return (
    <div className="w-full"><div className={cn("flex w-full gap-2", className)}>
      <Input
        placeholder="Enter Link or Code"
        className={cn("flex-1", inputClassName)}
        value={joinInput}
        onChange={(e) => setJoinInput(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && void handleJoin()}
      />
      <Button
        size="sm"
        className={cn(
          "shrink-0",
          buttonClassName,
        )}
        onClick={() => void handleJoin()}
        disabled={pending || !joinInput.trim()}
      >
        {pending ? "Finding…" : "Join"}
      </Button>
    </div>{error ? <p role="alert" className="mt-2 text-sm text-destructive">{error}</p> : null}</div>
  );
}
