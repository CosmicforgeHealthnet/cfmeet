"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

/**
 * Plain CSS-overflow scroll area.
 *
 * Previously this wrapped `@radix-ui/react-scroll-area`, which renders an
 * inline `<style dangerouslySetInnerHTML>` on mount. Under the site's
 * Content-Security-Policy (`require-trusted-types-for 'script'`) that raw
 * `innerHTML` assignment is blocked and throws during React's commit phase,
 * crashing any client-rendered page that uses it (e.g. the dashboard, which
 * renders client-side only). A native scroll container avoids Trusted Types
 * entirely.
 */
function ScrollArea({
  className,
  children,
  ...props
}: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="scroll-area"
      className={cn("relative min-w-0 overflow-auto", className)}
      {...props}
    >
      <div
        data-slot="scroll-area-viewport"
        className="rounded-[inherit] outline-none"
      >
        {children}
      </div>
    </div>
  )
}

/**
 * Kept for API compatibility. Native scrollbars are used instead of a
 * custom Radix scrollbar, so this renders nothing.
 */
function ScrollBar(_props: {
  orientation?: "vertical" | "horizontal"
  className?: string
}) {
  return null
}

export { ScrollArea, ScrollBar }
