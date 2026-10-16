"use client";

import React, { useState } from "react";
import Image from "next/image";
import Link from "next/link";
import { Menu, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ThemeToggle } from "@/components/ui/theme-toggle";

const Navbar = () => {
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <nav className="flex items-center justify-between px-4 sm:px-8 py-4 sm:py-6 bg-transparent relative">
      <div className="flex items-center gap-2">
        <Image
          src="/CFMeet.png"
          alt="CF Meet Logo"
          width={100}
          height={24}
          className="w-[80px] sm:w-[100px] h-auto"
          priority
        />
      </div>

      {/* Desktop nav */}
      <div className="hidden md:flex items-center gap-6">
        <ThemeToggle />
        <Link
          href="/login"
          className="text-muted-foreground hover:text-foreground font-medium transition-colors"
        >
          Login
        </Link>
        <Link href="/signup">
          <Button size="sm">Sign Up</Button>
        </Link>
      </div>

      {/* Mobile hamburger button */}
      <div className="flex md:hidden items-center gap-2">
        <ThemeToggle />
        <button
          className="text-muted-foreground"
          onClick={() => setMenuOpen(!menuOpen)}
          aria-label="Toggle menu"
        >
          {menuOpen ? <X size={28} /> : <Menu size={28} />}
        </button>
      </div>

      {/* Mobile menu dropdown */}
      {menuOpen && (
        <div className="absolute top-full left-0 right-0 bg-card shadow-lg border-t border-border flex flex-col items-center gap-4 py-6 z-50 md:hidden">
          <Link
            href="/login"
            className="text-muted-foreground hover:text-foreground font-medium text-lg transition-colors"
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
