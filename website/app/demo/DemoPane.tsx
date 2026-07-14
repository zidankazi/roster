"use client";

// The Claude Code session shown inside roster's focused pane. Everything the
// agent "prints" — header, messages, plan, tool call, thinking line, composer —
// is a brainless component, so the demo shows the same accessible React chrome
// (details/listbox/aria-live) roster is built to watch, never a <pre> dump. The
// turn is themed to *this* task (rebuilding the UI on the website), so the pane
// literally narrates the work that produced it.
import { ClaudeHeader } from "@/components/brainless/claude/claude-header";
import { ClaudeMessage } from "@/components/brainless/claude/claude-message";
import { ClaudeTodoList } from "@/components/brainless/claude/claude-todo-list";
import { ClaudeToolCall } from "@/components/brainless/claude/claude-tool-call";
import { ClaudeThinking } from "@/components/brainless/claude/claude-thinking";
import { ClaudePrompt } from "@/components/brainless/claude/claude-prompt";

// The pasted /goal prompt, shown parked in the composer — mirrors the real
// screenshot where the launch prompt sits waiting with the cursor after it.
const GOAL =
  "rebuild the roster UI on the website — use brainless for the agent chrome";

export function DemoPane() {
  return (
    <div className="space-y-3 font-mono text-[13px] leading-[1.6] text-[#c0caf5]">
      <ClaudeHeader
        version="v2.1.209"
        user="Zidan"
        model="Fable 5 with high effort · Claude Max"
        org="~/Desktop/roster"
        cwd="~/Desktop/roster/website"
        tips={["Ask roster which agent is blocked and why"]}
        whatsNew={[
          "Right-click a sidebar card for pin / close",
          "Workspace + clock now sit above the agents list",
        ]}
      />

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
