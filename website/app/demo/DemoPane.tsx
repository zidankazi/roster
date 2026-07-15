"use client";

// The Claude Code session shown inside roster's focused pane. Everything the
// agent "prints" is a brainless component, so the demo shows the same
// accessible React chrome (details/listbox/aria-live) roster is built to
// watch, never a <pre> dump. Split into a scrolling body and a pinned
// composer so RosterDemo can anchor the composer to the pane's bottom (a
// flex-none sibling) the way a real Claude Code pane sits.
import * as React from "react";
import { ClaudeLogo } from "@/components/brainless/claude/claude-header";
import { ClaudeMessage } from "@/components/brainless/claude/claude-message";
import { ClaudeTodoList } from "@/components/brainless/claude/claude-todo-list";
import { ClaudeThinking } from "@/components/brainless/claude/claude-thinking";
import { ClaudePrompt } from "@/components/brainless/claude/claude-prompt";

const GRAY = "#8a8a8a";

// The whole Claude session is brainless registry components with baked-in 13px
// type; roster's real pane packs its text far denser than the browser default.
// A single `zoom` on each block shrinks fonts, glyphs, borders and gaps together
// — uniformly, and without forking the registry components — so the pane reads
// at terminal density instead of dwarfing the window frame around it.
const PANE_SCALE = { zoom: 0.82 } as React.CSSProperties;

// The next-step prompt, parked in the composer with the cursor after it — the
// natural follow-up once the fix lands, so the pane reads like work in motion.
const GOAL =
  "open a PR with the before/after query timings once the tests pass";

/** The session transcript — header, warning, a short turn. Scrolls; the
    composer is rendered separately by RosterDemo so it can pin to the bottom. */
export function DemoPaneBody() {
  return (
    <div
      style={PANE_SCALE}
      className="space-y-3 font-mono text-[13px] leading-[1.6] text-[#e6e6e6]"
    >
      {/* Compact identity header — mascot + the three lines Claude Code boots
          with, no welcome-box border to clash with the pane frame. */}
      <div className="flex items-start gap-3">
        <ClaudeLogo scale={2.5} />
        <div className="min-w-0">
          <div className="font-semibold">
            Claude Code <span style={{ color: GRAY }}>v2.1.209</span>
          </div>
          <div style={{ color: GRAY }}>Fable 5 with high effort · Claude Max</div>
          <div style={{ color: GRAY }}>~/code/storefront</div>
        </div>
      </div>

      <div style={{ color: "#e0af68" }}>
        <span aria-hidden>⚠ </span>2 MCP servers need authentication · run /mcp
      </div>

      <ClaudeMessage role="user">
        the orders dashboard takes 4s to load — profile it and fix the slow query
      </ClaudeMessage>

      <ClaudeMessage>
        The list view runs a separate query per order to fetch its line items.
        I&apos;ll fold them into one join and pull the totals in the same
        round-trip.
      </ClaudeMessage>

      <ClaudeTodoList
        todos={[
          { label: "Reproduce the slow load with EXPLAIN ANALYZE", status: "done" },
          { label: "Batch the per-order lookups into one join", status: "active" },
          { label: "Add a regression test at 500 orders", status: "todo" },
        ]}
      />

      <ClaudeThinking />
    </div>
  );
}

/** The input composer, pinned to the pane bottom by RosterDemo. */
export function DemoComposer() {
  return (
    <div style={PANE_SCALE} className="font-mono text-[13px] leading-[1.6] text-[#e6e6e6]">
      {/* defaultValue (not value): the composer is a preset read-only-in-spirit
          display, and value-without-onChange is React's controlled-input trap. */}
      <ClaudePrompt defaultValue={GOAL} mode="auto" effort="high" />
    </div>
  );
}
