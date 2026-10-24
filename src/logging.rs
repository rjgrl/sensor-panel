//! Logging to %APPDATA%\DS916Tray\ds916_tray_log.txt.
//!
//! Three levels, configurable in Settings -> General:
//!   off     - nothing written at all
//!   normal  - INFO and above (startup, theme loads, connection status, settings changes, errors)
//!   verbose - DEBUG and above (per-read diagnostics, for actively diagnosing a problem)
//!
//! The file is rotated at ~1MB to ds916_tray_log.txt.1 (one backup kept), so
//! total on-disk log size stays bounded at roughly 2MB.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;

use crate::paths;

const MAX_BYTES: u64 = 1_000_000;

// 0 = off, 1 = normal, 2 = verbose
static LEVEL: AtomicU8 = AtomicU8::new(1);
static FILE: Mutex<Option<(File, u64)>> = Mutex::new(None);
static BOOT: AtomicBool = AtomicBool::new(false);

pub fn use_boot_log() {
    BOOT.store(true, Ordering::Relaxed);
}

fn log_path() -> std::path::PathBuf {
    if BOOT.load(Ordering::Relaxed) {
        paths::config_dir().join("ds916_boot_log.txt")
    } else {
        paths::log_file()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lvl {
    Debug,
    Info,
    Warning,
    Error,
}

pub fn level_from_name(name: &str) -> u8 {
    match name.trim().to_ascii_lowercase().as_str() {
        "off" => 0,
        "verbose" => 2,
        _ => 1,
    }
}

pub fn set_level(name: &str) {
    LEVEL.store(level_from_name(name), Ordering::Relaxed);
}

pub fn verbose() -> bool {
    LEVEL.load(Ordering::Relaxed) >= 2
}

fn open_file() -> Option<(File, u64)> {
    let path = log_path();
    let f = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    Some((f, size))
}

pub fn write(lvl: Lvl, msg: &str) {
    let cur = LEVEL.load(Ordering::Relaxed);
    let needed = if lvl == Lvl::Debug { 2 } else { 1 };
    if cur < needed {
        return;
    }
    let tag = match lvl {
        Lvl::Debug => "DEBUG",
        Lvl::Info => "INFO",
        Lvl::Warning => "WARNING",
        Lvl::Error => "ERROR",
    };
    let line = format!(
        "{} [{}] {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        tag,
        msg
    );
    let mut guard = match FILE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if guard.is_none() {
        *guard = open_file();
    }
    // Rotate when the file gets too big (another process may also have
    // written to it, so re-check the real size first).
    let too_big = match guard.as_ref() {
        Some((f, sz)) => *sz >= MAX_BYTES && f.metadata().map(|m| m.len()).unwrap_or(*sz) >= MAX_BYTES,
        None => false,
    };
    if too_big {
        *guard = None; // close before renaming
        let path = log_path();
        let mut bak = path.clone().into_os_string();
        bak.push(".1");
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::rename(&path, &bak);
        *guard = open_file();
    }
    if let Some((f, sz)) = guard.as_mut() {
        if f.write_all(line.as_bytes()).is_ok() {
            *sz += line.len() as u64;
        }
    }
}

#[macro_export]
macro_rules! log_info { ($($t:tt)*) => { $crate::logging::write($crate::logging::Lvl::Info, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_warn { ($($t:tt)*) => { $crate::logging::write($crate::logging::Lvl::Warning, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_error { ($($t:tt)*) => { $crate::logging::write($crate::logging::Lvl::Error, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_debug { ($($t:tt)*) => { if $crate::logging::verbose() { $crate::logging::write($crate::logging::Lvl::Debug, &format!($($t)*)) } } }
