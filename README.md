# roster

An agent-aware terminal multiplexer. Run your coding agents — Claude Code, Codex,
Aider — in real terminal panes, and see at a glance which one is 🔴 blocked,
🟡 working, 🔵 done, or 🟢 idle, **plus what each one is waiting on**.

```
 roster                1 blocked │ ● claude-code        │ ● codex
─────────────────────────────────│ ✳ Compiling…         │ Do you want to proceed?
 ● codex                     30s │                      │ ❯ 1. Yes
   blocked · Approve command?    │ ╭─ new agent ──────╮ │   2. No
 ● claude-code                5s │ │ ❯ cla            │ │
   working · compiling roster    │ │ ❯ claude-code    │ │
                                 │ ╰──────────────────╯ │
```

The sidebar rolls every agent up to a colored glyph — blocked and done float
to the top so whoever needs you is always in view — and, unlike a bare status
dot, shows **the reason**: the exact prompt an agent is blocked on. Every pane
gets a title bar with its agent's live state; the focused pane is highlighted.

Start bare and pick agents interactively — `roster` greets you with the
launcher; `ctrl-b c` opens it any time. Type to filter the configured agents,
or type any command to run it in a new pane. `ctrl-b j` jumps via the sidebar.

**Mouse-native:** click any pane to focus it, click a sidebar card to jump to
that agent, click launcher rows to launch, drag pane dividers to resize, and
scroll the pane under your cursor. Keyboard equivalents exist for everything.

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
| persistent sessions    | ✓    | —            | planned |
| detach / reattach      | ✓    | —            | planned |

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
| `roster-term` | Byte stream → screen grid (via `alacritty_terminal`) |
| `roster-core` | Panes, layout tree, session state |
| `roster-detect` | Agent identification + state heuristics + config |
| `roster-tui` | ratatui rendering: panes + the sidebar |
| `roster` | The binary; wires everything, owns the event loop |

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
roster claude codex   # or give each command its own pane up front
```

The sidebar shows who's blocked / working / done / idle and why. Keys are
tmux-flavored with a `ctrl-b` prefix — `roster --help` lists them. The
sidebar sits on the left by default (`--sidebar right` to flip it). Agent
detection rules live in
[`crates/roster-detect/agents.toml`](crates/roster-detect/agents.toml) and can
be overridden at `~/.config/roster/agents.toml`.

Agent detection is tuned against **Claude Code 2.1**, Codex, and Aider, and
verified against live Claude Code sessions. To customize, start from the
built-in config:

```sh
roster --print-config > ~/.config/roster/agents.toml
```

## Building from source

```sh
cargo build
cargo test
```
