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
  public/
    hero-cyclist.webp  # the hero backdrop photo (see "The hero" below)
  app/
    layout.tsx         # metadata + Navbar wrapper
    page.tsx           # landing: hero (Wordmark → Tagline → InstallCommand) then RosterDemo
    globals.css        # site styling (plain CSS) + a Tailwind v4 @import for brainless
    Navbar.tsx         # brand left; docs + GitHub links right
    HeroBackdrop.tsx   # the decorative hero photo, masked + scrimmed in globals.css
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

## The hero

The page is dark (`#0b1117`) and opens on a full-viewport hero: a decorative
photo behind the wordmark, tagline and install line. The photo is atmosphere,
not information — it says nothing about the product and is not required to.

Rules that keep it working, each learned the hard way:

- **Show the frame whole.** The photo's composition is the whole reason it's
  worth showing; centre-cropping into it destroys exactly that. This is why the
  hero is a full viewport tall — a 3:2 image cannot be shown whole in a short
  band, so a shorter hero necessarily crops. Don't add `scale`/`translate` to
  push the subject around: that is cropping by another name.
- **Type left, subject right.** The hero is the one asymmetric part of the
  page — centred text lands on the subject. A horizontal scrim beds the text
  column and clears before the subject, so type gets contrast without dimming
  the picture. Below 900px it recentres and the scrim goes heavy: at phone
  aspect ratios `cover` keeps roughly a third of the frame's width, the
  composition is gone regardless, and the type should simply win.
- **Text carries its own contrast.** The photo runs near-white in places, so
  the hero type uses an unoffset halo (`text-shadow` with no offset) rather
  than a fixed scrim behind the block — the bright patch moves with the crop
  and the viewport; the halo travels with the glyphs.
- **The nav is transparent and has no `backdrop-filter`.** A backdrop-filter
  blurs what's behind it even when the background is fully transparent, which
  paints a visible band across the top of the photo.

**Swapping the photo** is a one-file drop at `public/hero-cyclist.webp` — no
code change unless the aspect ratio moves, in which case update the `width`/
`height` on the `img` in `HeroBackdrop.tsx`. Size it for the zoom-free hero:
roughly 2× the widest viewport you care about, since it renders near 1:1 at
DPR 2. The current file is a Lanczos upscale of a 1536×1024 original and is
softer than it should be; a genuinely high-resolution source replaces it.

## One screen, one pitch

The pitch is still one screen's worth of ideas, but it is no longer one
viewport: the hero fills the fold and the demo sits below it, reached by a
scroll. That is the deliberate trade for showing the photo whole. Resist
letting anything else in between — the scroll must land on the demo.

What carries it:

1. **Wordmark + tagline** — who this is for, in two lines.
2. **Install** — `brew install zidankazi/roster/roster` with a copy button,
   the default of a two-segment toggle (Homebrew / Script). Homebrew leads
   because it's the clean, trusted line; the script (`curl … | sh`) is the
   fallback for machines without brew. Keep it to these two — the toggle
   swaps one line in place, it is not a doorway to a methods wall. If someone
   reads the tagline and copies the line, the page did its job.
3. **The demo — roster rebuilt as web chrome.** `RosterDemo.tsx` renders the
   whole window on the page below the hero: the title bar, the sidebar
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
