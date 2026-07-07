"use client";

import { useEffect, useRef } from "react";

// The ASCII wordmark, byte-for-byte the same art roster paints on launch
// (figlet Georgia11) — see crates/roster-tui/src/launcher.rs.
const WORDMARK: string[] = [
  "                            mm                   ",
  "                            MM                   ",
  "`7Mb,od8 ,pW\"Wq.  ,pP\"Ybd mmMMmm .gP\"Ya `7Mb,od8 ",
  "  MM' \"'6W'   `Wb 8I   `\"   MM  ,M'   Yb  MM' \"' ",
  "  MM    8M     M8 `YMMMa.   MM  8M\"\"\"\"\"\"  MM     ",
  "  MM    YA.   ,A9 L.   I8   MM  YM.    ,  MM     ",
  ".JMML.   `Ybmd9'  M9mmmP'   `Mbmo`Mbmmd'.JMML.   ",
];

// Widest trimmed row — the reveal wipe and shine sweep run across this width.
const MARK_WIDTH = Math.max(
  ...WORDMARK.map((row) => row.replace(/\s+$/, "").length),
);

// Stand-in glyphs a flickering cell shows for a beat. None occur in WORDMARK.
const FLICKER_GLYPHS = ["*", "+", "~", "#"];

const U64 = (1n << 64n) - 1n;

// The deterministic per-cell, per-beat flicker hash from launcher.rs: decides
// whether a cell shows a stand-in glyph this beat, and which one.
function flickerAt(col: number, row: number, beat: number): string | null {
  let h = (BigInt(beat) * 0x9e3779b97f4a7c15n) & U64;
  h = (h + (BigInt(col) << 17n)) & U64;
  h = (h + (BigInt(row) << 41n)) & U64;
  h ^= h >> 33n;
  h = (h * 0xff51afd7ed558ccdn) & U64;
  h ^= h >> 29n;
  if (h % 60n === 0n) {
    return FLICKER_GLYPHS[Number((h / 60n) % BigInt(FLICKER_GLYPHS.length))];
  }
  return null;
}

// The TUI advances a discrete `tick` ~8×/s. We keep the exact same geometry
// (same reveal, shine, and flicker math) but read `tick` as a continuous
// value off a real clock and re-evaluate every animation frame — so the
// motion glides at the display's refresh rate instead of stepping at 8fps.
const TICK_MS = 125;

type Cell = {
  el: HTMLSpanElement;
  row: number;
  col: number;
  ch: string;
  cls: string;
  glyph: string;
};

export function Wordmark() {
  const container = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const root = container.current;
    if (!root) return;

    const cells: Cell[] = Array.from(
      root.querySelectorAll<HTMLSpanElement>("span[data-cell]"),
    ).map((el) => ({
      el,
      row: Number(el.dataset.row),
      col: Number(el.dataset.col),
      ch: el.dataset.ch ?? " ",
      cls: "c-off",
      glyph: el.dataset.ch ?? " ",
    }));

    const set = (c: Cell, cls: string, glyph: string) => {
      if (c.cls !== cls) {
        c.el.className = cls;
        c.cls = cls;
      }
      if (c.glyph !== glyph) {
        c.el.textContent = glyph;
        c.glyph = glyph;
      }
    };

    // Respect reduced-motion: paint the wordmark solid and still.
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      for (const c of cells) set(c, "c-base", c.ch);
      return;
    }

    let raf = 0;
    let startedAt: number | null = null;

    const frame = (now: number) => {
      if (startedAt === null) startedAt = now;
      const tick = (now - startedAt) / TICK_MS;

      const revealed = Math.min(tick * 6, MARK_WIDTH);
      const shineCycle = MARK_WIDTH + 32;
      const shine = ((tick * 2) % shineCycle) - 16;
      const fullyRevealed = revealed >= MARK_WIDTH;
      const beat = Math.floor(tick / 2);

      for (const c of cells) {
        if (c.col >= revealed) {
          set(c, "c-off", c.ch);
          continue;
        }
        let cls = "c-base";
        let glyph = c.ch;
        const offset = c.col - shine;
        if (fullyRevealed && offset >= 0 && offset < 6) {
          cls = "c-shine";
        }
        const stand = flickerAt(c.col, c.row, beat);
        if (stand !== null) {
          glyph = stand;
          cls = "c-flicker";
        }
        set(c, cls, glyph);
      }

      raf = requestAnimationFrame(frame);
    };

    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div className="wordmark" role="img" aria-label="roster" ref={container}>
      {WORDMARK.map((line, row) => (
        <div className="wordmark-row" key={row}>
          {Array.from(line).map((ch, col) =>
            ch === " " ? (
              <span key={col}> </span>
            ) : (
              // Starts hidden so server and client first paint match; the rAF
              // loop reveals and animates it.
              <span
                key={col}
                data-cell=""
                data-row={row}
                data-col={col}
                data-ch={ch}
                className="c-off"
              >
                {ch}
              </span>
            ),
          )}
        </div>
      ))}
    </div>
  );
}
