"use client";

import { useEffect, useRef } from "react";
import lottie from "lottie-web";
import animationData from "./claude-lottie.json";

// The Claude mark, rendered inline from the dotLottie asset (extracted to
// claude-lottie.json). lottie-web injects an <svg> into the span container —
// valid inline content, unlike a Lottie <div>, so it nests in the tagline
// sentence without breaking the HTML.
//
// The source composition parks its artwork in the bottom-right of a large,
// mostly-empty canvas, so once it loads we crop the svg's viewBox to the
// actual drawn bounds — otherwise the mark renders tiny and low in its box.
export function ClaudeMark() {
  const container = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!container.current) return;
    const anim = lottie.loadAnimation({
      container: container.current,
      renderer: "svg",
      loop: true,
      autoplay: true,
      animationData,
    });

    const reframe = () => {
      const svg = container.current?.querySelector("svg");
      if (!svg) return;
      try {
        const b = svg.getBBox();
        const pad = 0.05;
        const px = b.width * pad;
        const py = b.height * pad;
        svg.setAttribute(
          "viewBox",
          `${b.x - px} ${b.y - py} ${b.width + px * 2} ${b.height + py * 2}`,
        );
        svg.setAttribute("preserveAspectRatio", "xMidYMid meet");
      } catch {
        // getBBox can throw if the svg isn't laid out yet; skip this pass.
      }
    };

    anim.addEventListener("DOMLoaded", reframe);
    return () => {
      anim.removeEventListener("DOMLoaded", reframe);
      anim.destroy();
    };
  }, []);

  return (
    <span className="claude-mark" role="img" aria-label="Claude" ref={container} />
  );
}
