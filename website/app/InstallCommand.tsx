"use client";

import { useState } from "react";

// roster's two install paths (see docs/04-website.md). Homebrew is the default
// — the clean, trusted line; the script is the fallback for machines without
// brew. The raw-GitHub URL is a placeholder until the domain lands, at which
// point only this constant and install.sh's header change (see the
// domain-install-endpoint plan).
const METHODS = [
  {
    id: "brew",
    label: "Homebrew",
    command: "brew install zidankazi/roster/roster",
  },
  {
    id: "script",
    label: "Script",
    command:
      "curl -fsSL https://raw.githubusercontent.com/zidankazi/roster/main/install.sh | sh",
  },
] as const;

type MethodId = (typeof METHODS)[number]["id"];

export function InstallCommand() {
  const [methodId, setMethodId] = useState<MethodId>("brew");
  const [copied, setCopied] = useState(false);

  const command =
    METHODS.find((m) => m.id === methodId)?.command ?? METHODS[0].command;

  // Switching method invalidates the previous copy — reset the icon so the
  // check never lingers over a line the user hasn't actually copied.
  const select = (id: MethodId) => {
    setMethodId(id);
    setCopied(false);
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard unavailable (e.g. insecure context) — leave the icon as-is.
    }
  };

  // The same content is rendered twice, stacked: a charcoal base for the
  // resting off-white, and a warm-white copy in the red fill layer that a
  // clip-path reveals in lockstep with the sweep — so each glyph flips to
  // white exactly as the red passes under it, legible at every frame.
  const inner = (
    <>
      <span className="install-prompt">$</span>
      <code className="install-cmd">{command}</code>
      <span className="install-icon">
        {copied ? <CheckIcon /> : <CopyIcon />}
      </span>
    </>
  );

  return (
    <div className="install-block">
      <div
        className="install-methods"
        role="group"
        aria-label="Install method"
        data-active={methodId}
      >
        {/* The moving indicator — a neutral pad under Homebrew that springs
            across and turns red under Script. Position is pure CSS (equal-width
            segments), so it needs no measurement and renders right on the
            server. */}
        <span className="install-thumb" aria-hidden="true" />
        {METHODS.map((m) => (
          <button
            key={m.id}
            type="button"
            className={`install-method${m.id === methodId ? " is-active" : ""}`}
            aria-pressed={m.id === methodId}
            onClick={() => select(m.id)}
          >
            {m.label}
          </button>
        ))}
      </div>
      <button
        type="button"
        className="install"
        onClick={copy}
        aria-label={copied ? "Copied install command" : "Copy install command"}
      >
        <span className="install-layer install-base">{inner}</span>
        <span className="install-layer install-fill" aria-hidden="true">
          {inner}
        </span>
      </button>
    </div>
  );
}

function CopyIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}
