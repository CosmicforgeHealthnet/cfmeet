import { cn } from "@/lib/utils";

export interface SpinnerProps extends React.HTMLAttributes<HTMLDivElement> {
  size?: "sm" | "md" | "lg" | "xl";
  label?: string;
}

export function Spinner({
  className,
  size = "md",
  label,
  ...props
}: SpinnerProps) {
  const sizeClasses = {
    sm: "w-4 h-4 border",
    md: "w-8 h-8 border-2",
    lg: "w-12 h-12 border-2",
    xl: "w-16 h-16 border-4",
  };

  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-4",
        className,
      )}
      {...props}
    >
      <div
        className={cn(
          "border-[#029CD4] border-t-transparent rounded-full animate-spin",
          sizeClasses[size],
        )}
      />
      {label && <p className="text-sm text-[#00000080]">{label}</p>}
    </div>
  );
}
