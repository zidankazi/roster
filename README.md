# roster

A terminal multiplexer for Claude Code. Run several Claude Code agents in
real terminal panes and see at a glance which one is 🔴 blocked, 🟡 working,
🔵 done, or 🟢 idle, **plus what each one is waiting on**.

```
 agents               1 blocked │ ⠼ claude-code      ✕ │▎ ◉ claude-code       ✕
                                │ ✳ Compiling…         │ Do you want to proceed?
  ◉ claude-code             30s │                      │ ❯ 1. Yes
    blocked · Approve command?  │ ╭─ new agent ──────╮ │   2. No
  ⠼ claude-code              5s │ │ ❯ cla            │ │
    working · compiling roster  │ │ ❯ claude-code    │ │
                                │ ╰──────────────────╯ │
 + new agent                    │                      │
```

The sidebar rolls every agent up to a colored glyph — blocked and done float
to the top so whoever needs you is always in view — and, unlike a bare status
dot, shows **the reason**: the exact prompt an agent is blocked on. Every pane
gets a title bar with its agent's live state; the focused pane is highlighted.

**Use it like an app — no hotkeys to learn.** Every action is a visible click
target: click a pane to focus it, a sidebar card to jump to that agent, the
pinned **+ new agent** button to open the launcher, a title bar's **✕** to
close that pane (closing a live agent asks first, in a real dialog — a stray
click won't kill it). Drag the dividers to resize. An exited pane shows a
card with **restart** and **close** buttons; launch failures arrive as
dismissable toasts, not buried status text. Closing the last pane quits.

**A real terminal underneath.** Every pane keeps 10,000 lines of scrollback —
wheel-scroll any pane to read history (an `↑ n` chip shows how far back you
are; typing snaps back to live). Full-screen TUIs still get their arrow keys.
Drag across pane text to select it; release copies it to your clipboard via
OSC 52, which works over ssh too. Pastes arrive bracketed, so multi-line
prompts don't self-execute line by line.

**Two layouts.** The grid tiles every pane; **solo** shows one agent at a
time, full size, with the sidebar as the switcher — click cards on the left
to flip between agents. Switch layouts with the `grid · solo` control at the
bottom of the sidebar (it appears once there's more than one pane), or
double-click any pane's title bar to toggle solo, like maximizing a window.

Start bare — `roster` opens a welcome screen: the wordmark over an agent
picker. Click a row (or type to filter and press enter) and that agent takes
over the window. Type `claude` (or any command — it's a real terminal) and
enter runs it in a pane. Claude Code gets a named card and live state
detection; anything else just runs.

Each agent launched from the **+ new agent** button opens in its own
workspace window rather than splitting the current pane. The sidebar groups
cards by workspace — click a workspace header to jump there (shell-only
windows included), click the `⧉ 2/3` indicator in the status bar or press
`ctrl-b n`/`p` to cycle.

**Workspaces name themselves after the task.** Headers pick up the terminal
title the agent broadcasts — Claude Code sets it to what it's working on —
so the sidebar reads `1 · fix auth bug`, not `workspace 1`. Double-click a
header (or `ctrl-b ,`) to set your own name; an empty rename goes back to
automatic. Manual names persist across detach/reattach.

Keyboard equivalents exist for everything (`ctrl-b` is the prefix — `c` new
agent, `n`/`p` windows, `z` solo, `j` jump, `o` focus, `x` close, `d`
detach, `q` quit); the status bar keeps the hints on screen.

## Persistent sessions

Agents shouldn't die because a terminal window closed. Run roster inside a
named session and every pane lives in a background server that survives the
UI — detach, close the laptop lid, reattach later; the agents kept working
and the sidebar rebuilds itself, states and all:

```sh
roster -s work claude    # run claude in the persistent session "work"
# … ctrl-b d to detach (or just quit — quitting a session detaches) …
roster ls                # sessions still running
roster attach work       # back where you left off, layout restored
roster kill work         # end the session and every agent in it
```

Attach from another machine over plain ssh — roster runs a thin stdio proxy
on the remote side, so the session server never touches the network:

```sh
roster attach user@devbox:work   # needs roster installed on devbox
```

## How it compares

|                        | tmux | GUI managers | roster |
|------------------------|------|--------------|--------|
| agent awareness        | —    | ✓            | ✓      |
| shows *why* (reasons)  | —    | —            | ✓      |
| panes, workspaces      | ✓    | ✓            | ✓      |
| lives in your terminal | ✓    | —            | ✓      |
| real terminal views    | ✓    | —            | ✓      |
| mouse-native           | —    | ✓            | ✓      |
| lightweight binary     | ✓    | —            | ✓ (~4 MB) |
| persistent sessions    | ✓    | —            | ✓      |
| detach / reattach      | ✓    | —            | ✓      |
| remote attach over ssh | ✓ (by hand) | —     | ✓ (built in) |

## Built for Claude Code

roster is built exclusively for Claude Code — it's the only agent it ships
detection for. Today that detection reads the screen; the direction is to read
Claude Code's own state instead — its hooks and statusline — for exact *blocked
/ working / done* and the things the screen never shows: context left, cost, and
the tool it's about to run. That Claude-native attention layer is the roadmap;
[`docs/05-claude-native-attention.md`](docs/05-claude-native-attention.md) is
the spec. (Any command still runs in a pane — roster is a real terminal — but
Claude Code is what it detects and is built for.)

## Two toolchains, one repo

This repo holds two independent build systems that do not share a package manager:

- **Cargo** owns everything under [`crates/`](crates). The workspace root
  `Cargo.toml` lists the members.
- **pnpm** owns [`website/`](website) only — the Next.js landing page, with its
  own `package.json` and lockfile.

They stay isolated. Cargo never sees `node_modules`; pnpm never sees `target/`.
If the site ever needs data from the Rust side, the Rust build emits a JSON
artifact the site reads at build time — that is the only bridge.

## Crates

| Crate | Role |
|---|---|
| `roster-pty` | PTY allocation + agent child-process spawn |
| `roster-term` | Byte stream → screen grid + scrollback (via `alacritty_terminal`) |
| `roster-core` | Panes, layout tree, session state + snapshot/restore |
| `roster-detect` | Agent identification + state heuristics + config |
| `roster-proto` | Framed client/server protocol for persistent sessions |
| `roster-tui` | ratatui rendering: panes, sidebar, dialogs, toasts |
| `roster` | The binary; the event loop, and the session server |

Architecture docs live in [`docs/`](docs) — start with
[`docs/00-architecture.md`](docs/00-architecture.md).

## Install

One line, prebuilt binary, no toolchain needed (macOS arm64/x86_64, Linux
x86_64/arm64 — checksum-verified, installs to `~/.local/bin`):

```sh
curl -fsSL https://raw.githubusercontent.com/zidankazi/roster/main/install.sh | sh
```

Homebrew:

```sh
brew install zidankazi/roster/roster
```

Cargo:

```sh
cargo install --git https://github.com/zidankazi/roster roster
```

Prebuilt tarballs are attached to each
[release](https://github.com/zidankazi/roster/releases); the installer takes
`ROSTER_VERSION=vX.Y.Z` to pin one and `ROSTER_BINDIR=…` to change the
destination.

## Use

```sh
roster                # start with the launcher, add agents as you go
roster claude claude  # or launch several Claude Code agents up front
```

The sidebar shows who's blocked / working / done / idle and why. Keys are
tmux-flavored with a `ctrl-b` prefix — `roster --help` lists them. The
sidebar sits on the left by default (`--sidebar right` to flip it). Agent
detection rules live in
[`crates/roster-detect/agents.toml`](crates/roster-detect/agents.toml) and can
be overridden at `~/.config/roster/agents.toml`.

Detection is tuned against **Claude Code 2.1** and verified against live
sessions. To customize — or add your own agent for a pane — start from the
built-in config:

```sh
roster --print-config > ~/.config/roster/agents.toml
```

**Launching with flags.** Set `launch_command` on an agent to control
exactly what the launcher runs — flags included:

```toml
[claude-code]
match_command = ["claude"]
launch_command = "claude --dangerously-skip-permissions"
```

For a one-off, press **tab** in the launcher: it expands the selected
agent's command into the input so you can edit flags before hitting enter.
(Anything you type in the launcher runs verbatim, so `claude --continue`
always works too.)

## Building from source

```sh
cargo build
cargo test
```
