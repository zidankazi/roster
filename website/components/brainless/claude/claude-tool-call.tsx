import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * ClaudeToolCall — Claude Code's collapsed tool/result line.
 *
 * In the terminal this is faked with box-drawing glyphs and a "ctrl+o to
 * expand" hint. Here it's a real <details> disclosure: keyboard-operable,
 * announced to screen readers, and it keeps the exact ⏺ / ⎿ visual grammar.
 */
type Status = "success" | "error" | "pending";

const STATUS_COLOR: Record<Status, string> = {
  success: "#4ea96f",
  error: "#f7768e",
  pending: "#e0af68",
};

export function ClaudeToolCall({
  tool,
  arg,
  result,
  status = "success",
  defaultOpen = false,
  className,
  children,
}: {
  tool: string;
  arg?: string;
  result: string;
  status?: Status;
  defaultOpen?: boolean;
  className?: string;
  children?: React.ReactNode;
}) {
  const expandable = Boolean(children);

  return (
    <details
      open={defaultOpen}
      className={cn(
        "group font-mono text-[12px] leading-[1.55] [&_summary::-webkit-details-marker]:hidden",
        className,
      )}
    >
      <summary
        className={cn(
          "list-none",
          expandable ? "cursor-pointer" : "cursor-default",
          "rounded-none outline-none focus-visible:ring-1 focus-visible:ring-[#9a9a9a]/60",
        )}
      >
        <span className="flex min-w-0 items-baseline gap-2">
          <span aria-hidden className="shrink-0" style={{ color: STATUS_COLOR[status] }}>
            ⏺
          </span>
          <span className="min-w-0 break-words">
            <span className="text-[#e6e6e6]">{tool}</span>
            {arg !== undefined ? (
              <>
                <span className="text-[#808080]">(</span>
                <span className="text-[#9a9a9a]">{arg}</span>
                <span className="text-[#808080]">)</span>
              </>
            ) : null}
          </span>
        </span>
        <span className="flex min-w-0 items-baseline gap-2 text-[#9a9a9a]">
          {/* invisible status glyph spacer: aligns ⎿ under the tool name */}
          <span aria-hidden className="invisible shrink-0">
            ⏺
          </span>
          <span className="flex min-w-0 items-baseline gap-2">
            <span aria-hidden className="shrink-0 text-[#808080]">
              ⎿
            </span>
            <span className="min-w-0 break-words">
              {result}
              {expandable ? (
                <span className="ml-2 text-[#808080] group-open:hidden">
                  (ctrl+o to expand)
                </span>
              ) : null}
            </span>
          </span>
        </span>
      </summary>

      {expandable ? (
        <div className="mt-1 whitespace-pre-wrap pl-[32px] text-[#9a9a9a]">
          {children}
        </div>
      ) : null}
    </details>
  );
}
