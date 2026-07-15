"use client";

// A faithful, static rebuild of roster's TUI as web chrome, so the landing page
// SHOWS what `brew install` gets you: the window frame, the sidebar that reads
// each agent's state AND the exact reason it's in that state (roster's wedge),
// and a focused pane whose contents are real brainless Claude Code components.
// Purely presentational — no live PTY, no I/O; it's a screenshot you can read.
import { DemoPaneBody, DemoComposer } from "./DemoPane";

// roster's real palette (crates/roster-tui/src/style.rs): a neutral grayscale
// foreground ramp on dark surfaces, a red ACCENT, and the state ramp —
// 🔴 blocked / 🟡 working / 🔵 done / 🟢 idle. NOT tokyo-night blue; the chrome
// is deliberately gray so color means state, not decoration.
const RED = "#df2c2c"; // ACCENT — badges, pane focus border
const BLOCKED = "#fb4b3e"; // state 196
const WORKING = "#e8b73f"; // state 220
const IDLE = "#6fbf5f"; // state 71
const BRIGHT = "#ececec"; // fg 255 — card names, titles
const TEXT = "#bdbdbd"; // fg 250 — body text
const MUTED = "#828282"; // fg 244 — ages, paths, reasons

// Neutral dark surfaces + the inverted selection roster uses: the focused card
// flips to a light bar with dark text (SELECTED_BG 254 / SELECTED_FG 235).
const BG = "#141414"; // SURFACE_BASE 233
const BG_RAISED = "#1b1b1b"; // sidebar / new-agent pad
const TITLEBAR = "#2c2c2c";
const BORDER = "#333333";
const SELECTED_BG = "#e4e4e4"; // 254
const SELECTED_FG = "#1f1f1f"; // 235
const SELECTED_MUTED = "#565656"; // 240

type State = "blocked" | "working" | "idle";

const STATE_COLOR: Record<State, string> = {
  blocked: BLOCKED,
  working: WORKING,
  idle: IDLE,
};

// Darker state shades for the inverted (light) selected card, so the dot and
// state word keep contrast on the near-white bar.
const SELECTED_STATE: Record<State, string> = {
  blocked: "#c62828",
  working: "#8a6d00",
  idle: "#2f7d32",
};

type Agent = {
  title: string;
  state: State;
  /** The exact prompt/verb the agent is sitting on — roster's whole point. */
  reason: string;
  elapsed: string;
  selected?: boolean;
};

// Three agents in three states, blocked first the way roster sorts by
// attention — the blocked card's "Allow …?" is the money shot: state you can
// see, prompt you can act on.
const AGENTS: Agent[] = [
  {
    title: "Tune the detection fixtures",
    state: "blocked",
    reason: "Allow Bash(cargo test)?",
    elapsed: "40s",
  },
  {
    title: "Add shell commands to the launcher",
    state: "idle",
    reason: "waiting for your input",
    elapsed: "2m",
  },
  {
    title: "Rebuild the roster UI on the website",
    state: "working",
    reason: "Herding… (esc to interrupt)",
    elapsed: "53s",
    selected: true,
  },
];

/** How many agents are blocked — surfaced next to the agents header, in red. */
const BLOCKED_COUNT = AGENTS.filter((a) => a.state === "blocked").length;

/** A red pill badge — roster's `auto` / `auto-yes` approval chips. `small` is
    the per-card size; the default is the sidebar-header `auto-yes` chip. */
function Badge({ children, small }: { children: React.ReactNode; small?: boolean }) {
  return (
    <span
      className={`rounded-[4px] font-semibold uppercase leading-none tracking-wide ${
        small ? "px-1.5 py-[3px] text-[10px]" : "px-2 py-[3px] text-[11px]"
      }`}
      style={{ background: RED, color: "#fff" }}
    >
      {children}
    </span>
  );
}

/** One sidebar row, matching roster: a status dot (hollow when idle, filled
    otherwise) + task title + elapsed, then the reason with its `auto` approval
    chip. The focused agent's card gets the full highlighted box. */
function AgentCard({ agent }: { agent: Agent }) {
  const idle = agent.state === "idle";
  const sel = agent.selected;
  // On the inverted (light) selected bar, foregrounds flip dark; the state
  // color darkens so the dot/word stay legible on white.
  const dot = sel ? SELECTED_STATE[agent.state] : STATE_COLOR[agent.state];
  const titleFg = sel ? SELECTED_FG : BRIGHT;
  const metaFg = sel ? SELECTED_MUTED : MUTED;
  const sparkleFg = sel ? SELECTED_FG : BRIGHT;
  return (
    <div
      className="rounded-[5px] px-3 py-2.5"
      style={{ background: sel ? SELECTED_BG : "transparent" }}
    >
      <div className="flex gap-2.5">
        {/* Fixed dot column keeps title and reason on one left edge. Hollow ring
            for idle, filled dot otherwise — roster's own status glyphs. */}
        <span aria-hidden className="mt-[4px] text-[11px] leading-none" style={{ color: dot }}>
          {idle ? "○" : "●"}
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span aria-hidden className="text-[13px]" style={{ color: sparkleFg }}>
              ✳
            </span>
            <span
              className="min-w-0 flex-1 truncate text-[13px] font-semibold"
              style={{ color: titleFg }}
            >
              {agent.title}
            </span>
            <span className="whitespace-nowrap text-[12px]" style={{ color: metaFg }}>
              {agent.elapsed}
            </span>
          </div>
          <div className="mt-1 flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate text-[12px]">
              <span style={{ color: dot }}>{agent.state}</span>
              <span style={{ color: metaFg }}> · {agent.reason}</span>
            </span>
            <Badge small>auto</Badge>
          </div>
        </div>
      </div>
    </div>
  );
}

