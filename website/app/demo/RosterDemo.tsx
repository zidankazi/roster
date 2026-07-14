"use client";

// A faithful, static rebuild of roster's TUI as web chrome, so the landing page
// SHOWS what `brew install` gets you: the window frame, the sidebar that reads
// each agent's state AND the exact reason it's in that state (roster's wedge),
// and a focused pane whose contents are real brainless Claude Code components.
// Purely presentational — no live PTY, no I/O; it's a screenshot you can read.
import { DemoPane } from "./DemoPane";

// Tokyo-night surface + roster's brand red for chrome accents (focus border,
// badges). The state ramp mirrors roster's legend: 🔴 blocked / 🟡 working /
// 🔵 done / 🟢 idle — color carries state at a glance, the reason text carries
// why.
const RED = "#df2c2c";
const BLOCKED = "#f7768e";
const WORKING = "#e0af68";
const IDLE = "#9ece6a";
const BRIGHT = "#c0caf5";
const TEXT = "#a9b1d6";
const MUTED = "#565f89";

type State = "blocked" | "working" | "idle";

const STATE_COLOR: Record<State, string> = {
  blocked: BLOCKED,
  working: WORKING,
  idle: IDLE,
};

type Agent = {
  title: string;
  state: State;
  /** The exact prompt/verb the agent is sitting on — roster's whole point. */
  reason: string;
  elapsed: string;
  selected?: boolean;
};

// Three agents in three states, each with its reason — the blocked card's
// "Allow …?" is the money shot: state you can see, prompt you can act on.
const AGENTS: Agent[] = [
  {
    title: "Add shell commands to the launcher",
    state: "idle",
    reason: "waiting for your input",
    elapsed: "2m",
  },
  {
    title: "Tune the detection fixtures",
    state: "blocked",
    reason: "Allow Bash(cargo test)?",
    elapsed: "40s",
  },
  {
    title: "Rebuild the roster UI on the website",
    state: "working",
    reason: "Herding… (esc to interrupt)",
    elapsed: "53s",
    selected: true,
  },
];

/** A red pill badge — roster's `auto` / `auto-yes` approval chips. */
function Badge({ children }: { children: React.ReactNode }) {
  return (
    <span
      className="rounded-[3px] px-1.5 py-px text-[10px] font-semibold uppercase leading-none tracking-wide"
      style={{ background: RED, color: "#fff" }}
    >
      {children}
    </span>
  );
}

/** One sidebar row: title + elapsed, then the colored state word and its reason. */
function AgentCard({ agent }: { agent: Agent }) {
  const color = STATE_COLOR[agent.state];
  return (
    <div
      className="relative rounded-[5px] px-2.5 py-2"
      style={{
        background: agent.selected ? "#24283b" : "transparent",
        boxShadow: agent.selected ? `inset 2px 0 0 ${RED}` : "none",
      }}
    >
      <div className="flex items-center gap-1.5">
        <span aria-hidden style={{ color }}>
          ●
        </span>
        <span aria-hidden style={{ color: BRIGHT }}>
          ✳
        </span>
        <span className="min-w-0 flex-1 truncate font-semibold" style={{ color: BRIGHT }}>
          {agent.title}
        </span>
        <span className="whitespace-nowrap text-[11px]" style={{ color: MUTED }}>
          <span aria-hidden>⧉ </span>
          {agent.elapsed}
        </span>
      </div>
      <div className="mt-0.5 flex items-center gap-2 pl-[22px]">
        <span className="min-w-0 flex-1 truncate">
          <span style={{ color }}>{agent.state}</span>
          <span style={{ color: MUTED }}> · {agent.reason}</span>
        </span>
        <Badge>auto</Badge>
      </div>
    </div>
  );
}

/** A stacked usage meter — roster's 5h / weekly budget gauges. */
function Meter({ label, pct, note }: { label: string; pct: number; note: string }) {
  return (
    <div className="flex items-center gap-2 text-[11px]" style={{ color: MUTED }}>
      <span className="w-4" style={{ color: TEXT }}>
        {label}
      </span>
      <span
        className="h-2 w-16 overflow-hidden rounded-[2px]"
        style={{ background: "#2a2b3a" }}
        aria-hidden
      >
        <span className="block h-full" style={{ width: `${pct}%`, background: TEXT }} />
      </span>
      <span>
        {pct}% · {note}
      </span>
    </div>
  );
}

