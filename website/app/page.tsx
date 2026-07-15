import { Wordmark } from "./Wordmark";
import { Tagline } from "./Tagline";
import { InstallCommand } from "./InstallCommand";
import { RosterDemo } from "./demo/RosterDemo";
import { HeroBackdrop } from "./HeroBackdrop";

export default function Home() {
  return (
    <main>
      {/* The hero is its own viewport-height section. That height bought the
          old photo room to show its 3:2 frame whole; this backdrop doesn't
          need it — a shorter band centre-crops the bright top margin away
          first and leaves the black hollow the type reads against intact. The
          height is kept, but nothing here depends on it any more; see
          docs/04-website.md ("The hero"). */}
      <section className="hero">
        <HeroBackdrop />
        <Wordmark />
        <Tagline />
        <InstallCommand />
      </section>
      {/* A living picture of what installs: roster watching a fleet of agents,
          its focused pane a real Claude Code session built from brainless. */}
      <RosterDemo />
    </main>
  );
}
