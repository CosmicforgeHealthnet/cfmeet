// app/page.tsx
import Navbar from '@/components/layout/Navbar';
import SocialSidebar from '@/components/layout/SocialSidebar';
import Hero from '@/components/home/Hero';

export default function Home() {
  return (
    <div className="min-h-screen bg-white flex flex-col relative overflow-hidden">
      {/* Decorative dashed lines - hidden on small screens */}
      <div className="hidden sm:block absolute top-0 left-1/2 w-px h-24 border-l border-dashed border-blue-400 -z-10" />
      <div className="hidden sm:block absolute top-1/2 left-0 w-32 h-px border-t border-dashed border-blue-400 -z-10" />

      <Navbar />
      <Hero />
      <SocialSidebar />
    </div>
  );
}
