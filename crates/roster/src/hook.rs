//! The Claude Code hook bridge: the receiving half of docs/05 Phase 1.
//!
//! roster spawns each pane's process with [`PANE_ENV`] (its pane id) and
//! [`SOCK_ENV`] (a unix socket) in the environment. Claude Code hooks
//! inherit that environment, so a `roster _hook` command registered for the
//! `PermissionRequest`, `PreToolUse`, and `Stop` events (see
//! [`settings_snippet`]) can report, the instant they fire, exactly which
//! pane is blocked and on what — no screen-scraping, no debounce.
//!
//! `PermissionRequest` maps to [`Frame::HookBlocked`] with the verbatim ask;
//! `PreToolUse` (fires when an approved tool starts) clears the ask for
//! that tool only — a parallel auto-approved tool must not clear someone
//! else's pending ask — and `Stop` (end of turn: denials, interrupts that
//! still stop cleanly) clears unconditionally. Subagent tool events carry
//! an `agent_id` and never clear: the parent may still be waiting. Any
//! other event is a no-op. The command must never disturb the Claude
//! session it runs inside: it always exits 0, silently, whether or not
//! roster is listening — a claude launched outside roster simply has no
//! [`PANE_ENV`], and the hook does nothing.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use roster_proto::{read_frame, write_frame, Frame};

/// The environment variable carrying the pane id into spawned processes.
pub const PANE_ENV: &str = "ROSTER_PANE";
/// The environment variable carrying the hook socket path.
pub const SOCK_ENV: &str = "ROSTER_HOOK_SOCK";

/// Hook payloads beyond this size are ignored rather than buffered — real
/// payloads are a few KB; anything larger is not for us.
const MAX_PAYLOAD: u64 = 1024 * 1024;

/// Reasons are sidebar copy: one line, not a document.
const MAX_REASON: usize = 120;

/// How long `_hook` waits for its stdin payload. Claude Code pipes the
/// JSON and closes; a runner that never closes stdin (or a human trying
/// `roster _hook` at a tty) must not hang the session behind it.
const STDIN_DEADLINE: Duration = Duration::from_secs(2);

/// How long `_hook` waits for the socket owner's auto-approve decision after
/// reporting a permission ask. The owner replies the instant it reads the
/// ask (a local socket round-trip, sub-millisecond in practice), so this is
/// a safety cap, not latency the user feels. Kept short so the worst case
/// (`STDIN_DEADLINE` + connect/write + this) stays well under the 5s hook
/// timeout in [`settings_snippet`], after which Claude Code kills the hook
/// and shows its own prompt regardless. A missing reply — owner gone, an
/// older roster that predates the reply channel, a wedged loop — simply
/// means "no auto-approve", identical to the behavior before this channel.
const REPLY_DEADLINE: Duration = Duration::from_secs(1);

/// The hooks to merge into `~/.claude/settings.json`, printed by
/// `roster --print-hooks`. Uses this binary's absolute path so the hook
/// doesn't depend on (or trust) whatever `roster` a future `PATH` resolves.
pub fn settings_snippet() -> String {
    let command = match std::env::current_exe() {
        Ok(path) => format!("{} _hook", path.display()),
        Err(_) => "roster _hook".to_string(),
    };
    let entry = serde_json::json!([
        { "matcher": "", "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }
    ]);
    let snippet = serde_json::json!({
        "hooks": {
            "PermissionRequest": entry,
            "PreToolUse": entry,
            "Stop": entry,
        }
    });
    let mut text = serde_json::to_string_pretty(&snippet).expect("static json serializes");
    text.push('\n');
    text
}

/// The `roster _hook` entrypoint: read the hook payload from stdin, and
/// when this claude runs inside a roster pane, report the event over the
/// hook socket. Silent and infallible by design — a hook that could fail
/// would break the user's Claude session, which is worse than a missed
/// state update (screen-scraping still runs underneath).
pub fn run() -> Result<(), String> {
    let (Ok(pane_var), Ok(sock)) = (std::env::var(PANE_ENV), std::env::var(SOCK_ENV)) else {
        return Ok(()); // Not a roster pane: no-op.
    };
    let Ok(pane) = pane_var.parse::<u64>() else {
        return Ok(());
    };
    // Read stdin on a helper thread so an unclosed stdin can't hang the
    // Claude session; the thread dies with the process.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut payload = String::new();
        if std::io::stdin()
            .take(MAX_PAYLOAD)
            .read_to_string(&mut payload)
            .is_ok()
        {
            let _ = tx.send(payload);
        }
    });
    let Ok(payload) = rx.recv_timeout(STDIN_DEADLINE) else {
        return Ok(());
    };
    let Some(frame) = frame_from_payload(pane, &payload) else {
        return Ok(());
    };
    let Ok(mut stream) = UnixStream::connect(&sock) else {
        return Ok(()); // roster is gone; the agent lives on.
    };
    // A wedged listener must not stall the Claude session behind it.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    // Only a permission ask (`HookBlocked`) can be auto-approved; clears are
    // fire-and-forget and get no reply.
    let is_ask = matches!(frame, Frame::HookBlocked { .. });
    if write_frame(&mut stream, &frame).is_err() {
        return Ok(());
    }
    if is_ask {
        // Wait, deadlined, for the owner's decision. Approve only on an
        // explicit allow; a timeout, EOF, or any other reply leaves stdout
        // empty so Claude shows its own prompt — exactly as before.
        let _ = stream.set_read_timeout(Some(REPLY_DEADLINE));
        if let Ok(Some(Frame::HookDecision { allow: true })) = read_frame(&mut stream) {
            let mut out = std::io::stdout();
            let _ = out.write_all(approve_json().as_bytes());
            let _ = out.flush();
        }
    }
    Ok(())
}

