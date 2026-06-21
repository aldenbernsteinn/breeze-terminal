//! A running terminal: a PTY-attached child whose output is continuously parsed
//! into a [`Vt`] grid on a reader thread. This is the headless core a renderer
//! draws and a session copies from.

use crate::pty::Pty;
use breeze_vt::Vt;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Child-output activity for a pane: when output last arrived, and whether it is
/// unacknowledged. Source of truth for the "settled" badge (uses `has_unseen`).
#[derive(Default)]
pub struct Activity {
    last_output: Option<Instant>,
    has_unseen: bool,
}

impl Activity {
    /// When child output last arrived, if ever.
    pub fn last_output(&self) -> Option<Instant> {
        self.last_output
    }
}

/// Activity behind a condvar so a waiter can park until output goes quiet and be
/// woken the instant new output arrives — event-driven, no polling.
pub type ActivitySignal = Arc<(Mutex<Activity>, Condvar)>;

pub struct Terminal {
    pty: Pty,
    vt: Arc<Mutex<Vt>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    stop: Arc<AtomicBool>,
    /// Set by the reader thread when the child's output stream ends (the shell
    /// exited). Lets the UI close the pane that owned it.
    exited: Arc<AtomicBool>,
    /// Child-output activity, updated by the reader thread.
    activity: ActivitySignal,
    pump: Option<JoinHandle<()>>,
}