/** A stacked usage meter — roster's 5h / weekly budget gauges. */
function Meter({ label, pct, note }: { label: string; pct: number; note: string }) {
  return (
    <div className="flex items-center gap-2.5 text-[12px]" style={{ color: MUTED }}>
      <span className="w-5" style={{ color: TEXT }}>
        {label}
      </span>
      <span
        className="h-2.5 w-20 overflow-hidden rounded-[3px]"
        style={{ background: "#3a3a3a" }}
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
          style={{ background: BG, color: TEXT }}
        >
          {/* Title bar */}
          <div
            className="flex h-12 items-center px-5"
            style={{ background: TITLEBAR, borderBottom: "1px solid #00000040" }}
          >
            <div className="flex items-center gap-2.5" aria-hidden>
              <span className="h-3.5 w-3.5 rounded-full" style={{ background: "#ff5f57" }} />
              <span className="h-3.5 w-3.5 rounded-full" style={{ background: "#febc2e" }} />
              <span className="h-3.5 w-3.5 rounded-full" style={{ background: "#28c840" }} />
            </div>
            <div
              className="flex flex-1 items-center justify-center gap-2.5 text-[14px] font-semibold"
              style={{ color: BRIGHT }}
            >
              <span aria-hidden>📁</span>
              <span>cargo run -p roster</span>
            </div>
            <div className="w-[64px]" />
          </div>

          {/* Body: sidebar + focused pane. min-height (not a hard height) so the
              pane grows with its content and never scrolls the composer out of
              view if font metrics render the session taller than expected. */}
          <div className="flex" style={{ minHeight: 560 }}>
            {/* Sidebar */}
            <aside
              className="flex w-[300px] flex-none flex-col"
              style={{ background: BG_RAISED, borderRight: `1px solid ${BORDER}` }}
            >
              <div className="flex-1 overflow-hidden px-4 py-5">
                {/* Workspace + clock */}
                <div className="flex items-baseline justify-between px-1">
                  <span className="text-[16px] font-bold" style={{ color: BRIGHT }}>
                    roster
                  </span>
                  <span className="text-[12px]" style={{ color: MUTED }}>
                    13:12
                  </span>
                </div>
                <div className="mt-1 truncate px-1 text-[12px]" style={{ color: MUTED }}>
                  ~/Desktop/roster
                </div>

                {/* Agents header: the label + a live blocked count (roster
                    surfaces how many need you, in red) + the global toggle. */}
                <div className="mb-2.5 mt-6 flex items-center gap-2.5 px-1">
                  <span className="text-[13px]" style={{ color: TEXT }}>
                    agents
                  </span>
                  {BLOCKED_COUNT > 0 && (
                    <span className="text-[12px] font-semibold" style={{ color: RED }}>
                      {BLOCKED_COUNT} blocked
                    </span>
                  )}
                  <span className="flex-1" />
                  <Badge>auto-yes</Badge>
                </div>

                <div className="space-y-1.5">
                  {AGENTS.map((a) => (
                    <AgentCard key={a.title} agent={a} />
                  ))}
                </div>
              </div>

              {/* Usage meters + new-agent affordance, pinned to the sill */}
              <div
                className="space-y-2.5 px-4 py-5"
                style={{ borderTop: `1px solid ${BORDER}` }}
              >
                <Meter label="5h" pct={2} note="resets 2h57m" />
                <Meter label="wk" pct={45} note="resets 3d" />
                {/* A styled affordance, not a real button — the demo is static,
                    so nothing here should advertise a clickable action. */}
                <div
                  className="mt-2 w-full rounded-[6px] px-3 py-2.5 text-left text-[13px]"
                  style={{ color: TEXT, background: "#262626" }}
                  aria-hidden
                >
                  + new agent
                </div>
              </div>
            </aside>

            {/* Focused pane — red border signals it holds keyboard focus.
                Flex all the way down so the composer pins to the pane bottom. */}
            <div className="flex min-w-0 flex-1 flex-col p-3.5">
              <div
                className="flex flex-1 flex-col overflow-hidden rounded-[8px]"
                style={{ border: `1px solid ${RED}`, background: BG }}
              >
                {/* Pane title: the focused agent's task, in brand red */}
                <div
                  className="flex flex-none items-center gap-2.5 px-4 py-3 text-[13px]"
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

                {/* The session transcript scrolls in the middle (flex-1)… */}
                <div className="min-h-0 flex-1 overflow-hidden px-6 pt-5">
                  <DemoPaneBody />
                </div>
                {/* …and the composer is pinned to the pane bottom (flex-none),
                    calm space between it and the transcript above. */}
                <div className="flex-none px-6 pb-5 pt-4">
                  <DemoComposer />
                </div>
              </div>
            </div>
          </div>

          {/* Status bar */}
          <div
            className="relative flex h-11 items-center justify-center px-4 text-[13px]"
            style={{ background: BG_RAISED, borderTop: `1px solid ${BORDER}`, color: MUTED }}
          >
            <span>
              focused <span style={{ color: TEXT }}>▸ claude</span> ·{" "}
              <span style={{ color: RED }}>ctrl-b</span> keys · then{" "}
              <span style={{ color: RED }}>j</span> jump
            </span>
            <span className="absolute right-4" style={{ color: RED }}>
              <span aria-hidden>⧉ </span>2/2
            </span>
          </div>
        </div>
      </div>
    </section>
  );
}