export function RosterDemo() {
  return (
    <section className="roster-demo" aria-label="What roster looks like">
      <div className="roster-demo-scroll">
        <div
          className="roster-window"
          style={{ background: "#16161e", color: TEXT }}
        >
          {/* Title bar */}
          <div
            className="flex h-9 items-center px-3.5"
            style={{ background: "#30303c", borderBottom: "1px solid #00000040" }}
          >
            <div className="flex items-center gap-2" aria-hidden>
              <span className="h-3 w-3 rounded-full" style={{ background: "#ff5f57" }} />
              <span className="h-3 w-3 rounded-full" style={{ background: "#febc2e" }} />
              <span className="h-3 w-3 rounded-full" style={{ background: "#28c840" }} />
            </div>
            <div
              className="flex flex-1 items-center justify-center gap-2 text-[13px] font-semibold"
              style={{ color: BRIGHT }}
            >
              <span aria-hidden>📁</span>
              <span>cargo run -p roster</span>
            </div>
            <div className="w-[52px]" />
          </div>

          {/* Body: sidebar + focused pane. min-height (not a hard height) so the
              pane grows with its content and never scrolls the composer out of
              view if font metrics render the session taller than expected. */}
          <div className="flex" style={{ minHeight: 620 }}>
            {/* Sidebar */}
            <aside
              className="flex w-[248px] flex-none flex-col"
              style={{ background: "#1a1b26", borderRight: "1px solid #2a2b3a" }}
            >
              <div className="flex-1 overflow-hidden px-2.5 py-3">
                {/* Workspace + clock */}
                <div className="flex items-baseline justify-between px-1.5">
                  <span className="text-[15px] font-bold" style={{ color: BRIGHT }}>
                    roster
                  </span>
                  <span className="text-[11px]" style={{ color: MUTED }}>
                    13:12
                  </span>
                </div>
                <div className="truncate px-1.5 text-[11px]" style={{ color: MUTED }}>
                  ~/Desktop/roster
                </div>

                {/* Agents header + global approval toggle */}
                <div className="mt-4 mb-1 flex items-center justify-between px-1.5">
                  <span
                    className="text-[11px] font-semibold uppercase tracking-wide"
                    style={{ color: TEXT }}
                  >
                    agents
                  </span>
                  <Badge>auto-yes</Badge>
                </div>

                <div className="space-y-0.5">
                  {AGENTS.map((a) => (
                    <AgentCard key={a.title} agent={a} />
                  ))}
                </div>
              </div>

              {/* Usage meters + new-agent affordance, pinned to the sill */}
              <div
                className="space-y-1.5 px-3 py-3"
                style={{ borderTop: "1px solid #2a2b3a" }}
              >
                <Meter label="5h" pct={2} note="resets 2h57m" />
                <Meter label="wk" pct={45} note="resets 3d" />
                {/* A styled affordance, not a real button — the demo is static,
                    so nothing here should advertise a clickable action. */}
                <div
                  className="mt-1 w-full rounded-[5px] py-1.5 text-left text-[12px]"
                  style={{ color: TEXT, background: "#24283b" }}
                  aria-hidden
                >
                  <span className="px-2">+ new agent</span>
                </div>
              </div>
            </aside>

            {/* Focused pane — red border signals it holds keyboard focus */}
            <div className="min-w-0 flex-1 p-2.5">
              <div
                className="flex h-full flex-col overflow-hidden rounded-[6px]"
                style={{ border: `1px solid ${RED}`, background: "#16161e" }}
              >
                {/* Pane title: the focused agent's task, in brand red */}
                <div
                  className="flex flex-none items-center gap-2 px-3 py-1.5 text-[12px]"
                  style={{ borderBottom: `1px solid ${RED}33` }}
                >
                  <span aria-hidden style={{ color: MUTED }}>
                    ○
                  </span>
                  <span className="min-w-0 flex-1 truncate font-semibold" style={{ color: RED }}>
                    <span aria-hidden>✳ </span>Rebuild the roster UI on the website
                  </span>
                  <span aria-hidden style={{ color: MUTED }}>
                    ×
                  </span>
                </div>

                {/* The Claude Code session itself — all brainless components */}
                <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
                  <DemoPane />
                </div>
              </div>
            </div>
          </div>

          {/* Status bar */}
          <div
            className="relative flex h-8 items-center justify-center px-3 text-[12px]"
            style={{ background: "#1a1b26", borderTop: "1px solid #2a2b3a", color: MUTED }}
          >
            <span>
              focused <span style={{ color: TEXT }}>▸ claude</span> ·{" "}
              <span style={{ color: RED }}>ctrl-b</span> keys · then{" "}
              <span style={{ color: RED }}>j</span> jump
            </span>
            <span className="absolute right-3" style={{ color: RED }}>
              <span aria-hidden>⧉ </span>2/2
            </span>
          </div>
        </div>
      </div>
    </section>
  );
}
