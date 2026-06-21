//! Per-pane append-only on-disk history of lines evicted from the terminal's
//! in-RAM scrollback ring. Backs full-session copy and page-on-scroll. Lines are
//! stored as ANSI-SGR text (one `\n`-terminated line each); an in-memory
//! byte-offset index lets any line range be seeked without scanning the file.

use breeze_core::transcript::strip_sgr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Flush eagerly past this buffered size so a burst can't grow `pending`
/// unbounded between reads.
const FLUSH_THRESHOLD: usize = 64 * 1024;

struct Inner {
    file: Option<File>,
    pending: Vec<u8>,
    /// Byte offset where each stored line begins (in the logical flushed+pending
    /// file). `line_offsets.len()` == number of lines.
    line_offsets: Vec<u64>,
    total_bytes: u64,
}

pub struct ScrollbackTranscript {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl ScrollbackTranscript {
    /// Open a transcript for `pane_id` under `~/.breeze/scrollback`. The filename
    /// includes this process's PID so a prior run's file can't bleed in.
    pub fn new(pane_id: i64) -> std::io::Result<ScrollbackTranscript> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        ScrollbackTranscript::new_in(home.join(".breeze/scrollback"), pane_id)
    }

    /// Open a transcript in an explicit directory (used by tests).
    pub fn new_in(dir: impl AsRef<Path>, pane_id: i64) -> std::io::Result<ScrollbackTranscript> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let pid = std::process::id();
        let path = dir.join(format!("pane-{pane_id}-{pid}.log"));
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(&path)?;
        Ok(ScrollbackTranscript {
            path,
            inner: Mutex::new(Inner {
                file: Some(file),
                pending: Vec::new(),
                line_offsets: Vec::new(),
                total_bytes: 0,
            }),
        })
    }

    /// Append one evicted line (ANSI-SGR text). A trailing newline is added.
    pub fn append_line(&self, ansi_line: &str) {
        let mut inner = self.inner.lock().unwrap();
        let start = inner.total_bytes;
        inner.line_offsets.push(start);
        inner.pending.extend_from_slice(ansi_line.as_bytes());
        inner.pending.push(b'\n');
        inner.total_bytes += ansi_line.len() as u64 + 1;
        if inner.pending.len() >= FLUSH_THRESHOLD {
            flush_locked(&mut inner);
        }
    }

    /// Lines spilled so far.
    pub fn line_count(&self) -> usize {
        self.inner.lock().unwrap().line_offsets.len()
    }

    /// The full transcript as plain text (SGR stripped) — for clipboard copy.
    pub fn read_all_text(&self) -> String {
        let mut inner = self.inner.lock().unwrap();
        flush_locked(&mut inner);
        drop(inner);
        let mut data = String::new();
        if let Ok(mut f) = File::open(&self.path) {
            let _ = f.read_to_string(&mut data);
        }
        strip_sgr(&data)
    }

    /// The ANSI-SGR lines for `range` (color preserved), seeked via the offset
    /// index. Returns fewer than requested if the range exceeds what's stored.
    pub fn read_lines(&self, range: Range<usize>) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap();
        flush_locked(&mut inner);
        let n = inner.line_offsets.len();
        let lo = range.start.max(0);
        let hi = range.end.min(n);
        if lo >= hi {
            return Vec::new();
        }
        let start_byte = inner.line_offsets[lo];
        let end_byte = if hi < n { inner.line_offsets[hi] } else { inner.total_bytes };
        drop(inner);

        let Ok(mut f) = File::open(&self.path) else { return Vec::new() };
        if f.seek(SeekFrom::Start(start_byte)).is_err() {
            return Vec::new();
        }
        let mut buf = vec![0u8; (end_byte - start_byte) as usize];
        if f.read_exact(&mut buf).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        if lines.last().map(|s| s.is_empty()).unwrap_or(false) {
            lines.pop(); // trailing newline
        }
        lines
    }

    /// Flush, close, and delete the file. The transcript only needs to outlive
    /// the pane.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        flush_locked(&mut inner);
        inner.file = None;
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ScrollbackTranscript {
    fn drop(&mut self) {
        self.close();
    }
}

fn flush_locked(inner: &mut Inner) {
    if inner.pending.is_empty() {
        return;
    }
    if let Some(f) = inner.file.as_mut() {
        if f.write_all(&inner.pending).is_ok() {
            inner.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("breeze-sb-test-{}-{}", std::process::id(), rand_suffix()))
    }
    fn rand_suffix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[test]
    fn append_count_and_read_lines() {
        let dir = tmp();
        let t = ScrollbackTranscript::new_in(&dir, 1).unwrap();
        t.append_line("\u{1b}[31mline one\u{1b}[0m");
        t.append_line("line two");
        t.append_line("line three");
        assert_eq!(t.line_count(), 3);

        let mid = t.read_lines(0..2);
        assert_eq!(mid, vec!["\u{1b}[31mline one\u{1b}[0m".to_string(), "line two".to_string()]);

        // Out-of-range upper bound is clamped.
        assert_eq!(t.read_lines(2..99), vec!["line three".to_string()]);
    }

    #[test]
    fn read_all_text_strips_sgr() {
        let dir = tmp();
        let t = ScrollbackTranscript::new_in(&dir, 2).unwrap();
        t.append_line("\u{1b}[1;32mgreen\u{1b}[0m bold");
        t.append_line("plain");
        assert_eq!(t.read_all_text(), "green bold\nplain\n");
    }

    #[test]
    fn offsets_survive_a_large_flush() {
        let dir = tmp();
        let t = ScrollbackTranscript::new_in(&dir, 3).unwrap();
        // Push enough bytes to cross the 64 KiB flush threshold mid-stream.
        let big = "x".repeat(1000);
        for _ in 0..100 {
            t.append_line(&big); // ~100 KB total → flushes
        }
        t.append_line("sentinel-after-flush");
        assert_eq!(t.line_count(), 101);
        // The post-flush line must still be seekable by offset.
        assert_eq!(t.read_lines(100..101), vec!["sentinel-after-flush".to_string()]);
        // A line from before the flush too.
        assert_eq!(t.read_lines(0..1), vec![big]);
    }

    #[test]
    fn close_removes_the_file() {
        let dir = tmp();
        let t = ScrollbackTranscript::new_in(&dir, 4).unwrap();
        t.append_line("data");
        let path = t.path.clone();
        assert!(path.exists());
        t.close();
        assert!(!path.exists());
    }
}
