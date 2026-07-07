import { Wordmark } from "./Wordmark";
import { Tagline } from "./Tagline";
import { InstallCommand } from "./InstallCommand";

export default function Home() {
  return (
    <main>
      <Wordmark />
      <Tagline />
      <InstallCommand />
    </main>
  );
}
