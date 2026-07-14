import { Wordmark } from "./Wordmark";
import { Tagline } from "./Tagline";
import { InstallCommand } from "./InstallCommand";
import { RosterDemo } from "./demo/RosterDemo";

export default function Home() {
  return (
    <main>
      <Wordmark />
      <Tagline />
      <InstallCommand />
      {/* A living picture of what installs: roster watching three agents, its
          focused pane a real Claude Code session built from brainless. */}
      <p className="demo-caption">this is what you&apos;re installing</p>
      <RosterDemo />
    </main>
  );
}
