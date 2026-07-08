//! The wire protocol between a roster client and a session server.
//!
//! Frames are length-prefixed binary: a little-endian `u32` payload length,
//! a `u8` tag, then the payload. Encoding is hand-rolled — the message set
//! is small and stable, and the crate stays dependency-free. The transport
//! is any `Read`/`Write` pair: a unix socket locally, an ssh subprocess's
//! stdio remotely.
//!
//! Corrupt or oversized input is always an `io::Error`, never a panic — see
//! `MAX_FRAME` and the length checks in `read_frame`. Tags are an
//! append-only compatibility surface: never reuse or renumber one (see
//! `Frame::tag`).

use std::io::{self, Read, Write};

/// The largest frame either side will read or write. Enforced before the
/// length-prefixed allocation happens — a corrupt or hostile length prefix
/// must never drive an unbounded `Vec` allocation.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// One pane as described in a [`Frame::Hello`]: the server's id, the
/// command it runs, and its exit code if it has ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloPane {
    /// The server's pane id.
    pub pane: u64,
    /// The command the pane runs.
    pub command: String,
    /// The exit code, when the process has ended (the pane lingers).
    pub exited: Option<u32>,
    /// Whether the server auto-approves this pane's permission asks, so a
    /// reattaching client seeds its local mirror (the lit `auto` chip and the
    /// blocked-pin suppression) instead of diverging from the server's set.
    pub auto_approve: bool,
}

/// Every message either side can send. Tags 1–63 flow client → server,
/// 64+ server → client — except the hook frames (tags 10/11) and the
/// statusline frame (tag 14), which enter from `roster _hook` /
/// `roster _statusline` and are also relayed server → client verbatim, and
/// the hook reply (tag 12, [`Frame::HookDecision`]), written back to
/// `roster _hook` on its own connection — never relayed to a client, never
/// valid client → server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    // Client → server.
    /// Take over the session: the server replies with `Hello` and replays,
    /// disconnecting any previously attached client.
    Attach,
    /// Keystrokes for a pane.
    Input {
        /// Target pane.
        pane: u64,
        /// Raw bytes to write to the pane's pty.
        bytes: Vec<u8>,
    },
    /// A pane's new size.
    Resize {
        /// Target pane.
        pane: u64,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Start a command in a fresh pane; the server replies `PaneOpened`.
    Spawn {
        /// The shell command to run.
        command: String,
    },
    /// Kill a pane's process and forget it.
    Close {
        /// Target pane.
        pane: u64,
    },
    /// Store the client's layout blob for the next attach. Opaque to the
    /// server.
    SetLayout {
        /// The serialized layout.
        blob: Vec<u8>,
    },
    /// The client is leaving; the session keeps running.
    Detach,
    /// Kill every pane and end the session.
    Kill,
    /// Liveness probe; the server replies `Pong` without disturbing an
    /// attached client.
    Ping,
    /// Turn auto-approval on or off for a pane. The session server owns the
    /// auto-approve set for its panes and answers hook asks from it; the
    /// client sends this when the user toggles auto-approve. `pane` is the
    /// server-side pane id (the `ROSTER_PANE` value a hook reports), which
    /// the client mirrors 1:1.
    SetAutoApprove {
        /// Target pane (server-side id).
        pane: u64,
        /// Auto-approve the pane's future asks when true.
        on: bool,
    },

    // Hook → server (or the in-process app). Sent by `roster _hook` when a
    // Claude Code hook fires; a session server relays these two frames
    // verbatim to the attached client, where detection applies them — the
    // one exception to the tag-range convention below.
    /// A pane's agent is blocked on a permission request; `reason` is the
    /// verbatim ask (tool + input), extracted from the hook payload.
    HookBlocked {
        /// The pane whose agent is blocked (`ROSTER_PANE` in its env).
        pane: u64,
        /// The tool being asked about (e.g. `Bash`), so a later clear can
        /// be matched to this ask. Empty when the payload named none.
        tool: String,
        /// What the agent is asking to do, e.g. `Bash: cargo test`.
        reason: String,
    },
    /// A hook event that answers a permission ask: the approved tool
    /// started (`PreToolUse`), or the turn ended (`Stop`).
    HookClear {
        /// The pane to release back to screen-based detection.
        pane: u64,
        /// The tool whose ask this clears; an empty string clears any ask
        /// (end of turn — nothing can still be pending).
        tool: String,
    },

    /// A pane's statusline telemetry payload, verbatim. Sent by
    /// `roster _statusline` when Claude Code feeds its statusline command;
    /// like the hook frames it enters from outside the client/server pair
    /// and a session server relays it verbatim to the attached client,
    /// where the pinned parser (`roster-detect`) turns it into telemetry.
    /// Fire-and-forget: unlike `HookBlocked`, it never gets a reply.
    Statusline {
        /// The pane whose agent reported telemetry (`ROSTER_PANE` in its
        /// env).
        pane: u64,
        /// The statusline session JSON exactly as Claude Code piped it.
        json: String,
    },

    // Hook reply (owner → `roster _hook`). Written back on the hook
    // connection after a `HookBlocked`, never relayed to a client.
    /// The socket owner's answer to a pane's permission ask: auto-approve it
    /// (`allow: true`) or not. On `true` the hook prints an allow decision
    /// and Claude proceeds without its prompt; otherwise the hook stays
    /// silent and Claude asks the human as before.
    HookDecision {
        /// Approve the ask when true.
        allow: bool,
    },

    // Server → client.
    /// The session state at attach: every pane plus the stored layout blob.
    Hello {
        /// All live and lingering panes.
        panes: Vec<HelloPane>,
        /// The layout blob from the last `SetLayout`, empty if none.
        layout: Vec<u8>,
    },
    /// A pane's buffered output history, sent once after `Hello`.
    Replay {
        /// Source pane.
        pane: u64,
        /// The retained tail of the pane's output.
        bytes: Vec<u8>,
    },
    /// Live output from a pane.
    Output {
        /// Source pane.
        pane: u64,
        /// Raw pty output.
        bytes: Vec<u8>,
    },
    /// A pane's process ended; the pane lingers until closed.
    Exited {
        /// The pane whose process ended.
        pane: u64,
        /// Its exit code.
        code: u32,
    },
    /// A `Spawn` succeeded.
    PaneOpened {
        /// The new pane's server id.
        pane: u64,
        /// The command it runs.
        command: String,
    },
    /// The server is dropping this client: another client attached, the
    /// session was killed, or the last pane closed.
    Shutdown {
        /// Human-readable reason.
        reason: String,
    },
    /// Reply to `Ping`.
    Pong,
    /// A `Spawn` failed; the session carries on.
    SpawnFailed {
        /// What went wrong.
        error: String,
    },
}

