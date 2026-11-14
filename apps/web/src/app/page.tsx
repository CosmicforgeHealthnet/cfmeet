import LandingNav from "@/components/home/LandingNav";
import LandingHero from "@/components/home/LandingHero";
import LandingFeatures from "@/components/home/LandingFeatures";
import LandingHowItWorks from "@/components/home/LandingHowItWorks";
import LandingOrganizations from "@/components/home/LandingOrganizations";
import LandingDevelopers from "@/components/home/LandingDevelopers";
import { LandingCta, LandingFooter, LandingSafety } from "@/components/home/LandingClosing";

export default function Home() {
  return (
    <div className="min-h-screen overflow-x-clip bg-background">
      <LandingNav />
      <main>
        <LandingHero />
        <LandingFeatures />
        <LandingHowItWorks />
        <LandingOrganizations />
        <LandingDevelopers />
        <LandingSafety />
        <LandingCta />
      </main>
      <LandingFooter />
    </div>
  );
}
