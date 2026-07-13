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
//! `PreToolUse` (fires when an approved tool starts) maps to
//! [`Frame::HookActivity`] with the same verbatim reason — it announces the
//! working card's activity *and* clears the ask for that tool only, since a
//! started tool answers its own request (a parallel auto-approved tool must
//! not clear someone else's pending ask) — and `Stop` (end of turn: denials,
//! interrupts that still stop cleanly) clears unconditionally. Subagent tool
//! events carry an `agent_id` and never clear: the parent may still be
//! waiting. Any
//! other event is a no-op. The command must never disturb the Claude
//! session it runs inside: it always exits 0, silently, whether or not
//! roster is listening — a claude launched outside roster simply has no
//! [`PANE_ENV`], and the hook does nothing.
//!
//! The same env pair carries the telemetry half of docs/05 Phase 2:
//! `roster _statusline`, registered as the `statusLine` command (see
//! [`statusline_snippet`]), forwards Claude Code's statusline session JSON
//! verbatim as a [`Frame::Statusline`] under the identical safety contract.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use roster_proto::{read_frame, write_frame, Frame};

/// The environment variable carrying the pane id into spawned processes.
pub const PANE_ENV: &str = "ROSTER_PANE";
/// The environment variable carrying the hook socket path.
pub const SOCK_ENV: &str = "ROSTER_HOOK_SOCK";

/// Payloads beyond this size are ignored rather than buffered — real hook
/// and statusline payloads are a few KB; anything larger is not for us.
/// Shared with the receiving side (`apply_statusline`) as its drop
/// threshold, so sender and receiver cannot drift apart.
pub(crate) const MAX_PAYLOAD: u64 = 1024 * 1024;

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

/// The shell command Claude Code should run for a bridge subcommand: this
/// binary's absolute path so it doesn't depend on (or trust) whatever
/// `roster` a future `PATH` resolves. The one home of that rule for both
/// printed snippets. Double-quoted because Claude Code hands the string to
/// `sh -c`: a binary path with spaces must stay one word, exactly as the
/// Claude Code docs quote paths in their own examples.
fn bridge_command(subcommand: &str) -> String {
    match std::env::current_exe() {
        Ok(path) => format!("\"{}\" {subcommand}", path.display()),
        Err(_) => format!("roster {subcommand}"),
    }
}