impl Frame {
    // Tags are an append-only compatibility surface: never reuse or
    // renumber one, even for a removed variant. A new tag needs an arm
    // here, in write_frame, in read_frame, and a round-trip case in
    // every_frame_round_trips.
    fn tag(&self) -> u8 {
        match self {
            Frame::Attach => 1,
            Frame::Input { .. } => 2,
            Frame::Resize { .. } => 3,
            Frame::Spawn { .. } => 4,
            Frame::Close { .. } => 5,
            Frame::SetLayout { .. } => 6,
            Frame::Detach => 7,
            Frame::Kill => 8,
            Frame::Ping => 9,
            Frame::HookBlocked { .. } => 10,
            Frame::HookClear { .. } => 11,
            Frame::HookDecision { .. } => 12,
            Frame::SetAutoApprove { .. } => 13,
            Frame::Statusline { .. } => 14,
            Frame::Hello { .. } => 64,
            Frame::Replay { .. } => 65,
            Frame::Output { .. } => 66,
            Frame::Exited { .. } => 67,
            Frame::PaneOpened { .. } => 68,
            Frame::Shutdown { .. } => 69,
            Frame::Pong => 70,
            Frame::SpawnFailed { .. } => 71,
        }
    }
}

/// Write one frame.
pub fn write_frame(w: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let mut payload = Vec::new();
    match frame {
        Frame::Attach | Frame::Detach | Frame::Kill | Frame::Ping | Frame::Pong => {}
        Frame::Input { pane, bytes }
        | Frame::Replay { pane, bytes }
        | Frame::Output { pane, bytes } => {
            put_u64(&mut payload, *pane);
            put_bytes(&mut payload, bytes);
        }
        Frame::Resize { pane, cols, rows } => {
            put_u64(&mut payload, *pane);
            payload.extend_from_slice(&cols.to_le_bytes());
            payload.extend_from_slice(&rows.to_le_bytes());
        }
        Frame::Spawn { command } => put_bytes(&mut payload, command.as_bytes()),
        Frame::Close { pane } => put_u64(&mut payload, *pane),
        Frame::HookBlocked { pane, tool, reason } => {
            put_u64(&mut payload, *pane);
            put_bytes(&mut payload, tool.as_bytes());
            put_bytes(&mut payload, reason.as_bytes());
        }
        Frame::HookClear { pane, tool } => {
            put_u64(&mut payload, *pane);
            put_bytes(&mut payload, tool.as_bytes());
        }
        Frame::Statusline { pane, json } => {
            put_u64(&mut payload, *pane);
            put_bytes(&mut payload, json.as_bytes());
        }
        Frame::HookDecision { allow } => payload.push(*allow as u8),
        Frame::SetAutoApprove { pane, on } => {
            put_u64(&mut payload, *pane);
            payload.push(*on as u8);
        }
        Frame::SetLayout { blob } => put_bytes(&mut payload, blob),
        Frame::Hello { panes, layout } => {
            put_u64(&mut payload, panes.len() as u64);
            for p in panes {
                put_u64(&mut payload, p.pane);
                put_bytes(&mut payload, p.command.as_bytes());
                match p.exited {
                    Some(code) => {
                        payload.push(1);
                        payload.extend_from_slice(&code.to_le_bytes());
                    }
                    None => payload.push(0),
                }
                payload.push(p.auto_approve as u8);
            }
            put_bytes(&mut payload, layout);
        }
        Frame::Exited { pane, code } => {
            put_u64(&mut payload, *pane);
            payload.extend_from_slice(&code.to_le_bytes());
        }
        Frame::PaneOpened { pane, command } => {
            put_u64(&mut payload, *pane);
            put_bytes(&mut payload, command.as_bytes());
        }
        Frame::Shutdown { reason } => put_bytes(&mut payload, reason.as_bytes()),
        Frame::SpawnFailed { error } => put_bytes(&mut payload, error.as_bytes()),
    }
    let len = payload.len() as u32 + 1;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&[frame.tag()])?;
    w.write_all(&payload)?;
    w.flush()
}

