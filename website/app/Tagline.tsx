import { ClaudeMark } from "./ClaudeMark";

// Sits between the wordmark and the install pill. The second line places the
// animated Claude mark inline, right before the word "Claude".
export function Tagline() {
  return (
    <div className="tagline">
      <p className="tagline-lead">Terminal multiplexer for Claude Code agents.</p>
      <p className="tagline-sub">
        The best way to build with{" "}
        <span className="nowrap">
          <ClaudeMark />
          Claude.
        </span>
      </p>
    </div>
  );
}