/// The stdout a `PermissionRequest` hook prints to auto-approve its ask.
/// Claude Code honors `hookSpecificOutput.decision.behavior = "allow"` (with
/// exit 0) by running the tool without painting its prompt. Pinned to the
/// Claude Code 2.x contract (see [docs/05](../../../docs/05-claude-native-attention.md));
/// the envelope moves across major versions, so this one function is where
/// to re-pin it. `decision.behavior` is `PermissionRequest`-specific — the
/// `permissionDecision` field belongs to `PreToolUse`, which cannot suppress
/// the prompt.
fn approve_json() -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": { "behavior": "allow" },
        }
    })
    .to_string()
}

/// Map one hook payload to the frame it should send, or `None` for events
/// that say nothing about the pane's blocked state.
fn frame_from_payload(pane: u64, payload: &str) -> Option<Frame> {
    let json: serde_json::Value = serde_json::from_str(payload).ok()?;
    let event = json.get("hook_event_name")?.as_str()?;
    let tool = json
        .get("tool_name")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    // Subagent events (agent_id present) must not clear: the parent agent
    // may still be waiting on its own ask. Their asks still block, though —
    // a subagent's permission prompt needs the human all the same.
    let subagent = json.get("agent_id").and_then(|v| v.as_str()).is_some();
    match event {
        "PermissionRequest" => {
            let reason = render_reason(
                json.get("tool_name").and_then(|t| t.as_str()),
                json.get("tool_input"),
            );
            Some(Frame::HookBlocked { pane, tool, reason })
        }
        "PreToolUse" if !subagent => Some(Frame::HookClear { pane, tool }),
        "Stop" if !subagent => Some(Frame::HookClear {
            pane,
            tool: String::new(),
        }),
        _ => None,
    }
}

