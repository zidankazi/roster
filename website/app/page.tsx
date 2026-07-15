import { Wordmark } from "./Wordmark";
import { Tagline } from "./Tagline";
import { InstallCommand } from "./InstallCommand";
import { RosterDemo } from "./demo/RosterDemo";
import { HeroBackdrop } from "./HeroBackdrop";

export default function Home() {
  return (
    <main>
      {/* The hero is its own viewport-height section so the backdrop photo has
          room to show its full 3:2 frame. Squeezed into a shorter band it can
          only be centre-cropped, which is what its composition lives in. */}
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
