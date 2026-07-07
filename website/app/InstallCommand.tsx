"use client";

import { useState } from "react";

// roster's install line — the Homebrew tap formula (see docs/04-website.md).
const COMMAND = "brew install zidankazi/roster/roster";

export function InstallCommand() {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(COMMAND);
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
      <code className="install-cmd">{COMMAND}</code>
      <span className="install-icon">
        {copied ? <CheckIcon /> : <CopyIcon />}
      </span>
    </>
  );

  return (
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
