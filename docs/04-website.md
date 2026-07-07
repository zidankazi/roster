# 04 — website

*The `/website` landing page. Next.js, its own pnpm project, fully isolated from Cargo. For a tool like this the landing page is not a side quest — it's the pitch. "The demo is the product" applies here more than anywhere.*

## Isolation rules (do not violate)

`website/` is a self-contained pnpm project. It shares the git repo with the Rust crates and nothing else.

- Its own `package.json` and `pnpm-lock.yaml` live in `website/`, not at the repo root.
- The root `Cargo.toml` does **not** reference it. Cargo never sees `node_modules`.
- Root `.gitignore` must cover both worlds: `target/` (Rust) and `website/node_modules/`, `website/.next/` (JS).
- The only allowed bridge: if the site needs data from the Rust side (e.g. a generated feature table or version), the Rust build emits a JSON file the site imports at build time. Never a shared package manager, never a runtime call from site to binary.

## Structure

```
website/
  package.json
  pnpm-lock.yaml
  next.config.mjs        # MDX enabled here for later
  app/
    page.tsx             # the landing page
    layout.tsx
    docs/[[...slug]]/     # (later) MDX-rendered docs, when you want them
  components/
    Hero.tsx
    Demo.tsx             # the asciinema/gif embed — the centerpiece
    Install.tsx          # copy-paste install snippet
    Comparison.tsx       # roster vs tmux vs a status-only tool (the wedge, visualized)
  content/               # (later) .mdx docs pages
  public/
    demo.cast            # asciinema recording, or demo.gif
```

## Why Next.js + MDX (the call you made)

Plain HTML would ship the landing page faster, but you chose Next.js to leave room for **real MDX docs later** — Markdown pages with live React components (interactive install pickers, embedded demos). That's a legitimate reason and the one case where MDX earns its place: a *rendered docs site*, as opposed to the `/docs` folder, which stays plain `.md` because those are read by agents, not browsers. Enable MDX in `next.config.mjs` now so it's ready; don't build the docs section until v1 ships.

## Landing page content (v1 — one screen, one pitch)

Keep it to a single focused screen. Order:

1. **Hero:** the positioning line — *"Claude Code awareness for the terminal you already run."* One sentence under it: run your Claude Code agents in panes, see who's blocked / working / done at a glance, plus what each one needs.
2. **The demo:** an asciinema recording (preferred over gif — crisp, small, copy-able) of six Claude Code agents running, the sidebar lighting up, you jumping straight to the blocked one. This is 80% of the page's job. Autoplay, loop, no controls needed.
3. **Install:** the single line. `brew install zidankazi/roster/roster` (and a curl fallback). Copy button.
4. **The wedge, shown not told:** a tight comparison — bare tmux (no awareness), a status-only tool (a dot), roster (a dot + the reason). Three columns. This is where you make the differentiator legible to someone who's never seen the tool.
5. **Footer:** GitHub, license, a line of honesty ("v1, built in the open").

Resist a features wall. The demo and the one-line install carry it. If someone watches the cast and reads the install line, they're converted; everything else is noise.

## Build/deploy

- `pnpm dev` locally; static export or Vercel for deploy (Vercel is one click for Next.js and free for this).
- The domain is part of the pitch — grab `roster` across npm/crates/GitHub and the matching domain together.

## Sequence

The binary works; build the site when you're ready to launch — but record the demo cast the moment the sidebar looks good, because that recording is the single highest-leverage asset in the whole project. A great cast with a rough binary beats a polished binary nobody can see working.
