"use client";

import { useEffect, useRef, useState } from "react";

/**
 * Fades and lifts its content into place the first time it scrolls into view.
 * Inline styles (not utility classes) so the motion never depends on a compiled class;
 * people who ask for reduced motion see the content immediately.
 */
export default function Reveal({
  children,
  delay = 0,
  className,
  id,
  variant = "up",
  as: Tag = "div",
}: {
  children: React.ReactNode;
  delay?: number;
  className?: string;
  id?: string;
  variant?: "up" | "scale" | "left" | "right";
  as?: "div" | "li" | "article";
}) {
  const ref = useRef<HTMLElement>(null);
  const [shown, setShown] = useState(false);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches || !("IntersectionObserver" in window)) {
      setShown(true);
      return;
    }
    const io = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setShown(true);
          io.disconnect();
        }
      },
      { threshold: 0.15, rootMargin: "0px 0px -60px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, []);

  const ease = "cubic-bezier(0.22, 1, 0.36, 1)";
  const from = {
    up: "translateY(28px)",
    scale: "translateY(16px) scale(0.96)",
    left: "translateX(-36px)",
    right: "translateX(36px)",
  }[variant];
  return (
    <Tag
      ref={ref as React.Ref<never>}
      id={id}
      className={className}
      style={{
        opacity: shown ? 1 : 0,
        transform: shown ? "none" : from,
        filter: shown ? "none" : "blur(4px)",
        transition: `opacity 900ms ${ease} ${delay}ms, transform 900ms ${ease} ${delay}ms, filter 600ms ${ease} ${delay}ms`,
        willChange: shown ? undefined : "opacity, transform, filter",
      }}
    >
      {children}
    </Tag>
  );
}
