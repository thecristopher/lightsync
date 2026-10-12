use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1024 * 1024;

/// %LOCALAPPDATA%\LightSync, where the app keeps its log.
pub fn folder() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("LightSync")
}

pub fn path() -> PathBuf {
    folder().join("lightsync.log")
}

/// %APPDATA%\LightSync\settings.json, which roams with the person's profile.
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("LightSync").join("settings.json")
}

pub fn write(message: &str) {
    if crate::console::attached() {
        eprintln!("{message}");
    }
    record(message);
}

/// Just the log file, for messages that already reach the person some other way.
pub fn record(message: &str) {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let line = format!("[{seconds}] {message}\n");
    let _ = std::fs::create_dir_all(folder());
    let too_big = std::fs::metadata(path()).is_ok_and(|m| m.len() > MAX_BYTES);
    let file = std::fs::OpenOptions::new().create(true).append(!too_big).write(true).truncate(too_big).open(path());
    if let Ok(mut file) = file {
        let _ = file.write_all(line.as_bytes());
    }
}

macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
pub(crate) use log;
