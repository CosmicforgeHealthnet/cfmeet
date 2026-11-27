import * as React from "react"

import { cn } from "@/lib/utils"

function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "w-full px-3 sm:px-5 py-2 sm:py-3 h-9 sm:h-10 text-sm sm:text-base border border-transparent rounded-xl bg-muted/70 text-foreground font-medium placeholder:text-muted-foreground focus:outline-none focus:border-primary focus:bg-background focus:ring-2 focus:ring-primary/20 disabled:pointer-events-none disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
}

export { Input }
