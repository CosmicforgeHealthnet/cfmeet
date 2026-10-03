"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

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

  const handleJoin = () => {
    if (!joinInput.trim()) return;

    let identifier = joinInput.trim();
    // extract from URL if present
    try {
      if (identifier.includes("http") || identifier.includes("https")) {
        const url = new URL(identifier);
        const parts = url.pathname.split("/").filter((p) => p);
        if (parts.length > 0) identifier = parts[parts.length - 1];
      }
    } catch {
      // ignore
    }

    router.push(`/${identifier}`);
  };

  return (
    <div className={cn("flex w-full gap-2", className)}>
      <Input
        placeholder="Enter Link or Code"
        className={cn("flex-1", inputClassName)}
        value={joinInput}
        onChange={(e) => setJoinInput(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && handleJoin()}
      />
      <Button
        size="sm"
        className={cn(
          "bg-[#029CD4] hover:bg-[#028bbd] shrink-0",
          buttonClassName,
        )}
        onClick={handleJoin}
      >
        Join
      </Button>
    </div>
  );
}