/// One sidebar line for a permission ask: the tool plus the part of its
/// input a human decides on — `Bash: cargo test`, `Edit src/main.rs`.
fn render_reason(tool: Option<&str>, input: Option<&serde_json::Value>) -> String {
    let Some(tool) = tool else {
        return "permission requested".to_string();
    };
    let field = |key: &str| {
        input
            .and_then(|i| i.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let text = match tool {
        "Bash" => field("command").map(|c| format!("Bash: {c}")),
        "Edit" | "Write" | "Read" | "NotebookEdit" => {
            field("file_path").map(|p| format!("{tool} {p}"))
        }
        "WebFetch" | "WebSearch" => field("url")
            .or_else(|| field("query"))
            .map(|u| format!("{tool} {u}")),
        _ => None,
    };
    truncate(&text.unwrap_or_else(|| tool.to_string()))
}

/// Cap a reason at [`MAX_REASON`] characters on a char boundary.
fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_REASON {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_REASON - 1).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(event: &str, tool: &str, input: &str) -> String {
        format!(
            r#"{{"session_id":"s","cwd":"/w","hook_event_name":"{event}","tool_name":"{tool}","tool_input":{input}}}"#
        )
    }

    #[test]
    fn permission_request_blocks_with_the_command() {
        let frame = frame_from_payload(
            3,
            &payload(
                "PermissionRequest",
                "Bash",
                r#"{"command":"rm -rf target/"}"#,
            ),
        );
        assert_eq!(
            frame,
            Some(Frame::HookBlocked {
                pane: 3,
                tool: "Bash".into(),
                reason: "Bash: rm -rf target/".into()
            })
        );
    }

    #[test]
    fn file_tools_block_with_the_path() {
        let frame = frame_from_payload(
            1,
            &payload(
                "PermissionRequest",
                "Edit",
                r#"{"file_path":"src/main.rs","old_string":"a","new_string":"b"}"#,
            ),
        );
        assert_eq!(
            frame,
            Some(Frame::HookBlocked {
                pane: 1,
                tool: "Edit".into(),
                reason: "Edit src/main.rs".into()
            })
        );
    }

    #[test]
    fn unknown_tools_block_with_the_tool_name() {
        let frame = frame_from_payload(
            1,
            &payload("PermissionRequest", "ExitPlanMode", r#"{"plan":"do it"}"#),
        );
        assert_eq!(
            frame,
            Some(Frame::HookBlocked {
                pane: 1,
                tool: "ExitPlanMode".into(),
                reason: "ExitPlanMode".into()
            })
        );
    }

    #[test]
    fn missing_tool_fields_still_block() {
        let frame = frame_from_payload(2, r#"{"hook_event_name":"PermissionRequest"}"#);
        assert_eq!(
            frame,
            Some(Frame::HookBlocked {
                pane: 2,
                tool: String::new(),
                reason: "permission requested".into()
            })
        );
        // A Bash ask with no command falls back to the tool name.
        let frame = frame_from_payload(
            2,
            &payload("PermissionRequest", "Bash", r#"{"description":"x"}"#),
        );
        assert_eq!(
            frame,
            Some(Frame::HookBlocked {
                pane: 2,
                tool: "Bash".into(),
                reason: "Bash".into()
            })
        );
    }

    #[test]
    fn pre_tool_use_clears_only_its_own_tool() {
        let frame = frame_from_payload(7, &payload("PreToolUse", "Read", r#"{"file_path":"x"}"#));
        assert_eq!(
            frame,
            Some(Frame::HookClear {
                pane: 7,
                tool: "Read".into()
            })
        );
    }

    #[test]
    fn stop_clears_unconditionally() {
        let frame = frame_from_payload(7, r#"{"hook_event_name":"Stop","stop_hook_active":false}"#);
        assert_eq!(
            frame,
            Some(Frame::HookClear {
                pane: 7,
                tool: String::new()
            })
        );
    }

    #[test]
    fn subagent_events_never_clear_but_their_asks_block() {
        let sub = |event: &str| {
            format!(
                r#"{{"hook_event_name":"{event}","tool_name":"Bash","tool_input":{{"command":"ls"}},"agent_id":"a1","agent_type":"Explore"}}"#
            )
        };
        assert_eq!(frame_from_payload(7, &sub("PreToolUse")), None);
        assert_eq!(frame_from_payload(7, &sub("Stop")), None);
        assert!(matches!(
            frame_from_payload(7, &sub("PermissionRequest")),
            Some(Frame::HookBlocked { .. })
        ));
    }

    #[test]
    fn unrelated_events_are_no_ops() {
        for event in [
            "PostToolUse",
            "SessionEnd",
            "UserPromptSubmit",
            "PreCompact",
        ] {
            let frame = frame_from_payload(7, &payload(event, "Bash", r#"{"command":"ls"}"#));
            assert_eq!(frame, None, "event {event}");
        }
    }

    #[test]
    fn junk_payloads_map_to_nothing() {
        assert_eq!(frame_from_payload(1, ""), None);
        assert_eq!(frame_from_payload(1, "not json"), None);
        assert_eq!(frame_from_payload(1, "{}"), None);
        assert_eq!(frame_from_payload(1, r#"{"hook_event_name":42}"#), None);
    }

    #[test]
    fn long_reasons_truncate_on_char_boundaries() {
        let long = "x".repeat(500);
        let frame = frame_from_payload(
            1,
            &payload(
                "PermissionRequest",
                "Bash",
                &format!(r#"{{"command":"{long}"}}"#),
            ),
        );
        let Some(Frame::HookBlocked { reason, .. }) = frame else {
            panic!("expected blocked");
        };
        assert_eq!(reason.chars().count(), MAX_REASON);
        assert!(reason.ends_with('…'));

        // Multibyte input must not split a char.
        let emoji = "🦀".repeat(200);
        let reason = truncate(&format!("Bash: {emoji}"));
        assert_eq!(reason.chars().count(), MAX_REASON);
    }

    #[test]
    fn approve_json_is_the_permission_request_allow_contract() {
        let json: serde_json::Value = serde_json::from_str(&approve_json()).expect("valid json");
        assert_eq!(
            json["hookSpecificOutput"]["hookEventName"], "PermissionRequest",
            "the decision must name the event it answers"
        );
        assert_eq!(
            json["hookSpecificOutput"]["decision"]["behavior"], "allow",
            "PermissionRequest allows via decision.behavior"
        );
        // `permissionDecision` is the PreToolUse field and cannot suppress a
        // prompt — using it here would silently fail to auto-approve.
        assert!(
            json["hookSpecificOutput"]["permissionDecision"].is_null(),
            "must not use the PreToolUse-only permissionDecision field"
        );
    }

    #[test]
    fn settings_snippet_is_valid_json_registering_all_three_events() {
        let text = settings_snippet();
        let json: serde_json::Value = serde_json::from_str(&text).expect("snippet parses");
        for event in ["PermissionRequest", "PreToolUse", "Stop"] {
            let command = json["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_default();
            assert!(command.ends_with(" _hook"), "event {event}: {command}");
            // Absolute path to this binary, not a PATH lookup.
            assert!(command.starts_with('/'), "event {event}: {command}");
        }
    }
}
