//! The wire protocol between a roster client and a session server.
//!
//! Frames are length-prefixed binary: a little-endian `u32` payload length,
//! a `u8` tag, then the payload. Encoding is hand-rolled — the message set
//! is small and stable, and the crate stays dependency-free. The transport
//! is any `Read`/`Write` pair: a unix socket locally, an ssh subprocess's
//! stdio remotely.

use std::io::{self, Read, Write};

/// Frames larger than this are rejected as corrupt rather than allocated.
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
}

/// Every message either side can send. Tags 1–63 flow client → server,
/// 64+ server → client.
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
        64 => {
            let count = p.u64()?;
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
                panes.push(HelloPane {
                    pane,
                    command,
                    exited,
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
        round_trip(Frame::Hello {
            panes: vec![
                HelloPane {
                    pane: 1,
                    command: "claude".into(),
                    exited: None,
                },
                HelloPane {
                    pane: 2,
                    command: "zsh".into(),
                    exited: Some(130),
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
            command: "codex".into(),
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
    }
}
