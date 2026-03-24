"use client";

import { ChevronLeft } from "lucide-react";
import { useRouter } from "next/navigation";

const GoBack = ({ text = "Go Back" }: { text?: string }) => {
  const router = useRouter();

  return (
    <button
      onClick={() => router.back()}
      className="flex items-center gap-1 text-[#029CD4] text-xs hover:opacity-80 transition-opacity mb-2 sm:mb-4 cursor-pointer"
    >
      <ChevronLeft size={20} className="sm:hidden" />
      <ChevronLeft size={24} className="hidden sm:block" />
      {text}
    </button>
  );
};

export default GoBack;
