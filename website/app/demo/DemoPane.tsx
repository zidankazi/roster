"use client";

// The Claude Code session shown inside roster's focused pane. Everything the
// agent "prints" — messages, plan, tool call, thinking line, composer — is a
// brainless component, so the demo shows the same accessible React chrome
// (details/listbox/aria-live) roster is built to watch, never a <pre> dump.
// The turn is themed to *this* task (rebuilding the UI on the website), so the
// pane literally narrates the work that produced it.
//
// We use brainless's ClaudeLogo + a compact identity block rather than the full
// ClaudeHeader welcome box: an active session has scrolled the welcome box off
// the top, and its own border nested inside the pane's focus border read as two
// clashing frames. This matches what a real mid-work roster pane shows.
import { ClaudeLogo } from "@/components/brainless/claude/claude-header";
import { ClaudeMessage } from "@/components/brainless/claude/claude-message";
import { ClaudeTodoList } from "@/components/brainless/claude/claude-todo-list";
import { ClaudeToolCall } from "@/components/brainless/claude/claude-tool-call";
import { ClaudeThinking } from "@/components/brainless/claude/claude-thinking";
import { ClaudePrompt } from "@/components/brainless/claude/claude-prompt";

const GRAY = "#8a8a8a";

// The pasted /goal prompt, parked in the composer — mirrors the real screenshot
// where the launch prompt sits waiting with the cursor after it.
const GOAL =
  "rebuild the roster UI on the website — use brainless for the agent chrome";

export function DemoPane() {
  return (
    <div className="space-y-3 font-mono text-[15px] leading-[1.6] text-[#e6e6e6]">
      {/* Compact identity header — mascot + the three lines Claude Code boots
          with, no welcome-box border to clash with the pane frame. */}
      <div className="flex items-start gap-3">
        <ClaudeLogo scale={3} />
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

      <div className="space-y-3 pt-1">
        <ClaudeMessage role="user">
          rebuild the roster UI on the website so people can see what we&apos;re
          building
        </ClaudeMessage>

        <ClaudeMessage>
          I&apos;ll scaffold shadcn, pull in the brainless Claude Code
          components, then assemble roster&apos;s sidebar and focused pane around
          them.
        </ClaudeMessage>

        <ClaudeTodoList
          todos={[
            { label: "Register the @brainless registry", status: "done" },
            { label: "Build the sidebar + agent cards", status: "active" },
            { label: "Wire the pane to the Claude session", status: "todo" },
          ]}
        />

        <ClaudeToolCall
          tool="Bash"
          arg="bunx shadcn add @brainless/claude-session"
          result="Added 8 components"
        >
          {`Added 8 components:
  claude-header  claude-message  claude-todo-list
  claude-tool-call  claude-diff  claude-permission
  claude-thinking  claude-prompt`}
        </ClaudeToolCall>

        <ClaudeThinking />
      </div>

      <div className="pt-2">
        {/* defaultValue (not value): the composer is a preset read-only-in-spirit
            display, and value-without-onChange is React's controlled-input trap. */}
        <ClaudePrompt defaultValue={GOAL} mode="auto" effort="high" />
      </div>
    </div>
  );
}
