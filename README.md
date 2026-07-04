# roster

An agent-aware terminal multiplexer. Run your coding agents — Claude Code, Codex,
Aider — in real terminal panes, and see at a glance which one is 🔴 blocked,
🟡 working, 🔵 done, or 🟢 idle, **plus what each one is waiting on**.

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

```sh
brew install zidankazi/roster/roster
```

Or with cargo:

```sh
cargo install --git https://github.com/zidankazi/roster roster
```

Prebuilt binaries for macOS (Apple Silicon + Intel) and Linux (x86_64 +
arm64) are attached to each [release](https://github.com/zidankazi/roster/releases).

## Use

```sh
roster claude "codex exec 'fix the tests'" aider
```

Each command gets a pane; the sidebar shows who's blocked / working / done /
idle and why. Keys are tmux-flavored with a `ctrl-b` prefix — `roster --help`
lists them. Agent detection rules live in
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
