# roster — architecture overview

*Read this first. It's the map. Every other doc in `/docs` drills into one piece named here.*

`roster` is a terminal multiplexer built for Claude Code, written in Rust. It runs Claude Code in real terminal panes and surfaces, in a sidebar, which agent is 🔴 blocked / 🟡 working / 🔵 done / 🟢 idle — and **what each one is waiting on**. It never authenticates anyone and never calls a model API; it spawns the user's already-logged-in Claude Code processes as children, exactly as tmux would. Claude Code is the only shipped agent; the Claude-native attention layer in [`05-claude-native-attention.md`](05-claude-native-attention.md) is where it is headed. (Any command still runs in a pane — it is a real terminal — but Claude Code is what roster detects and is built for.)

## What roster is (and is not)

**Is:** a multiplexer for Claude Code — panes running Claude Code (or any command), a live explainable-state sidebar that shows the reason each agent is blocked, jump-to-pane, and persistent sessions you can detach from and re-attach to over ssh. Detection is screen-based today; reading Claude Code's own hooks and statusline is the committed direction ([`05-claude-native-attention.md`](05-claude-native-attention.md)).

**Is not:** no git worktrees, no diff/review UI, and — deliberately — no agent-orchestration socket API for agents to drive the multiplexer. That last one is a different product — agents watching agents; keeping the human in the cockpit is ours (see docs/05). Not a security boundary: it spawns your already-logged-in agents as child processes, exactly as tmux would.

Agents launched from **+ new agent** open in their own workspace window
rather than splitting the current pane; the sidebar groups cards by
workspace and doubles as the switcher — its header also carries the
**`auto-yes`** fleet toggle to arm auto-approve for every agent at once.
Each workspace renders as either a tiled grid (every pane visible) or solo
(one pane full-size, the sidebar left as the way to flip between agents).
This is UI surface, not architecture — see README.md for the click targets
and keybindings.

## Repo layout

```
roster/
  Cargo.toml            # Rust workspace root — governs crates/*
  .gitignore            # ignores target/ AND website/node_modules, .next
  README.md             # two-toolchain split explained up top
  crates/
    roster-pty/         # PTY allocation + child process spawn
    roster-term/        # alacritty_terminal wiring: bytes -> screen grid
    roster-core/        # panes, layout tree, session state
    roster-detect/      # agent identification + state heuristics + config
    roster-proto/       # framed client/server protocol for persistent sessions
    roster-tui/         # ratatui rendering: panes + the sidebar
    roster/             # the binary; wires everything, owns the event loop
  docs/                 # THIS folder — plain .md architecture docs
  website/              # Next.js landing page — ISOLATED Bun project
```

## Two toolchains, one git repo

This is a monorepo in the loosest sense: one git repo, two independent build systems that do not share a package manager.

- **Cargo** owns everything under `crates/`. The workspace root `Cargo.toml` lists them as members.
- **Bun** owns `website/` only. It has its own `package.json` and lockfile and knows nothing about Rust.

Do **not** try to unify them. Cargo should never see `node_modules`; Bun should never see `target/`. If the site ever needs data from the Rust side, the Rust build emits a JSON artifact the site reads at build time — that's the only bridge. Keep them isolated.

## The crate split — why it exists

The split is not cosmetic. It is the mechanism that lets you hand work to agents safely while you're away. Each crate is tagged in its own doc as **agent-safe** or **do-at-keyboard**:

| Crate | Role | Who builds it |
|---|---|---|
| `roster-pty` | Spawn agents in pseudo-terminals; pipe bytes in/out | **do-at-keyboard** |
| `roster-term` | Parse the raw byte stream into a screen grid (via `alacritty_terminal`) | **do-at-keyboard** |
| `roster-core` | Pane/window/layout model, session state, focus | agent-safe |
| `roster-detect` | Identify agent panes; classify state; load per-agent config | agent-safe |
| `roster-proto` | Framed client/server wire protocol for persistent sessions | agent-safe |
| `roster-tui` | Render panes and the sidebar with `ratatui` | agent-safe |
| `roster` | The binary: event loop tying PTY output → term → detect → tui | mostly do-at-keyboard |

**The rule for unattended runs:** point Claude Code at `roster-core`, `roster-detect`, and `roster-tui` (plus their tests). These are well-specified, verifiable, and don't touch the nondeterministic terminal plumbing. Leave `roster-pty`, `roster-term`, and the wiring in `roster` for when you're at the keyboard — those are the pieces where "looks like it works" and "actually works" diverge, and you can't verify them remotely.

## Data flow (one pass of the loop)

```
agent process --bytes--> roster-pty --raw stream--> roster-term --screen grid-->
      roster-detect (classify state + extract reason) --> roster-core (update model) -->
      roster-tui (repaint panes + sidebar)
```

For Claude Code panes there is a second, authoritative signal path: Claude
Code hooks (inheriting `ROSTER_PANE`/`ROSTER_HOOK_SOCK` from the pane's
environment) invoke `roster _hook`, which sends `HookBlocked`/`HookClear`
frames over a unix socket into the same loop — exact permission asks, no
scraping. The statusline feed rides the same socket: Claude Code pipes its
session JSON to `roster _statusline`, which forwards it verbatim as a
`Statusline` frame; the client parses it into telemetry (model, context %,
cost, rate limits). See
[`05-claude-native-attention.md`](05-claude-native-attention.md).

`roster-detect` reads the *parsed grid* from `roster-term`, not raw bytes — that's why it's agent-safe and testable: you can feed it fixture grids and assert the state, with no PTY in the loop.

## The wedge (keep this sharp)

A status-only tool shows a colored dot; `roster` shows the dot **plus the reason** — "blocked: *Allow edit to config.ts?*". State-with-explanation is the distinctive spine. The lane is real and contested, so v1's job is to nail that one differentiator and stay tight, not to out-feature anyone.

**Where this is heading (post-persistence):** the reason spine widens into a Claude-native attention layer — read Claude Code's own hooks + statusline instead of scraping pixels, and organize the UI around who needs you and why. This is the committed strategic direction; **[05-claude-native-attention.md](05-claude-native-attention.md) is the north star** for what we build next and how it sets roster apart. Read it before starting new feature work.

## Where to go next

- `01-crates.md` — every crate's responsibility, public types, and boundaries.
- `02-state-detection.md` — the heart: how state and reason are derived, debouncing, per-agent config.
- `03-build-sequence.md` — the order to build in, and the agent-safe vs keyboard split per milestone.
- `04-website.md` — the Next.js landing page and how it stays isolated.
- `05-claude-native-attention.md` — **the strategic direction after persistence**: the Claude-native attention layer, what sets it apart, and the phased plan. Start here for new feature work.
