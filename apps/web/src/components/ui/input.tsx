import * as React from "react"

import { cn } from "@/lib/utils"

function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "w-full px-3 sm:px-5 py-2 sm:py-3 h-9 sm:h-10 text-sm sm:text-base border border-border rounded-xl bg-transparent text-foreground font-medium placeholder:text-muted-foreground focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary disabled:pointer-events-none disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
}

export { Input }
