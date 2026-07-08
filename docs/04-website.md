# 04 — website

*The `/website` landing page: Next.js, its own Bun project, fully isolated
from Cargo. For a tool like this the landing page is not a side quest — it's
the pitch. "The demo is the product" applies here more than anywhere.*

## Isolation rules (do not violate)

`website/` is a self-contained **Bun** project. It shares the git repo with
the Rust crates and nothing else.

- Its own `package.json` and `bun.lock` live in `website/`, never at the
  repo root. The lockfile in the tree is the package-manager authority —
  never introduce a second lockfile or a second manager.
- The root `Cargo.toml` does **not** reference it. Cargo never sees
  `node_modules`; Bun never sees `target/`.
- Root `.gitignore` covers both worlds (`target/`, `website/node_modules/`,
  `website/.next/`); `website/.gitignore` additionally covers the site's own
  build output (`out/`, `next-env.d.ts`).
- The only allowed bridge: if the site needs data from the Rust side (a
  version, a feature table), the Rust build emits a JSON file the site
  imports at build time. Never a shared package manager, never a runtime
  call from site to binary.

## What it is

Next.js 15 (App Router) + React 19, TypeScript strict. Everything lives
under `app/` — components sit next to the one route that uses them; there is
no separate `components/` tree.

```
website/
  package.json         # scripts are plain `next dev/build/start/lint`
  bun.lock             # Bun's lockfile — the authority (see isolation rules)
  vercel.json          # deploy config; skips builds when website/ didn't change
  next.config.mjs      # default-empty today
  tsconfig.json
  app/
    layout.tsx         # metadata + Navbar wrapper
    page.tsx           # the landing page: Wordmark → Tagline → InstallCommand
    globals.css        # all styling — plain CSS, no framework
    Navbar.tsx         # brand left; docs + GitHub links right
    Wordmark.tsx       # the animated ASCII wordmark
    Tagline.tsx        # the pitch lines, animated Claude mark inline
    ClaudeMark.tsx     # Lottie-rendered Claude mark (asset: claude-lottie.json)
    InstallCommand.tsx # Homebrew/Script toggle, copy button, red sweep
```

Two deliberate brand bridges — keep them true:

- **The wordmark is the binary's wordmark.** `Wordmark.tsx` embeds
  byte-for-byte the figlet Georgia11 art roster paints on launch
  (`crates/roster-tui/src/launcher.rs`). If the TUI art ever changes, the
  site copy changes in the same change.
- **The tagline is the positioning.** "Terminal multiplexer for Claude Code
  agents" — the same story as the README's first line and docs/05. Don't
  let the site drift back into generic any-agent language the repo already
  moved away from.

## One screen, one pitch

Keep it to a single focused screen. What carries it:

1. **Wordmark + tagline** — who this is for, in two lines.
2. **Install** — `brew install zidankazi/roster/roster` with a copy button,
   the default of a two-segment toggle (Homebrew / Script). Homebrew leads
   because it's the clean, trusted line; the script (`curl … | sh`) is the
   fallback for machines without brew. Keep it to these two — the toggle
   swaps one line in place, it is not a doorway to a methods wall. If someone
   reads the tagline and copies the line, the page did its job.
3. **The demo cast — still the missing centerpiece.** An asciinema
   recording of several Claude Code agents running, the sidebar lighting up
   with reasons, and a jump straight to the blocked one. That recording is
   the single highest-leverage asset in the whole project — record it the
   moment the sidebar looks good. A great cast with a rough binary beats a
   polished binary nobody can see working.
4. If a comparison section ever lands, keep it generic — tmux / GUI
   managers / status-only tools, matching the README table. Never name
   specific competing products in the repo.

Resist a features wall. The demo and the one-line install carry it;
everything else is noise.

## Build, run, deploy

```sh
cd website
bun install
bun run dev      # local convention: --port 3002, clear of other dev servers
bun run build    # the gate for any site change — run it before proposing one
```

Deployment is Vercel, rooted at `website/`; `vercel.json`'s `ignoreCommand`
skips deploys for commits that didn't touch the directory.

## Later, maybe

**MDX docs.** Next.js was chosen over plain HTML to leave room for a
rendered docs site (Markdown pages with live components) later. Nothing is
configured for it today — `next.config.mjs` is empty — and `/docs` stays
plain `.md` regardless: those files are read by agents, not browsers. Don't
build a docs site before the demo cast exists.
