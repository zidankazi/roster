# roster — architecture overview

*Read this first. It's the map. Every other doc in `/docs` drills into one piece named here.*

`roster` is an agent-aware terminal multiplexer written in Rust. It runs the user's own coding-agent CLIs (Claude Code, Codex, Aider, etc.) in real terminal panes and surfaces, in a sidebar, which agent is 🔴 blocked / 🟡 working / 🔵 done / 🟢 idle — and **what each one is waiting on**. It never authenticates anyone and never calls a model API; it spawns the user's already-logged-in agents as child processes, exactly as tmux would.

## What v1 is (and is not)

**Is:** in-process multiplexer — panes running the user's agents, a live explainable-state sidebar, jump-to-pane, per-agent detection config.

**Is not (v1 non-goals — do not build these yet):** no detach/reattach persistence, no git worktrees, no diff/review UI, no remote/ssh logic of its own, no plugin/socket API, not a security boundary. Persistence is the first thing after v1, but it is *not* v1.

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
    roster-tui/         # ratatui rendering: panes + the sidebar
    roster/             # the binary; wires everything, owns the event loop
  docs/                 # THIS folder — plain .md architecture docs
  website/              # Next.js landing page — ISOLATED pnpm project
```

## Two toolchains, one git repo

This is a monorepo in the loosest sense: one git repo, two independent build systems that do not share a package manager.

- **Cargo** owns everything under `crates/`. The workspace root `Cargo.toml` lists them as members.
- **pnpm** owns `website/` only. It has its own `package.json` and lockfile and knows nothing about Rust.

Do **not** try to unify them. Cargo should never see `node_modules`; pnpm should never see `target/`. If the site ever needs data from the Rust side, the Rust build emits a JSON artifact the site reads at build time — that's the only bridge. Keep them isolated.

## The crate split — why it exists

The split is not cosmetic. It is the mechanism that lets you hand work to agents safely while you're away. Each crate is tagged in its own doc as **agent-safe** or **do-at-keyboard**:

| Crate | Role | Who builds it |
|---|---|---|
| `roster-pty` | Spawn agents in pseudo-terminals; pipe bytes in/out | **do-at-keyboard** |
| `roster-term` | Parse the raw byte stream into a screen grid (via `alacritty_terminal`) | **do-at-keyboard** |
| `roster-core` | Pane/window/layout model, session state, focus | agent-safe |
| `roster-detect` | Identify agent panes; classify state; load per-agent config | agent-safe |
| `roster-tui` | Render panes and the sidebar with `ratatui` | agent-safe |
| `roster` | The binary: event loop tying PTY output → term → detect → tui | mostly do-at-keyboard |

**The rule for unattended runs:** point Claude Code at `roster-core`, `roster-detect`, and `roster-tui` (plus their tests). These are well-specified, verifiable, and don't touch the nondeterministic terminal plumbing. Leave `roster-pty`, `roster-term`, and the wiring in `roster` for when you're at the keyboard — those are the pieces where "looks like it works" and "actually works" diverge, and you can't verify them remotely.

## Data flow (one pass of the loop)

```
agent process --bytes--> roster-pty --raw stream--> roster-term --screen grid-->
      roster-detect (classify state + extract reason) --> roster-core (update model) -->
      roster-tui (repaint panes + sidebar)
```

`roster-detect` reads the *parsed grid* from `roster-term`, not raw bytes — that's why it's agent-safe and testable: you can feed it fixture grids and assert the state, with no PTY in the loop.

## The wedge (keep this sharp)

herdr shows a colored dot; `roster` shows the dot **plus the reason** — "blocked: *Allow edit to config.ts?*". State-with-explanation is the distinctive spine. The lane is real and contested (rmux is doing from-scratch-Rust-multiplexer-for-agents too), so v1's job is to nail that one differentiator and stay tight, not to out-feature anyone.

**Where this is heading (post-persistence):** the reason spine widens into a Claude-native attention layer — read Claude Code's own hooks + statusline instead of scraping pixels, and organize the UI around who needs you and why. This is the committed strategic direction; **[05-claude-native-attention.md](05-claude-native-attention.md) is the north star** for what we build next and how it separates us from herdr. Read it before starting new feature work.

## Where to go next

- `01-crates.md` — every crate's responsibility, public types, and boundaries.
- `02-state-detection.md` — the heart: how state and reason are derived, debouncing, per-agent config.
- `03-build-sequence.md` — the order to build in, and the agent-safe vs keyboard split per milestone.
- `04-website.md` — the Next.js landing page and how it stays isolated.
- `05-claude-native-attention.md` — **the strategic direction after persistence**: the Claude-native attention layer, how it differs from herdr, and the phased plan. Start here for new feature work.
