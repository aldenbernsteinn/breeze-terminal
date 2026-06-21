//! Pseudo-terminal: spawn a child (a shell, optionally auto-resuming an agent)
//! attached to a PTY, and read/write/resize it.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Pty {
    /// Spawn `program` with `args` attached to a new PTY of the given size.
    pub fn spawn(
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> std::io::Result<Pty> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(to_io)?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }

        let child = pair.slave.spawn_command(cmd).map_err(to_io)?;
        drop(pair.slave); // the master keeps the PTY open
        Ok(Pty { master: pair.master, child })
    }

    /// A clone of the output stream (terminal → app).
    pub fn reader(&self) -> std::io::Result<Box<dyn Read + Send>> {
        self.master.try_clone_reader().map_err(to_io)
    }

    /// The input stream (app → terminal). Takes ownership; call once.
    pub fn writer(&self) -> std::io::Result<Box<dyn Write + Send>> {
        self.master.take_writer().map_err(to_io)
    }

    /// Resize the PTY window.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(to_io)
    }

    /// The child's process id, if still known.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Block until the child exits; returns true on a success exit code.
    pub fn wait(&mut self) -> std::io::Result<bool> {
        let status = self.child.wait().map_err(to_io)?;
        Ok(status.success())
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill().map_err(to_io)
    }
}

fn to_io(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

/// Launch the user's login shell in a PTY. A standard terminal launch: `$SHELL`
/// (or `/bin/zsh`) as a login shell, with `TERM=xterm-256color` and
/// `TERM_PROGRAM=Breeze`.
pub fn spawn_shell(directory: Option<&str>, cols: u16, rows: u16) -> std::io::Result<Pty> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let env = [("TERM", "xterm-256color"), ("TERM_PROGRAM", "Breeze")];
    Pty::spawn(&shell, &["-l"], &env, directory, cols, rows)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn reads_real_shell_output() {
        let mut pty = Pty::spawn(
            "/bin/sh",
            &["-c", "printf hello-pty"],
            &[("TERM", "xterm-256color")],
            None,
            80,
            24,
        )
        .expect("spawn pty");

        let mut reader = pty.reader().expect("reader");
        let mut out = String::new();
        let mut buf = [0u8; 1024];
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !out.contains("hello-pty") {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
        assert!(out.contains("hello-pty"), "pty output was: {out:?}");
        let _ = pty.wait();
    }

    #[test]
    fn spawn_shell_has_live_child_and_resizes() {
        let mut pty = spawn_shell(None, 80, 24).expect("spawn shell");
        assert!(pty.child_pid().is_some(), "shell should have a pid");
        pty.resize(100, 30).expect("resize");
        // Don't wait for a clean exit: an interactive login shell can block on a
        // full output buffer if nobody drains it. Just terminate it.
        let _ = pty.kill();
        let _ = pty.wait();
    }
}
