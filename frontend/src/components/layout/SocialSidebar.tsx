import React from "react";
import { Facebook, Twitter, Instagram } from "lucide-react";

const SocialSidebar = () => {
  return (
    <>
      {/* Desktop: fixed vertical sidebar on the right */}
      <div className="hidden md:flex fixed right-8 top-1/2 -translate-y-1/2 flex-col gap-6 text-[#029CD4]">
        <a href="#" className="hover:opacity-70 transition-opacity">
          <Facebook size={24} />
        </a>
        <a href="#" className="hover:opacity-70 transition-opacity">
          <Twitter size={24} />
        </a>
        <a href="#" className="hover:opacity-70 transition-opacity">
          <Instagram size={24} />
        </a>
      </div>

      {/* Mobile: horizontal row at the bottom of the page */}
      <div className="flex md:hidden justify-center gap-8 py-6 text-[#029CD4]">
        <a href="#" className="hover:opacity-70 transition-opacity">
          <Facebook size={22} />
        </a>
        <a href="#" className="hover:opacity-70 transition-opacity">
          <Twitter size={22} />
        </a>
        <a href="#" className="hover:opacity-70 transition-opacity">
          <Instagram size={22} />
        </a>
      </div>
    </>
  );
};

export default SocialSidebar;
