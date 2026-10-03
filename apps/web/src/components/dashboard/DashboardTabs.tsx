"use client";

import { usePathname, useRouter } from "next/navigation";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

const tabs = [
  { label: "Dashboard", href: "/dashboard" },
  { label: "Meetings", href: "/dashboard/meetings" },
  { label: "Schedule", href: "/dashboard/schedule" },
];

export default function DashboardTabs() {
  const pathname = usePathname();
  const router = useRouter();

  return (
    <div className="flex gap-3 sm:gap-4 md:gap-6 border-b px-4 sm:px-6 md:px-8 overflow-x-auto">
      {tabs.map((tab) => {
        const isActive =
          tab.href === "/dashboard"
            ? pathname === "/dashboard"
            : pathname.startsWith(tab.href);

        return (
          <Button
            key={tab.href}
            variant="ghost"
            onClick={() => router.push(tab.href)}
            className={cn(
              "py-3 sm:py-4 text-xs sm:text-sm font-medium relative rounded-none whitespace-nowrap",
              isActive ? "text-[#029CD4]" : "text-gray-400"
            )}
          >
            {tab.label}

            {isActive && (
              <span className="absolute left-0 bottom-0 h-[2px] w-full bg-[#029CD4]" />
            )}
          </Button>
        );
      })}
    </div>
  );
}
