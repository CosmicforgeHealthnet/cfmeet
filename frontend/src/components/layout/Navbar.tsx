"use client";

import React, { useState } from "react";
import Image from "next/image";
import Link from "next/link";
import { Menu, X } from "lucide-react";
import { Button } from "@/components/ui/button";

const Navbar = () => {
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <nav className="flex items-center justify-between px-4 sm:px-8 py-4 sm:py-6 bg-transparent relative">
      <div className="flex items-center gap-2">
        <Image
          src="/logo.png"
          alt="CosmicForge Logo"
          width={140}
          height={32}
          className="w-[120px] sm:w-[180px] h-auto"
          priority
        />
      </div>

      {/* Desktop nav */}
      <div className="hidden md:flex items-center gap-8">
        <Link
          href="/pricing"
          className="text-gray-700 hover:text-black font-medium"
        >
          Pricing
        </Link>
        <Link
          href="/login"
          className="text-gray-700 hover:text-black font-medium"
        >
          Login
        </Link>
        <Link href="/signup">
          <Button size="sm">Sign Up</Button>
        </Link>
      </div>

      {/* Mobile hamburger button */}
      <button
        className="md:hidden text-gray-700"
        onClick={() => setMenuOpen(!menuOpen)}
        aria-label="Toggle menu"
      >
        {menuOpen ? <X size={28} /> : <Menu size={28} />}
      </button>

      {/* Mobile menu dropdown */}
      {menuOpen && (
        <div className="absolute top-full left-0 right-0 bg-white shadow-lg border-t border-gray-100 flex flex-col items-center gap-4 py-6 z-50 md:hidden">
          <Link
            href="/pricing"
            className="text-gray-700 hover:text-black font-medium text-lg"
            onClick={() => setMenuOpen(false)}
          >
            Pricing
          </Link>
          <Link
            href="/login"
            className="text-gray-700 hover:text-black font-medium text-lg"
            onClick={() => setMenuOpen(false)}
          >
            Login
          </Link>
          <Link href="/signup" onClick={() => setMenuOpen(false)}>
            <Button size="sm">Sign Up</Button>
          </Link>
        </div>
      )}
    </nav>
  );
};

export default Navbar;