/// The hooks to merge into `~/.claude/settings.json`, printed by
/// `roster --print-hooks`.
pub fn settings_snippet() -> String {
    let command = bridge_command("_hook");
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

/// The pane id and hook socket path from the environment, or `None` when
/// this process is not running inside a roster pane.
fn bridge_env() -> Option<(u64, String)> {
    let (Ok(pane_var), Ok(sock)) = (std::env::var(PANE_ENV), std::env::var(SOCK_ENV)) else {
        return None;
    };
    Some((pane_var.parse().ok()?, sock))
}

/// The stdin payload, read on a helper thread so an unclosed stdin (or a
/// human at a tty) can't hang the Claude session behind it; the thread dies
/// with the process. `None` when nothing arrives within [`STDIN_DEADLINE`].
fn read_stdin_deadlined() -> Option<String> {
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
    rx.recv_timeout(STDIN_DEADLINE).ok()
}

/// Connect to the hook socket and send one frame, or `None` when either
/// step fails — roster being gone must never disturb the agent. The single
/// home of the wedged-listener protection: a 1-second write timeout so a
/// stuck owner can't stall the Claude session behind it.
fn send_frame(sock: &str, frame: &Frame) -> Option<UnixStream> {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return None; // roster is gone; the agent lives on.
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    write_frame(&mut stream, frame).ok()?;
    Some(stream)
}

/// The `roster _hook` entrypoint: read the hook payload from stdin, and
/// when this claude runs inside a roster pane, report the event over the
/// hook socket. Silent and infallible by design — a hook that could fail
/// would break the user's Claude session, which is worse than a missed
/// state update (screen-scraping still runs underneath).
pub fn run() -> Result<(), String> {
    let Some((pane, sock)) = bridge_env() else {
        return Ok(()); // Not a roster pane: no-op.
    };
    let Some(payload) = read_stdin_deadlined() else {
        return Ok(());
    };
    let Some(frame) = frame_from_payload(pane, &payload) else {
        return Ok(());
    };
    // Only a permission ask (`HookBlocked`) can be auto-approved; clears are
    // fire-and-forget and get no reply.
    let is_ask = matches!(frame, Frame::HookBlocked { .. });
    let Some(mut stream) = send_frame(&sock, &frame) else {
        return Ok(());
    };
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

/// The `roster _statusline` entrypoint: read Claude Code's statusline
/// session JSON from stdin and, inside a roster pane, forward it verbatim
/// as a [`Frame::Statusline`] — no parsing here; the pinned contract point
/// stays `roster-detect`'s statusline parser, applied by the receiving
/// client. Prints nothing, deliberately: stdout would become the pane's
/// visible statusline, and roster's sidebar is the display surface. Same
/// safety contract as [`run`]: silent no-op outside roster, always exit 0,
/// deadlined stdin read — the feed must never disturb the Claude session
/// it reports on.
pub fn run_statusline() -> Result<(), String> {
    let Some((pane, sock)) = bridge_env() else {
        return Ok(()); // Not a roster pane: no-op.
    };
    let Some(json) = read_stdin_deadlined() else {
        return Ok(());
    };
    if json.trim().is_empty() {
        return Ok(());
    }
    // Fire-and-forget: telemetry never gets a reply.
    let _ = send_frame(&sock, &Frame::Statusline { pane, json });
    Ok(())
}

/// The statusLine entry to merge into `~/.claude/settings.json`, printed by
/// `roster --print-statusline`. Same absolute-path rule as
/// [`settings_snippet`]. Unlike hooks, `statusLine` is a single slot, not a
/// list: merging this replaces any existing statusline command, so main
/// warns when [`statusline_already_configured`] says one is set.
pub fn statusline_snippet() -> String {
    let command = bridge_command("_statusline");
    let snippet = serde_json::json!({
        "statusLine": { "type": "command", "command": command }
    });
    let mut text = serde_json::to_string_pretty(&snippet).expect("static json serializes");
    text.push('\n');
    text
}

/// Whether `~/.claude/settings.json` already configures a `statusLine`.
/// Gates an advisory clobber warning only, so best-effort by design: an
/// unreadable or unparsable settings file reads as "none configured".
/// Deliberately user-settings-scope: a project's `.claude/settings.json` /
/// `settings.local.json` can also set (and outrank) the slot, but those are
/// per-cwd and this command can run from anywhere — checking the current
/// directory would be as often misleading as helpful.
pub fn statusline_already_configured() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let path = std::path::PathBuf::from(home)
        .join(".claude")
        .join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    json.get("statusLine").is_some_and(|v| !v.is_null())
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
        // A tool starting both answers a matching ask and announces the
        // activity: one frame does both. The reason is built exactly like a
        // blocked ask, so a working card reads `Bash: cargo test` in place
        // of the scraped spinner.
        "PreToolUse" if !subagent => {
            let reason = render_reason(
                json.get("tool_name").and_then(|t| t.as_str()),
                json.get("tool_input"),
            );
            Some(Frame::HookActivity { pane, tool, reason })
        }
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
    fn pre_tool_use_reports_the_activity_and_names_its_tool() {
        // A tool starting yields the rich activity (like a blocked ask) plus
        // the tool name, so applying it can also clear a matching pin.
        let frame = frame_from_payload(7, &payload("PreToolUse", "Read", r#"{"file_path":"x"}"#));
        assert_eq!(
            frame,
            Some(Frame::HookActivity {
                pane: 7,
                tool: "Read".into(),
                reason: "Read x".into(),
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
    fn statusline_snippet_registers_the_forwarder_in_the_single_slot() {
        let text = statusline_snippet();
        let json: serde_json::Value = serde_json::from_str(&text).expect("snippet parses");
        assert_eq!(json["statusLine"]["type"], "command");
        let command = json["statusLine"]["command"].as_str().unwrap_or_default();
        assert!(command.ends_with(" _statusline"), "{command}");
        // Absolute path to this binary — quoted for the shell, not a PATH
        // lookup.
        assert!(command.starts_with("\"/"), "{command}");
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
            // Absolute path to this binary — quoted for the shell, not a
            // PATH lookup.
            assert!(command.starts_with("\"/"), "event {event}: {command}");
        }
    }
}
