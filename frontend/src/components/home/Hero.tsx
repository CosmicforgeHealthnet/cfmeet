"use client";

import React, { useState, useEffect, useCallback, useRef } from "react";
import Image from "next/image";
import { ChevronLeft, ChevronRight } from "lucide-react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import JoinMeetingInput from "@/components/layout/JoinMeetingInput";

const slides = [
  {
    image: "/hero1.png",
    alt: "Video Call Interface",
    heading: "Connect Face-to-Face. Anytime. Anywhere.",
    description:
      "Experience crystal-clear video calls built for speed, security, and simplicity. Stay connected with friends, teams, and clients through seamless conversations that feel natural—no matter where you are.",
  },
  {
    image: "/hero2.png",
    alt: "Video Calls That Just Work.",
    heading: "Video Calls That Just Work.",
    description:
      "No delays. No distractions. Just smooth, high-quality video calls designed for real conversations. Connect instantly, collaborate effortlessly, and communicate without limits.",
  },
  {
    image: "/hero3.png",
    alt: "Fast. Secure. Real-Time Video Calling.",
    heading: "Fast. Secure. Real-Time Video Calling.",
    description:
      "Built for reliability and performance, our video calling app delivers uninterrupted communication with end-to-end security. Connect confidently with clarity you can trust.",
  },
];

const AUTO_SLIDE_INTERVAL = 5000;

// Phases:
// "visible"  → content shown at center (translate-x-0, opacity-1)
// "exit"     → slide out to the left (-translate-x-full, opacity-0) WITH transition
// "reset"    → instantly jump to right (translate-x-full, opacity-0) NO transition
// "enter"    → slide in from right to center (translate-x-0, opacity-1) WITH transition

type Phase = "visible" | "exit" | "reset" | "enter";

const Hero = () => {
  const [currentSlide, setCurrentSlide] = useState(0);
  const [imagePhase, setImagePhase] = useState<Phase>("visible");
  const [textPhase, setTextPhase] = useState<Phase>("visible");
  const lockRef = useRef(false);

  const changeSlide = useCallback((newIndex: number) => {
    if (lockRef.current) return;
    lockRef.current = true;

    // Step 1: Image exits to the left
    setImagePhase("exit");

    // Step 2: Text exits to the left (staggered)
    setTimeout(() => setTextPhase("exit"), 150);

    // Step 3: After exit animation, swap content & instantly reposition to the right
    setTimeout(() => {
      setCurrentSlide(newIndex);
      setImagePhase("reset");
    }, 550);

    setTimeout(() => {
      setTextPhase("reset");
    }, 700);

    // Step 4: Slide image in from right
    setTimeout(() => setImagePhase("enter"), 600);

    // Step 5: Slide text in from right (staggered)
    setTimeout(() => setTextPhase("enter"), 750);

    // Step 6: Done
    setTimeout(() => {
      setImagePhase("visible");
      setTextPhase("visible");
      lockRef.current = false;
    }, 1200);
  }, []);

  const prevSlide = useCallback(() => {
    const prev = currentSlide === 0 ? slides.length - 1 : currentSlide - 1;
    changeSlide(prev);
  }, [currentSlide, changeSlide]);

  const nextSlide = useCallback(() => {
    const next = currentSlide === slides.length - 1 ? 0 : currentSlide + 1;
    changeSlide(next);
  }, [currentSlide, changeSlide]);

  useEffect(() => {
    const timer = setInterval(nextSlide, AUTO_SLIDE_INTERVAL);
    return () => clearInterval(timer);
  }, [nextSlide]);

  const slide = slides[currentSlide];

  const getClasses = (phase: Phase) => {
    switch (phase) {
      case "visible":
        return "translate-x-0 opacity-100 transition-all duration-500 ease-out";
      case "exit":
        return "-translate-x-full opacity-0 transition-all duration-500 ease-in";
      case "reset":
        // Instantly jump to right — no transition
        return "translate-x-full opacity-0";
      case "enter":
        return "translate-x-0 opacity-100 transition-all duration-500 ease-out";
    }
  };

  return (
    <main className="flex flex-col items-center justify-center flex-1 px-4 sm:px-6 text-center mt-6 sm:mt-10">
      {/* Central Illustration Area */}
      <div className="relative w-full max-w-sm sm:max-w-lg md:max-w-2xl aspect-video mb-6 sm:mb-8 flex items-center justify-center overflow-hidden">
        {/* Navigation Arrows */}
        <button
          onClick={prevSlide}
          className="absolute left-0 z-10 text-[#029CD4] opacity-50 hover:opacity-100 transition-opacity"
          aria-label="Previous slide"
        >
          <ChevronLeft size={28} className="sm:hidden" />
          <ChevronLeft size={36} className="hidden sm:block" />
        </button>

        {/* Central UI Image */}
        <div
          className={`relative w-full h-56 sm:h-75 md:h-96 mx-14 sm:mx-20 ${getClasses(imagePhase)}`}
        >
          <Image
            src={slide.image}
            alt={slide.alt}
            fill
            className="object-contain"
          />
        </div>

        <button
          onClick={nextSlide}
          className="absolute right-0 z-10 text-[#029CD4] opacity-50 hover:opacity-100 transition-opacity"
          aria-label="Next slide"
        >
          <ChevronRight size={28} className="sm:hidden" />
          <ChevronRight size={36} className="hidden sm:block" />
        </button>
      </div>

      {/* Text Content */}
      <div className={`overflow-hidden w-full ${getClasses(textPhase)}`}>
        <h1 className="text-xl sm:text-2xl md:text-3xl font-bold text-gray-900 mb-3 sm:mb-4 px-2">
          {slide.heading}
        </h1>
        <p className="max-w-2xl mx-auto text-sm sm:text-base text-gray-600 leading-relaxed mb-6 sm:mb-8 px-2">
          {slide.description}
        </p>
      </div>

      {/* Slide Indicators */}
      <div className="flex gap-2 mb-6 sm:mb-8">
        {slides.map((_, index) => (
          <button
            key={index}
            onClick={() => changeSlide(index)}
            className={`w-2.5 h-2.5 rounded-full transition-colors ${
              index === currentSlide ? "bg-[#029CD4]" : "bg-gray-300"
            }`}
            aria-label={`Go to slide ${index + 1}`}
          />
        ))}
      </div>

      <Link href="/signup">
        <Button size="lg" className="mb-8 sm:mb-12">
          Sign Up
        </Button>
      </Link>

      {/* Join Link Input */}
      <JoinMeetingInput
        className="max-w-xs sm:max-w-md mb-6 sm:mb-12"
        inputClassName="border-[#029CD4] focus:ring-[#029CD44D] text-sm sm:text-base"
        buttonClassName="px-5 sm:px-8"
      />
    </main>
  );
};

export default Hero;