/// Read one frame. `Ok(None)` means the peer closed cleanly between frames.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    // A frame is always tag + payload, so a zero length is corruption,
    // not an empty frame; the upper bound guards the allocation below.
    if len == 0 || len > MAX_FRAME {
        return Err(corrupt("frame length out of range"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    let tag = buf[0];
    let mut p = Cursor {
        buf: &buf[1..],
        at: 0,
    };
    let frame = match tag {
        1 => Frame::Attach,
        2 => Frame::Input {
            pane: p.u64()?,
            bytes: p.bytes()?,
        },
        3 => Frame::Resize {
            pane: p.u64()?,
            cols: p.u16()?,
            rows: p.u16()?,
        },
        4 => Frame::Spawn {
            command: p.string()?,
        },
        5 => Frame::Close { pane: p.u64()? },
        6 => Frame::SetLayout { blob: p.bytes()? },
        7 => Frame::Detach,
        8 => Frame::Kill,
        9 => Frame::Ping,
        10 => Frame::HookBlocked {
            pane: p.u64()?,
            tool: p.string()?,
            reason: p.string()?,
        },
        11 => Frame::HookClear {
            pane: p.u64()?,
            tool: p.string()?,
        },
        12 => Frame::HookDecision {
            allow: p.u8()? != 0,
        },
        13 => Frame::SetAutoApprove {
            pane: p.u64()?,
            on: p.u8()? != 0,
        },
        14 => Frame::Statusline {
            pane: p.u64()?,
            json: p.string()?,
        },
        64 => {
            let count = p.u64()?;
            // A pane count small in bytes can still claim far more
            // slots than the payload could possibly hold; cap it before
            // with_capacity over-allocates on a corrupt frame.
            if count > 4096 {
                return Err(corrupt("absurd pane count"));
            }
            let mut panes = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let pane = p.u64()?;
                let command = p.string()?;
                let exited = match p.u8()? {
                    0 => None,
                    1 => Some(p.u32()?),
                    _ => return Err(corrupt("bad exited flag")),
                };
                let auto_approve = p.u8()? != 0;
                panes.push(HelloPane {
                    pane,
                    command,
                    exited,
                    auto_approve,
                });
            }
            Frame::Hello {
                panes,
                layout: p.bytes()?,
            }
        }
        65 => Frame::Replay {
            pane: p.u64()?,
            bytes: p.bytes()?,
        },
        66 => Frame::Output {
            pane: p.u64()?,
            bytes: p.bytes()?,
        },
        67 => Frame::Exited {
            pane: p.u64()?,
            code: p.u32()?,
        },
        68 => Frame::PaneOpened {
            pane: p.u64()?,
            command: p.string()?,
        },
        69 => Frame::Shutdown {
            reason: p.string()?,
        },
        70 => Frame::Pong,
        71 => Frame::SpawnFailed { error: p.string()? },
        _ => return Err(corrupt("unknown frame tag")),
    };
    if p.at != p.buf.len() {
        return Err(corrupt("trailing bytes in frame"));
    }
    Ok(Some(frame))
}

fn corrupt(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("protocol: {what}"))
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.at + n > self.buf.len() {
            return Err(corrupt("truncated frame"));
        }
        let slice = &self.buf[self.at..self.at + n];
        self.at += n;
        Ok(slice)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let len = self.u32()?;
        // Deliberately re-checked: a nested length claim is corrupt input
        // independently of the outer frame length read_frame already vetted.
        if len > MAX_FRAME {
            return Err(corrupt("bytes length out of range"));
        }
        Ok(self.take(len as usize)?.to_vec())
    }

    fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| corrupt("invalid utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: Frame) {
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).expect("write");
        let mut r = buf.as_slice();
        let back = read_frame(&mut r).expect("read").expect("some frame");
        assert_eq!(back, frame);
        assert!(r.is_empty(), "reader consumed the whole frame");
    }

    #[test]
    fn every_frame_round_trips() {
        round_trip(Frame::Attach);
        round_trip(Frame::Input {
            pane: 7,
            bytes: b"ls -la\r".to_vec(),
        });
        round_trip(Frame::Resize {
            pane: 7,
            cols: 120,
            rows: 40,
        });
        round_trip(Frame::Spawn {
            command: "claude --dangerously-skip-permissions".into(),
        });
        round_trip(Frame::Close { pane: 3 });
        round_trip(Frame::SetLayout {
            blob: b"v1\nwindow focused=1 (l 1)\nactive 0\n".to_vec(),
        });
        round_trip(Frame::Detach);
        round_trip(Frame::Kill);
        round_trip(Frame::Ping);
        round_trip(Frame::HookBlocked {
            pane: 3,
            tool: "Bash".into(),
            reason: "Bash: rm -rf target/".into(),
        });
        round_trip(Frame::HookBlocked {
            pane: 3,
            tool: String::new(),
            reason: String::new(),
        });
        round_trip(Frame::HookClear {
            pane: 3,
            tool: "Bash".into(),
        });
        round_trip(Frame::HookClear {
            pane: 3,
            tool: String::new(),
        });
        round_trip(Frame::Statusline {
            pane: 3,
            json: r#"{"model":{"display_name":"Opus"},"context_window":{"remaining_percentage":62.5}}"#.into(),
        });
        round_trip(Frame::Statusline {
            pane: 3,
            json: String::new(),
        });
        round_trip(Frame::Statusline {
            pane: u64::MAX,
            json: "non-json 🦀 payloads still round-trip".into(),
        });
        round_trip(Frame::HookDecision { allow: true });
        round_trip(Frame::HookDecision { allow: false });
        round_trip(Frame::SetAutoApprove { pane: 5, on: true });
        round_trip(Frame::SetAutoApprove { pane: 5, on: false });
        round_trip(Frame::Hello {
            panes: vec![
                HelloPane {
                    pane: 1,
                    command: "claude".into(),
                    exited: None,
                    auto_approve: true,
                },
                HelloPane {
                    pane: 2,
                    command: "zsh".into(),
                    exited: Some(130),
                    auto_approve: false,
                },
            ],
            layout: b"blob".to_vec(),
        });
        round_trip(Frame::Hello {
            panes: vec![],
            layout: vec![],
        });
        round_trip(Frame::Replay {
            pane: 1,
            bytes: vec![0, 27, 91, 65, 255],
        });
        round_trip(Frame::Output {
            pane: 9,
            bytes: b"\x1b[1;31mred\x1b[0m".to_vec(),
        });
        round_trip(Frame::Exited { pane: 2, code: 130 });
        round_trip(Frame::PaneOpened {
            pane: 4,
            command: "claude".into(),
        });
        round_trip(Frame::Shutdown {
            reason: "another client attached".into(),
        });
        round_trip(Frame::Pong);
        round_trip(Frame::SpawnFailed {
            error: "spawning command: no such file".into(),
        });
    }

    #[test]
    fn frames_stream_back_to_back() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Frame::Ping).unwrap();
        write_frame(
            &mut buf,
            &Frame::Input {
                pane: 1,
                bytes: b"x".to_vec(),
            },
        )
        .unwrap();
        write_frame(&mut buf, &Frame::Detach).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(read_frame(&mut r).unwrap(), Some(Frame::Ping));
        assert!(matches!(
            read_frame(&mut r).unwrap(),
            Some(Frame::Input { pane: 1, .. })
        ));
        assert_eq!(read_frame(&mut r).unwrap(), Some(Frame::Detach));
        assert_eq!(read_frame(&mut r).unwrap(), None, "clean EOF");
    }

    #[test]
    fn corrupt_input_errors_instead_of_panicking() {
        // Truncated length prefix mid-stream is clean EOF at frame start
        // only; anything after must error.
        let mut r: &[u8] = &[5, 0, 0, 0, 2, 1]; // claims 5 bytes, has 2
        assert!(read_frame(&mut r).is_err());

        // Oversized length.
        let huge = (MAX_FRAME + 1).to_le_bytes();
        let mut r: &[u8] = &huge;
        assert!(read_frame(&mut r).is_err());

        // Zero length.
        let mut r: &[u8] = &[0, 0, 0, 0];
        assert!(read_frame(&mut r).is_err());

        // Unknown tag.
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.push(200);
        let mut r = buf.as_slice();
        assert!(read_frame(&mut r).is_err());

        // Trailing garbage inside a frame.
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.push(1); // Attach carries no payload
        buf.push(99);
        let mut r = buf.as_slice();
        assert!(read_frame(&mut r).is_err());

        // Invalid utf-8 in a string field.
        let mut payload = Vec::new();
        put_bytes(&mut payload, &[0xff, 0xfe]);
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
        buf.push(4); // Spawn
        buf.extend_from_slice(&payload);
        let mut r = buf.as_slice();
        assert!(read_frame(&mut r).is_err());

        // Invalid utf-8 in a Statusline json field.
        let mut payload = Vec::new();
        put_u64(&mut payload, 3);
        put_bytes(&mut payload, &[0xff, 0xfe]);
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
        buf.push(14); // Statusline
        buf.extend_from_slice(&payload);
        let mut r = buf.as_slice();
        assert!(read_frame(&mut r).is_err());

        // A Statusline frame truncated inside its json length.
        let mut payload = Vec::new();
        put_u64(&mut payload, 3);
        payload.extend_from_slice(&8u32.to_le_bytes()); // claims 8 bytes
        payload.extend_from_slice(b"abc"); // has 3
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
        buf.push(14);
        buf.extend_from_slice(&payload);
        let mut r = buf.as_slice();
        assert!(read_frame(&mut r).is_err());
    }
}
