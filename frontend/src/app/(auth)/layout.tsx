import Image from "next/image";

export default function AuthLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <div className="relative min-h-screen w-full flex items-center justify-center px-4 py-6 sm:p-6 overflow-y-auto">
      {/* Full-screen background image */}
      <Image
        src="/frame.png"
        alt="Background"
        fill
        className="object-cover"
        priority
      />

      {/* Dark overlay */}
      <div className="absolute inset-0 bg-black/68" />

      {children}
    </div>
  );
}
