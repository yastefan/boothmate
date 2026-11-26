//! Minimal logging to the terminal and `logs/boothmate.log` in the data folder.

use serde::Serialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);
static VERBOSE: AtomicBool = AtomicBool::new(false);
/// Recent info and warning lines for the web UI.
static RECENT: Mutex<(u64, VecDeque<Line>)> = Mutex::new((0, VecDeque::new()));
const RECENT_MAX: usize = 300;

#[derive(Debug, Clone, Serialize)]
pub struct Line {
    /// Increasing number, so the web UI can ask for newer lines only.
    pub seq: u64,
    /// Unix time in ms.
    pub t: u64,
    pub level: &'static str,
    pub msg: String,
}

/// Lines after `seq`, oldest first.
pub fn recent(after: u64) -> Vec<Line> {
    let r = RECENT.lock().unwrap();
    r.1.iter().filter(|l| l.seq > after).cloned().collect()
}

fn remember(level: &'static str, msg: &str) {
    let mut r = RECENT.lock().unwrap();
    r.0 += 1;
    let line = Line {
        seq: r.0,
        t: crate::state::now_ms(),
        level,
        msg: msg.to_string(),
    };
    r.1.push_back(line);
    while r.1.len() > RECENT_MAX {
        r.1.pop_front();
    }
}

pub fn init(dir: &Path, verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
    let logs = dir.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let path = logs.join("boothmate.log");
    // Keep one previous log; start fresh each run so the stick does not fill up.
    let _ = std::fs::rename(&path, logs.join("boothmate.old.log"));
    *FILE.lock().unwrap() = File::create(path).ok();
}

fn write(level: &str, msg: &str) {
    let secs = crate::state::now_ms() / 1000;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let line = format!("{h:02}:{m:02}:{s:02} UTC {level} {msg}");
    if level != "DEBUG" || VERBOSE.load(Ordering::Relaxed) {
        eprintln!("{line}");
    }
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

pub fn info(msg: &str) {
    write("INFO ", msg);
    remember("info", msg);
}

pub fn warn(msg: &str) {
    write("WARN ", msg);
    remember("warn", msg);
}

pub fn debug(msg: &str) {
    if VERBOSE.load(Ordering::Relaxed) {
        write("DEBUG", msg);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn recent_lines_after_seq() {
        super::info("first test line");
        let all = super::recent(0);
        let seq = all.last().unwrap().seq;
        super::warn("second test line");
        let newer = super::recent(seq);
        assert!(newer
            .iter()
            .any(|l| l.msg == "second test line" && l.level == "warn"));
        assert!(newer.iter().all(|l| l.seq > seq));
    }
}
