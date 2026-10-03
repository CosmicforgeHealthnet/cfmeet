"use client";

import { useMicLevel } from "@/hooks/useMicLevel";

interface MicVisualizerProps {
  stream: MediaStream | null;
  isOn: boolean;
  className?: string;
  barColor?: string;
  count?: number;
}

export function MicVisualizer({
  stream,
  isOn,
  className = "",
  barColor = "bg-green-400",
  count = 5,
}: MicVisualizerProps) {
  const level = useMicLevel(isOn, stream);

  if (!isOn) return null;

  return (
    <div
      className={`flex items-end justify-center gap-[2px] pointer-events-none ${className}`}
    >
      {Array.from({ length: count }).map((_, i) => {
        // Simple visualizer math
        const height = Math.max(2, level * 20 * ((i + 1) / count));
        return (
          <div
            key={i}
            className={`w-[3px] rounded-full transition-all duration-75 ${barColor}`}
            style={{ height: `${height}px` }}
          />
        );
      })}
    </div>
  );
}
