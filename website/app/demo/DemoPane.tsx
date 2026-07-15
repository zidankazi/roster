"use client";

// The Claude Code session shown inside roster's focused pane. Everything the
// agent "prints" is a brainless component, so the demo shows the same
// accessible React chrome (details/listbox/aria-live) roster is built to
// watch, never a <pre> dump. Split into a scrolling body and a pinned
// composer so RosterDemo can anchor the composer to the pane's bottom (a
// flex-none sibling) the way a real Claude Code pane sits.
import { ClaudeLogo } from "@/components/brainless/claude/claude-header";
import { ClaudeMessage } from "@/components/brainless/claude/claude-message";
import { ClaudeTodoList } from "@/components/brainless/claude/claude-todo-list";
import { ClaudeThinking } from "@/components/brainless/claude/claude-thinking";
import { ClaudePrompt } from "@/components/brainless/claude/claude-prompt";

const GRAY = "#8a8a8a";

// The pasted /goal prompt, parked in the composer — mirrors the real screenshot
// where the launch prompt sits waiting with the cursor after it.
const GOAL =
  "rebuild the roster UI on the website — use brainless for the agent chrome";

/** The session transcript — header, warning, a short turn. Scrolls; the
    composer is rendered separately by RosterDemo so it can pin to the bottom. */
export function DemoPaneBody() {
  return (
    <div className="space-y-3 font-mono text-[13px] leading-[1.6] text-[#e6e6e6]">
      {/* Compact identity header — mascot + the three lines Claude Code boots
          with, no welcome-box border to clash with the pane frame. */}
      <div className="flex items-start gap-3">
        <ClaudeLogo scale={2.5} />
        <div className="min-w-0">
          <div className="font-semibold">
            Claude Code <span style={{ color: GRAY }}>v2.1.209</span>
          </div>
          <div style={{ color: GRAY }}>Fable 5 with high effort · Claude Max</div>
          <div style={{ color: GRAY }}>~/Desktop/roster</div>
        </div>
      </div>

      <div style={{ color: "#e0af68" }}>
        <span aria-hidden>⚠ </span>2 MCP servers need authentication · run /mcp
      </div>

      <ClaudeMessage role="user">
        rebuild the roster UI on the website
      </ClaudeMessage>

      <ClaudeMessage>
        I&apos;ll pull in the brainless Claude Code components and assemble the
        sidebar and pane around them.
      </ClaudeMessage>

      <ClaudeTodoList
        todos={[
          { label: "Register the @brainless registry", status: "done" },
          { label: "Build the sidebar + agent cards", status: "active" },
          { label: "Wire the pane to the Claude session", status: "todo" },
        ]}
      />

      <ClaudeThinking />
    </div>
  );
}

/** The input composer, pinned to the pane bottom by RosterDemo. */
export function DemoComposer() {
  return (
    <div className="font-mono text-[13px] leading-[1.6] text-[#e6e6e6]">
      {/* defaultValue (not value): the composer is a preset read-only-in-spirit
          display, and value-without-onChange is React's controlled-input trap. */}
      <ClaudePrompt defaultValue={GOAL} mode="auto" effort="high" />
    </div>
  );
}
