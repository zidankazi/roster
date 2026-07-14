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

Next.js 15 (App Router) + React 19, TypeScript strict. App-specific
components sit next to the one route that uses them under `app/`. The one
exception is the vendored `components/brainless/` tree (see "The demo" below):
those come from a shadcn registry and land where the registry's target paths
put them.

```
website/
  package.json         # scripts are plain `next dev/build/start/lint`
  bun.lock             # Bun's lockfile — the authority (see isolation rules)
  components.json      # shadcn config: aliases + the @brainless registry
  postcss.config.mjs   # loads @tailwindcss/postcss (Tailwind v4)
  vercel.json          # deploy config; skips builds when website/ didn't change
  next.config.mjs      # default-empty today
  tsconfig.json
  lib/
    utils.ts           # shadcn's cn() helper — brainless components import it
  components/
    brainless/         # vendored Claude Code UI from the @brainless registry
  app/
    layout.tsx         # metadata + Navbar wrapper
    page.tsx           # landing: Wordmark → Tagline → InstallCommand → RosterDemo
    globals.css        # site styling (plain CSS) + a Tailwind v4 @import for brainless
    Navbar.tsx         # brand left; docs + GitHub links right
    Wordmark.tsx       # the animated ASCII wordmark
    Tagline.tsx        # the pitch lines, animated Claude mark inline
    ClaudeMark.tsx     # Lottie-rendered Claude mark (asset: claude-lottie.json)
    InstallCommand.tsx # Homebrew/Script toggle, copy button, red sweep
    demo/
      RosterDemo.tsx   # the roster window rebuilt as web chrome (sidebar + pane)
      DemoPane.tsx     # the focused pane's contents, composed from brainless
```

The site's own chrome is still plain CSS in `globals.css` (the `.roster-demo*`
and `.roster-window` classes frame the demo). Tailwind is pulled in **only**
so the brainless components render — `globals.css` opens with
`@import "tailwindcss"` plus the shadcn theme tokens `shadcn init` wrote. Don't
Tailwind-ify the hand-written landing chrome; the two styling systems coexist,
they don't merge.

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
3. **The demo — roster rebuilt as web chrome.** `RosterDemo.tsx` renders the
   whole window on the page below the install line: the title bar, the sidebar
   reading three agents' state **and reason** (🟢 idle / 🔴 blocked /
   🟡 working, each with the exact prompt or verb it's sitting on — roster's
   wedge), the usage meters, and a focused pane. The pane's contents
   (`DemoPane.tsx`) are real brainless Claude Code components — header,
   messages, todo list, tool call, thinking line, composer — themed to *this*
   task, so the pane narrates the work that built it. It's static: no PTY, no
   I/O, a screenshot you can read and select. A live asciinema cast of several
   real agents is still the higher-leverage asset to add once the sidebar
   looks good; this hand-built demo carries the page until then.

   **brainless is a vendored dependency.** The Claude Code components come from
   the `@brainless` shadcn registry (registered in `components.json`). To add
   or refresh one: `bunx shadcn@latest add @brainless/<name>`. They land under
   `components/brainless/` and are Tailwind-styled — that is why Tailwind and
   shadcn's tokens exist in the project at all. Keep the components' semantics
   (`details`, `listbox`, `radiogroup`, `aria-live`) intact; never flatten
   them back into a `<pre>`.
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
skips deploys for commits that didn't touch the directory (its `git diff
... -- .` is relative to the Vercel dashboard's Root Directory setting,
`website/` — an out-of-repo config the claim depends on).

## Later, maybe

**MDX docs.** Next.js was chosen over plain HTML to leave room for a
rendered docs site (Markdown pages with live components) later. Nothing is
configured for it today — `next.config.mjs` is empty — and `/docs` stays
plain `.md` regardless: those files are read by agents, not browsers. Don't
build a docs site before the demo cast exists.
