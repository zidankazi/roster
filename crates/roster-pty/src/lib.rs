//! PTY allocation and agent child-process lifecycle.
//!
//! [`Pty::spawn`] runs a command inside a pseudo-terminal so the agent CLI
//! believes it has a real TTY. The command string is run through `sh -c`,
//! which handles quoting and, for a single command, execs it directly —
//! signals reach the agent, not an intermediate shell. Reading is blocking;
//! the binary owns the thread that pumps bytes out.

use std::fmt;
use std::io::{self, Read, Write};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// The terminal type advertised to spawned agents.
const TERM: &str = "xterm-256color";

/// Why a PTY operation failed.
#[derive(Debug)]
pub struct PtyError {
    context: &'static str,
    message: String,
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.message)
    }
}

impl std::error::Error for PtyError {}

impl PtyError {
    fn new(context: &'static str, error: impl fmt::Display) -> Self {
        PtyError {
            context,
            message: error.to_string(),
        }
    }
}

/// How a child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    /// The process exit code.
    pub code: u32,
    /// Whether the process exited cleanly with code zero.
    pub success: bool,
}

/// A live pseudo-terminal with a child process attached.
///
/// Dropping the `Pty` kills and reaps the child — closing a pane must not
/// leak agents.
pub struct Pty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
}

impl Pty {
    /// Spawn `command` (a shell command line) in a fresh PTY of
    /// `cols` × `rows` cells, with `TERM=xterm-256color`.
    pub fn spawn(command: &str, cols: u16, rows: u16) -> Result<Pty, PtyError> {
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::new("opening pty", e))?;

        let mut builder = CommandBuilder::new("/bin/sh");
        builder.arg("-c");
        builder.arg(command);
        builder.env("TERM", TERM);

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| PtyError::new("spawning command", e))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::new("taking pty writer", e))?;

        Ok(Pty {
            master: pair.master,
            child,
            writer,
        })
    }

    /// A blocking reader over the child's output. Each call returns an
    /// independent reader; the binary typically hands one to a pump thread.
    pub fn reader(&self) -> Result<Box<dyn Read + Send>, PtyError> {
        self.master
            .try_clone_reader()
            .map_err(|e| PtyError::new("cloning pty reader", e))
    }

    /// Write bytes to the child's input.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Propagate a pane resize to the child (delivers `SIGWINCH`).
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::new("resizing pty", e))
    }

    /// The child's OS process id, when still known.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Whether the child has exited, without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?.map(convert_status))
    }

    /// Block until the child exits.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        Ok(convert_status(self.child.wait()?))
    }

    /// Kill the child. Reaping still happens via [`Pty::wait`] or drop.
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn convert_status(status: portable_pty::ExitStatus) -> ExitStatus {
    ExitStatus {
        code: status.exit_code(),
        success: status.success(),
    }
}
