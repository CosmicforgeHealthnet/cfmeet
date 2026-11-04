import DashboardLayout from "@/components/layout/DashboardLayout";
import CallingNavigation from "@/components/calling/CallingNavigation";

export default function CallingLayout({ children }: { children: React.ReactNode }) {
  return (
    <DashboardLayout>
      <CallingNavigation />
      {children}
    </DashboardLayout>
  );
}
