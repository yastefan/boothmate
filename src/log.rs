//! Minimal logging to the terminal and `logs/boothmate.log` in the data folder.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);
static VERBOSE: AtomicBool = AtomicBool::new(false);

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
}

pub fn warn(msg: &str) {
    write("WARN ", msg);
}

pub fn debug(msg: &str) {
    if VERBOSE.load(Ordering::Relaxed) {
        write("DEBUG", msg);
    }
}