impl Terminal {
    /// Spawn `program` in a PTY of `cols`×`rows` and start parsing its output.
    pub fn spawn(
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> std::io::Result<Terminal> {
        Terminal::spawn_with_output(program, args, env, cwd, cols, rows, Vt::DEFAULT_HISTORY, None)
    }

    /// Like [`Terminal::spawn`], but `on_output` is invoked after each chunk of
    /// child output is parsed (used to feed the session-state detector), and the
    /// scrollback depth (`history`) is explicit.
    pub fn spawn_with_output(
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        history: usize,
        on_output: Option<Box<dyn FnMut() + Send>>,
    ) -> std::io::Result<Terminal> {
        let pty = Pty::spawn(program, args, env, cwd, cols, rows)?;
        let writer = Arc::new(Mutex::new(pty.writer()?));
        let mut reader = pty.reader()?;

        let vt = Arc::new(Mutex::new(Vt::with_history(cols as usize, rows as usize, history)));
        let stop = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let activity: ActivitySignal = Arc::new((Mutex::new(Activity::default()), Condvar::new()));

        let pump_vt = Arc::clone(&vt);
        let pump_stop = Arc::clone(&stop);
        let pump_exited = Arc::clone(&exited);
        let pump_activity = Arc::clone(&activity);
        let mut on_output = on_output;
        let pump = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                if pump_stop.load(Ordering::Relaxed) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF: child exited
                    Ok(n) => {
                        if let Ok(mut vt) = pump_vt.lock() {
                            vt.feed(&buf[..n]);
                        }
                        // Mark activity and wake any waiter parked on quiescence.
                        let (m, cv) = &*pump_activity;
                        if let Ok(mut a) = m.lock() {
                            a.last_output = Some(Instant::now());
                            a.has_unseen = true;
                        }
                        cv.notify_all();
                        if let Some(cb) = on_output.as_mut() {
                            cb();
                        }
                    }
                    Err(_) => break,
                }
            }
            // Reader fell out of the loop: the stream ended. If it wasn't a
            // deliberate stop (Drop), the child exited on its own.
            if !pump_stop.load(Ordering::Relaxed) {
                pump_exited.store(true, Ordering::Relaxed);
                pump_activity.1.notify_all(); // wake a parked driver so it can stop
                if let Some(cb) = on_output.as_mut() {
                    cb(); // wake the UI so it polls and closes the pane
                }
            }
        });

        Ok(Terminal { pty, vt, writer, stop, exited, activity, pump: Some(pump) })
    }

    /// Write input (keystrokes) to the child.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()
    }

    /// Print text directly into the grid (display-only — NOT sent to the child),
    /// for status/notice lines.
    pub fn display(&self, text: &str) {
        if let Ok(mut vt) = self.vt.lock() {
            vt.feed(text.as_bytes());
        }
    }

    /// Whether child output is unacknowledged and has been quiet for `quiet`
    /// (used by the "settled" badge).
    pub fn is_settled(&self, quiet: Duration) -> bool {
        let (m, _) = &*self.activity;
        let a = m.lock().unwrap();
        a.has_unseen && a.last_output.map(|t| t.elapsed() >= quiet).unwrap_or(false)
    }

    /// Acknowledge current activity (pane focused/visible), clearing the badge
    /// until new output arrives.
    pub fn ack(&self) {
        let (m, _) = &*self.activity;
        m.lock().unwrap().has_unseen = false;
    }

    /// Resize both the PTY and the grid.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.pty.resize(cols, rows)?;
        if let Ok(mut vt) = self.vt.lock() {
            vt.resize(cols as usize, rows as usize);
        }
        Ok(())
    }

    /// Scroll the grid viewport through scrollback (+ older, − toward bottom).
    pub fn scroll(&self, delta: i32) {
        if let Ok(mut vt) = self.vt.lock() {
            vt.scroll(delta);
        }
    }

    /// The child's process id, if known.
    pub fn child_pid(&self) -> Option<u32> {
        self.pty.child_pid()
    }

    /// Whether the child's output stream has ended (the shell exited).
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// Scrollback viewport position: `(offset_from_bottom, total_lines)`.
    pub fn scroll_position(&self) -> (usize, usize) {
        self.vt
            .lock()
            .map(|vt| (vt.scroll_offset(), vt.total_lines()))
            .unwrap_or((0, 0))
    }

    /// Read a row of the current grid.
    pub fn row_text(&self, row: usize) -> String {
        self.vt.lock().map(|vt| vt.row_text(row)).unwrap_or_default()
    }

    /// The full visible screen as text.
    pub fn screen_text(&self) -> String {
        self.vt.lock().map(|vt| vt.screen_text()).unwrap_or_default()
    }

    /// The full visible screen as colored cells (what a color renderer draws).
    pub fn screen_cells(&self) -> Vec<Vec<breeze_vt::Cell>> {
        self.vt.lock().map(|vt| vt.screen_cells()).unwrap_or_default()
    }

    /// Cursor position `(row, col)` in visible-screen coordinates.
    pub fn cursor_pos(&self) -> (usize, usize) {
        self.vt.lock().map(|vt| vt.cursor_pos()).unwrap_or((0, 0))
    }

    /// The title last set by the program (OSC 2), if any.
    pub fn title(&self) -> Option<String> {
        self.vt.lock().ok().and_then(|vt| vt.title())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.pty.kill(); // EOF wakes the reader
        if let Some(p) = self.pump.take() {
            let _ = p.join();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn command_output_lands_in_the_grid() {
        let term = Terminal::spawn(
            "/bin/sh",
            &["-c", "printf 'hello-grid'"],
            &[("TERM", "xterm-256color")],
            None,
            40,
            10,
        )
        .expect("spawn terminal");

        // Poll the grid until the pump thread has parsed the output.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut row = String::new();
        while Instant::now() < deadline {
            row = term.row_text(0);
            if row.contains("hello-grid") {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(row.contains("hello-grid"), "grid row 0 was: {row:?}");
    }

    #[test]
    fn exit_is_observed_when_the_child_finishes() {
        // A shell that exits immediately should flip has_exited().
        let term = Terminal::spawn("/bin/sh", &["-c", "exit 0"], &[("TERM", "xterm-256color")], None, 40, 10)
            .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !term.has_exited() {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(term.has_exited(), "child exit should be observed");
    }

    #[test]
    fn echoes_written_input_into_the_grid() {
        // `cat` echoes its stdin back through the PTY.
        let mut term = Terminal::spawn("/bin/cat", &[], &[("TERM", "xterm-256color")], None, 40, 10)
            .expect("spawn cat");
        term.write(b"typed-text\r").expect("write");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while Instant::now() < deadline {
            if term.screen_text().contains("typed-text") {
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(seen, "echoed input not found; screen: {:?}", term.screen_text());
    }
}
