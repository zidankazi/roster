# roster

[![latest release](https://img.shields.io/github/v/release/zidankazi/roster?label=release&color=c0392b)](https://github.com/zidankazi/roster/releases)
[![downloads](https://img.shields.io/github/downloads/zidankazi/roster/total?color=555)](https://github.com/zidankazi/roster/releases)
[![license: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue)](LICENSE)

a terminal multiplexer for Claude Code. run several agents in real terminal
panes and see which one is blocked, working, done, or idle, plus the exact
prompt each one is waiting on.

[install](#install) · [quick start](#quick-start) · [built for Claude Code](#built-for-claude-code) · [docs](docs)

<!-- TODO: capture a real screenshot/gif and drop it here:
![roster](docs/assets/hero.png)
-->

<details>
<summary>text preview of the sidebar (until the screenshot lands)</summary>

```
 agents               1 blocked │ ⠼ claude-code      ✕ │▍ ◉ claude-code       ✕
                                │ ✳ Compiling…         │ Do you want to proceed?
  ◉ claude-code             30s │                      │ ❯ 1. Yes
    Approve command?            │ ╭─ new agent ──────╮ │   2. No
  ⠼ claude-code              5s │ │ ❯ cla            │ │
    compiling roster            │ │ ❯ claude-code    │ │
                                │ ╰──────────────────╯ │
 + new agent                    │                      │
```

</details>

a status dot only tells you an agent stopped. roster tells you it stopped on
`Approve command?`, the verbatim prompt, so you know which pane to jump to
without reading every screen.

- **every agent at a glance.** one colored glyph per agent, triaged so
  whoever's been blocked longest sits on top, each card showing why. a
  finished agent's check pulses until you look, and the card you're on is
  the one inverted card in the stack — light fill, dark text.
- **a real terminal, not a wrapped interpretation.** each agent in its own
  rounded panel, 10k lines of scrollback per pane, full-screen TUIs keep
  their keys, drag to select and copy (OSC 52, works over ssh) — hold the
  drag at a pane's edge and it scrolls on through history — bracketed
  paste for multi-line prompts. mouse-native apps (Claude Code's UI
  included) get the real mouse instead: their own drag-selection,
  scrolling, and copy work exactly as in a bare terminal, relayed to
  your clipboard.
- **mouse and keyboard, both first-class.** click a pane, drag a divider, hit
  the `+ new agent` button, right-click an agent card for a pin-to-top / close
  menu, or drive all of it with tmux-style `ctrl-b` keys.
- **detach, agents keep running.** every pane lives in a background server.
  close the lid, reattach later, even over ssh. sessions survive the UI.
- **reads Claude Code's own hooks.** exact permission asks and state straight
  from Claude, not screen-scraping. auto-approve per agent or across the whole
  fleet, without giving up visibility.
- **one Rust binary, under 4 MB.** no Electron, runs in whatever terminal you
  already use. designed for dark terminals: the chrome pins its grays and
  accent from the fixed 256-color ramp, so your theme can't wash it out.

## install

```sh
curl -fsSL https://raw.githubusercontent.com/zidankazi/roster/main/install.sh | sh
```

or `brew install zidankazi/roster/roster` · `cargo install --git https://github.com/zidankazi/roster roster` · [prebuilt binaries](https://github.com/zidankazi/roster/releases)

prebuilt for macOS (arm64/x86_64) and Linux (x86_64/arm64), checksum-verified,
installs to `~/.local/bin`. the installer takes `ROSTER_VERSION=vX.Y.Z` to pin
a version and `ROSTER_BINDIR=…` to change where it lands.

## quick start

```sh
roster                # launcher; add agents as you go
roster claude claude  # or launch a couple up front
```

run bare and roster opens a welcome screen: pick an agent, or type any command
(it's a real terminal) and hit enter. Claude Code gets a named card with live
state, anything else just runs.

the sidebar rolls every agent into a colored glyph and triages by who needs
you first: blocked (longest wait leading), then done, then idle, working at the
bottom. two layouts: grid tiles every pane, solo shows one full size with the
sidebar as the switcher. toggle with the `grid · solo` pills in the status bar
or by double-clicking a title bar. `ctrl-b` is the prefix; hold it and the
status bar shows the full key palette, and `roster --help` lists it all.

## workspaces

each agent from `+ new agent` opens its own workspace window instead of
splitting the current pane; cycle them with `ctrl-b n`/`p` or click the `⧉`
indicator in the status bar. the sidebar is one list ranked
most-blocked-first across every workspace, so the agent waiting on you rises
to the top no matter where it lives — with more than one workspace each card
carries a `⧉N` tag naming its home. cards and pane title bars name
themselves after the task Claude Code is working on (`fix auth bug`, not
`claude-code`) — the card truncates to its column, the title bar has the
pane's width for it. when Claude Code never broadcasts a task title (a
session started with a slash command, say), the card falls back to Claude
Code's own name for the session from the statusline feed.

## persistent sessions

agents shouldn't die because a window closed. run roster inside a named session
and the panes live in a background server that outlives the UI:

```sh
roster -s work claude    # run claude in the persistent session "work"
# ctrl-b d to detach, or just quit (quitting a session detaches)
roster ls                # sessions still running
roster attach work       # back where you left off, layout restored
roster kill work         # end the session and every agent in it
```

attach from another machine over plain ssh. roster runs a thin stdio proxy on
the remote side, so the session server never touches the network:

```sh
roster attach user@devbox:work   # needs roster installed on devbox
```

## built for Claude Code

roster is built for Claude Code and ships detection for it alone. it doesn't
just watch the screen, it reads Claude Code's own state. register the hooks
(and, optionally, the telemetry feed) once:

```sh
roster --print-hooks        # merge into ~/.claude/settings.json
roster --print-statusline   # same file; model / context left / cost / rate-limit
```

now every pane reports its permission asks directly. the moment Claude wants a
tool, the sidebar shows the verbatim ask, `Bash: cargo test`, on the blocked
card, straight from the `PermissionRequest` hook, and approving clears it just
as precisely. the hooks are silent no-ops for any Claude running outside roster,
so registering them costs nothing; screen detection keeps running underneath
and reconciles if a signal ever goes missing.

because the hook is two-way, roster can answer for you. each card has an `auto`
pill — it appears on the card under your pointer (an armed one stays visible):
flip it (click, or `ctrl-b j` then `a`) and roster approves that pane's next
asks, so it runs uninterrupted but stays observable. that's the difference
from `--dangerously-skip-permissions`, which hides the asks entirely. the
sidebar header's `auto-yes` pill arms the whole fleet at once, and per-card
pills still win, so one sensitive agent can stay manual. with the statusline
feed on, the focused card carries a quiet telemetry line — context
remaining, the busiest rate-limit window, and session cost — and any card's
context badge surfaces on its own (yellow, then red) as that agent nears a
compaction. the feed also drives fleet-level rate-limit awareness: your
account's usage limits are one budget shared by every agent, so the sidebar
pins a footer showing each reported window as a labeled bar (`5h`, `wk`) with
its used share and reset time, colored quiet below 70%, yellow from 70%, red
from 90% — and a toast fires once as a window crosses each threshold, so a
fleet quietly burning through the budget can't surprise you. no feed, no
footer: bridge-less panes keep the exact screen-scraped sidebar.

this is the first slice of a Claude-native attention layer;
[`docs/05-claude-native-attention.md`](docs/05-claude-native-attention.md) is
the spec and roadmap.

## how it compares

|                        | tmux | GUI managers | roster |
|------------------------|------|--------------|--------|
| agent awareness        | —    | ✓            | ✓      |
| shows *why* (reasons)  | —    | —            | ✓      |
| panes, workspaces      | ✓    | ✓            | ✓      |
| lives in your terminal | ✓    | —            | ✓      |
| real terminal views    | ✓    | —            | ✓      |
| mouse-native           | —    | ✓            | ✓      |
| lightweight binary     | ✓    | —            | ✓ (under 4 MB) |
| persistent sessions    | ✓    | —            | ✓      |
| detach / reattach      | ✓    | —            | ✓      |
| remote attach over ssh | ✓ (by hand) | —     | ✓ (built in) |

## configuration

detection is tuned against **Claude Code 2.1** and verified against live
sessions. rules live in
[`crates/roster-detect/agents.toml`](crates/roster-detect/agents.toml),
overridable at `~/.config/roster/agents.toml`; start from the built-in with
`roster --print-config > ~/.config/roster/agents.toml`. set `launch_command` on
an agent to control exactly what the launcher runs, or press **tab** in the
launcher to edit the flags for a one-off (anything you type runs verbatim, so
`claude --continue` works too).

## building from source

two toolchains share the repo and never mix: Cargo owns `crates/`, Bun owns the
`website/` landing page, bridged only by a build-time JSON artifact.

```sh
cargo build
cargo test
```

architecture docs are in [`docs/`](docs); start with
[`docs/00-architecture.md`](docs/00-architecture.md).

## license

[GPL-3.0-only](LICENSE). free to use, study, share, and modify. any distributed
derivative stays under the GPL.
